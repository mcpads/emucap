//! Reconnecting adapter-to-MCP session loop used by bridge processes.
//!
//! The backend connection (GDB/WebSocket) is owned by the caller and therefore survives an MCP
//! socket EOF. Only the front-side TCP session is recreated, preserving emulator state across MCP
//! restarts and consecutive-timeout connection drops.

use std::io::{self, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

use super::protocol::{read_ndjson_frame, ProtocolError, Request, Response};
use super::runtime::{capture_process, process_state, ProcessIdentity, ProcessState};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const WORKING_INTERVAL: Duration = Duration::from_secs(1);
const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MIN_RETRY: Duration = Duration::from_millis(50);
const MAX_RETRY: Duration = Duration::from_secs(2);

const EMULATOR_PID_ENV: &str = "EMUCAP_EMULATOR_PID";
const EMULATOR_START_IDENTITY_ENV: &str = "EMUCAP_EMULATOR_START_IDENTITY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeDirective {
    Continue,
    Terminate,
}

pub struct BridgeReply {
    pub response: Response,
    pub directive: BridgeDirective,
}

impl BridgeReply {
    pub fn continue_with(response: Response) -> Self {
        Self {
            response,
            directive: BridgeDirective::Continue,
        }
    }

    pub fn terminate_with(response: Response) -> Self {
        Self {
            response,
            directive: BridgeDirective::Terminate,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessDependency {
    identity: ProcessIdentity,
}

impl ProcessDependency {
    pub fn from_process_env() -> io::Result<Option<Self>> {
        let Some(raw_pid) = std::env::var_os(EMULATOR_PID_ENV) else {
            return Ok(None);
        };
        let raw_pid = raw_pid.to_string_lossy();
        let pid = raw_pid.parse::<u32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{EMULATOR_PID_ENV} must be a decimal process id"),
            )
        })?;
        if pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{EMULATOR_PID_ENV} must be greater than zero"),
            ));
        }
        let identity = match std::env::var(EMULATOR_START_IDENTITY_ENV) {
            Ok(start_identity) if !start_identity.trim().is_empty() => ProcessIdentity {
                pid,
                start_identity: Some(start_identity),
            },
            _ => capture_process(pid),
        };
        if identity.start_identity.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot capture process start identity for {EMULATOR_PID_ENV}={pid}"),
            ));
        }
        Ok(Some(Self { identity }))
    }

    pub fn terminal_reason(&self) -> Option<String> {
        match process_state(&self.identity) {
            ProcessState::Alive => None,
            ProcessState::Exited => Some(format!(
                "emulator process {} exited or its PID was reused",
                self.identity.pid
            )),
            ProcessState::Unknown => Some(format!(
                "emulator process {} identity is no longer verifiable",
                self.identity.pid
            )),
        }
    }
}

pub fn serve_reconnecting<F>(port: u16, label: &str, handle: F) -> io::Result<()>
where
    F: FnMut(Request) -> Response + Send,
{
    serve_reconnecting_inner(port, label, handle, None)
}

pub fn serve_reconnecting_controlled<F, P>(
    port: u16,
    label: &str,
    handle: F,
    terminal_probe: P,
) -> io::Result<()>
where
    F: FnMut(Request) -> BridgeReply + Send,
    P: FnMut() -> Option<String>,
{
    serve_reconnecting_controlled_inner(port, label, handle, terminal_probe, None)
}

fn serve_reconnecting_inner<F>(
    port: u16,
    label: &str,
    mut handle: F,
    max_sessions: Option<usize>,
) -> io::Result<()>
where
    F: FnMut(Request) -> Response + Send,
{
    serve_reconnecting_controlled_inner(
        port,
        label,
        move |request| BridgeReply::continue_with(handle(request)),
        || None,
        max_sessions,
    )
}

fn serve_reconnecting_controlled_inner<F, P>(
    port: u16,
    label: &str,
    mut handle: F,
    mut terminal_probe: P,
    max_sessions: Option<usize>,
) -> io::Result<()>
where
    F: FnMut(Request) -> BridgeReply + Send,
    P: FnMut() -> Option<String>,
{
    let endpoint = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut delay = MIN_RETRY;
    let mut sessions = 0usize;
    loop {
        if let Some(reason) = terminal_probe() {
            eprintln!("[{label}] dependency ended ({reason}); exiting bridge");
            return Ok(());
        }
        let stream = match TcpStream::connect_timeout(&endpoint, CONNECT_TIMEOUT) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("[{label}] emucap unavailable ({error}); retrying in {delay:?}");
                std::thread::sleep(delay);
                delay = (delay * 2).min(MAX_RETRY);
                continue;
            }
        };
        delay = MIN_RETRY;
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        stream.set_read_timeout(Some(LIFECYCLE_POLL_INTERVAL))?;
        eprintln!("[{label}] emucap connected");
        let end = serve_one_controlled(stream, &mut handle, &mut terminal_probe, WORKING_INTERVAL);
        sessions += 1;
        if let Ok(SessionEnd::DependencyTerminal(reason)) = &end {
            eprintln!("[{label}] dependency ended ({reason}); exiting bridge");
            return Ok(());
        }
        if max_sessions.is_some_and(|limit| sessions >= limit) {
            return end.map(|_| ());
        }
        match end {
            Ok(SessionEnd::FrontDisconnected) => {
                eprintln!("[{label}] emucap disconnected; reconnecting")
            }
            Ok(SessionEnd::DependencyTerminal(_)) => unreachable!(),
            Err(error) => eprintln!("[{label}] emucap session failed ({error}); reconnecting"),
        }
        std::thread::sleep(delay);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SessionEnd {
    FrontDisconnected,
    DependencyTerminal(String),
}

#[cfg(test)]
fn serve_one_with_interval<F>(
    stream: TcpStream,
    handle: &mut F,
    working_interval: Duration,
) -> io::Result<()>
where
    F: FnMut(Request) -> Response + Send,
{
    let mut controlled = |request| BridgeReply::continue_with(handle(request));
    let mut terminal_probe = || None;
    serve_one_controlled(
        stream,
        &mut controlled,
        &mut terminal_probe,
        working_interval,
    )
    .map(|_| ())
}

fn serve_one_controlled<F, P>(
    stream: TcpStream,
    handle: &mut F,
    terminal_probe: &mut P,
    working_interval: Duration,
) -> io::Result<SessionEnd>
where
    F: FnMut(Request) -> BridgeReply + Send,
    P: FnMut() -> Option<String>,
{
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut pending = Vec::new();
    loop {
        if let Some(reason) = terminal_probe() {
            return Ok(SessionEnd::DependencyTerminal(reason));
        }
        let line = match read_ndjson_frame(&mut reader, &mut pending) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(SessionEnd::FrontDisconnected),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => {
                let completion =
                    serve_request_controlled(&mut writer, handle, request, working_interval)?;
                if completion.directive == BridgeDirective::Terminate {
                    return Ok(SessionEnd::DependencyTerminal(
                        "backend reported a terminal transport state".into(),
                    ));
                }
                if let Some(error) = completion.write_error {
                    return Err(error);
                }
            }
            Err(error) => {
                let response = Response {
                    id: 0,
                    ok: false,
                    result: None,
                    error: Some(ProtocolError {
                        kind: "protocol_error".into(),
                        message: error.to_string(),
                    }),
                };
                write_response(&mut writer, &response)?;
            }
        }
    }
}

struct RequestCompletion {
    directive: BridgeDirective,
    write_error: Option<io::Error>,
}

/// Run a synchronous backend request without making the front-side link look idle. The bridge
/// handler remains the sole owner of its GDB/WebSocket connection, while this thread emits bounded
/// `working` frames until the terminal response is ready. If the MCP side disconnects, the handler
/// is still joined so request-scoped backend cleanup finishes before a replacement session starts.
fn serve_request_controlled<F>(
    writer: &mut TcpStream,
    handle: &mut F,
    request: Request,
    working_interval: Duration,
) -> io::Result<RequestCompletion>
where
    F: FnMut(Request) -> BridgeReply + Send,
{
    let id = request.id;
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel(1);
        scope.spawn(move || {
            let _ = tx.send(handle(request));
        });

        let mut write_error = None;
        loop {
            match rx.recv_timeout(working_interval) {
                Ok(reply) => {
                    if write_error.is_none() {
                        write_error = write_response(writer, &reply.response).err();
                    }
                    return Ok(RequestCompletion {
                        directive: reply.directive,
                        write_error,
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if write_error.is_none() {
                        let working = Response {
                            id,
                            ok: true,
                            result: Some(serde_json::json!({ "status": "working" })),
                            error: None,
                        };
                        if let Err(error) = write_response(writer, &working) {
                            write_error = Some(error);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other(
                        "bridge request handler exited without a response",
                    ));
                }
            }
        }
    })
}

fn write_response(writer: &mut TcpStream, response: &Response) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod tests;
