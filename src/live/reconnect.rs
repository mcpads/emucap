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

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const WORKING_INTERVAL: Duration = Duration::from_secs(1);
const MIN_RETRY: Duration = Duration::from_millis(50);
const MAX_RETRY: Duration = Duration::from_secs(2);

pub fn serve_reconnecting<F>(port: u16, label: &str, handle: F) -> io::Result<()>
where
    F: FnMut(Request) -> Response + Send,
{
    serve_reconnecting_inner(port, label, handle, None)
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
    let endpoint = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut delay = MIN_RETRY;
    let mut sessions = 0usize;
    loop {
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
        eprintln!("[{label}] emucap connected");
        let end = serve_one(stream, &mut handle);
        sessions += 1;
        if max_sessions.is_some_and(|limit| sessions >= limit) {
            return end;
        }
        match end {
            Ok(()) => eprintln!("[{label}] emucap disconnected; reconnecting"),
            Err(error) => eprintln!("[{label}] emucap session failed ({error}); reconnecting"),
        }
        std::thread::sleep(delay);
    }
}

fn serve_one<F>(stream: TcpStream, handle: &mut F) -> io::Result<()>
where
    F: FnMut(Request) -> Response + Send,
{
    serve_one_with_interval(stream, handle, WORKING_INTERVAL)
}

fn serve_one_with_interval<F>(
    stream: TcpStream,
    handle: &mut F,
    working_interval: Duration,
) -> io::Result<()>
where
    F: FnMut(Request) -> Response + Send,
{
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut pending = Vec::new();
    loop {
        let Some(line) = read_ndjson_frame(&mut reader, &mut pending)? else {
            return Ok(());
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => {
                serve_request(&mut writer, handle, request, working_interval)?;
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

/// Run a synchronous backend request without making the front-side link look idle. The bridge
/// handler remains the sole owner of its GDB/WebSocket connection, while this thread emits bounded
/// `working` frames until the terminal response is ready. If the MCP side disconnects, the handler
/// is still joined so request-scoped backend cleanup finishes before a replacement session starts.
fn serve_request<F>(
    writer: &mut TcpStream,
    handle: &mut F,
    request: Request,
    working_interval: Duration,
) -> io::Result<()>
where
    F: FnMut(Request) -> Response + Send,
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
                Ok(response) => {
                    return match write_error {
                        Some(error) => Err(error),
                        None => write_response(writer, &response),
                    };
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
