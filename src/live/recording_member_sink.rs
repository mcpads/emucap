use std::io::{self, BufRead, BufReader, Read};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::bundle::recording_manifest::InitialSnapshotRequest;

const HANDSHAKE_LIMIT: u64 = 1024;
const HEADER_LIMIT: u64 = 1024;
const POLL_MS: u64 = 20;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handshake {
    token: String,
    capture_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberHeader {
    label: String,
    bytes: u64,
}

#[derive(Debug)]
pub(super) struct CapturedInitialSnapshot {
    pub(super) request: InitialSnapshotRequest,
    pub(super) bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct MemberSinkOutcome {
    pub(super) members: Vec<CapturedInitialSnapshot>,
    pub(super) partial: Option<CapturedInitialSnapshot>,
    pub(super) error: Option<String>,
}

pub(super) struct MemberSinkServer {
    pub(super) endpoint: String,
    pub(super) token: String,
    stop: Arc<AtomicBool>,
    result: mpsc::Receiver<MemberSinkOutcome>,
    handle: Option<JoinHandle<()>>,
}

impl MemberSinkServer {
    pub(super) fn spawn(
        capture_id: &str,
        requests: &[InitialSnapshotRequest],
        max_host_ms: u64,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let endpoint = listener.local_addr()?.to_string();
        let token = format!(
            "member-{}{}",
            ulid::Ulid::generate().to_string().to_ascii_lowercase(),
            ulid::Ulid::generate().to_string().to_ascii_lowercase()
        );
        let expected_token = token.clone();
        let expected_capture = capture_id.to_string();
        let expected_requests = requests.to_vec();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (tx, rx) = mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || {
            let outcome = run_sink(
                listener,
                &expected_token,
                &expected_capture,
                expected_requests,
                max_host_ms,
                &thread_stop,
            );
            let _ = tx.send(outcome);
        });
        Ok(Self {
            endpoint,
            token,
            stop,
            result: rx,
            handle: Some(handle),
        })
    }

    pub(super) fn finish(mut self, max_host_ms: u64) -> MemberSinkOutcome {
        let outcome = self
            .result
            .recv_timeout(Duration::from_millis(max_host_ms.saturating_add(500)))
            .unwrap_or_else(|_| {
                self.stop.store(true, Ordering::Release);
                MemberSinkOutcome {
                    members: Vec::new(),
                    partial: None,
                    error: Some("initial snapshot sink terminal deadline exceeded".into()),
                }
            });
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        outcome
    }

    pub(super) fn cancel_unarmed(mut self) -> MemberSinkOutcome {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let mut outcome = self.result.try_recv().unwrap_or(MemberSinkOutcome {
            members: Vec::new(),
            partial: None,
            error: Some("unarmed initial snapshot sink did not terminate".into()),
        });
        if outcome.members.is_empty() && outcome.partial.is_none() {
            outcome.error = None;
        }
        outcome
    }
}

fn run_sink(
    listener: TcpListener,
    expected_token: &str,
    expected_capture: &str,
    requests: Vec<InitialSnapshotRequest>,
    max_host_ms: u64,
    stop: &AtomicBool,
) -> MemberSinkOutcome {
    let deadline = Instant::now() + Duration::from_millis(max_host_ms);
    let mut members = Vec::with_capacity(requests.len());
    let mut partial = None;
    let result = (|| -> Result<(), String> {
        let stream = loop {
            if stop.load(Ordering::Acquire) {
                return Err("initial snapshot sink stopped before connection".into());
            }
            if Instant::now() >= deadline {
                return Err("initial snapshot sink accept deadline exceeded".into());
            }
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(format!("initial snapshot sink accept failed: {error}")),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_millis(POLL_MS)))
            .map_err(|error| format!("initial snapshot sink timeout setup failed: {error}"))?;
        let mut reader = BufReader::new(stream);
        let handshake =
            read_required_line(&mut reader, HANDSHAKE_LIMIT, deadline, stop, "handshake")?;
        let handshake: Handshake = serde_json::from_slice(&handshake)
            .map_err(|error| format!("initial snapshot handshake JSON failed: {error}"))?;
        if handshake.token != expected_token || handshake.capture_id != expected_capture {
            return Err("initial snapshot sink authentication failed".into());
        }

        for request in requests {
            let header =
                read_required_line(&mut reader, HEADER_LIMIT, deadline, stop, "member header")?;
            let header: MemberHeader = serde_json::from_slice(&header)
                .map_err(|error| format!("initial snapshot member header JSON failed: {error}"))?;
            if header.label != request.label || header.bytes != request.length {
                return Err("initial snapshot member order, label, or length mismatch".into());
            }
            let length = usize::try_from(request.length)
                .map_err(|_| "initial snapshot member length exceeds host size".to_string())?;
            let mut bytes = vec![0_u8; length];
            if let Err((error, received)) =
                read_exact_bounded(&mut reader, &mut bytes, deadline, stop)
            {
                bytes.truncate(received);
                partial = Some(CapturedInitialSnapshot { request, bytes });
                return Err(format!("initial snapshot member read failed: {error}"));
            }
            members.push(CapturedInitialSnapshot { request, bytes });
        }

        let mut extra = [0_u8; 1];
        loop {
            if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err("initial snapshot sink did not close after exact members".into());
            }
            match reader.read(&mut extra) {
                Ok(0) => break,
                Ok(_) => return Err("initial snapshot sink received trailing bytes".into()),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => {
                    return Err(format!("initial snapshot sink close read failed: {error}"))
                }
            }
        }
        Ok(())
    })();
    MemberSinkOutcome {
        members,
        partial,
        error: result.err(),
    }
}

fn read_required_line<R: BufRead>(
    reader: &mut R,
    limit: u64,
    deadline: Instant,
    stop: &AtomicBool,
    name: &str,
) -> Result<Vec<u8>, String> {
    let (mut line, complete) = super::recording_sink::read_sink_line(reader, limit, deadline, stop)
        .map_err(|error| format!("{name} read failed: {error}"))?
        .ok_or_else(|| format!("initial snapshot sink closed before {name}"))?;
    if !complete {
        return Err(format!("initial snapshot {name} was truncated"));
    }
    line.pop();
    Ok(line)
}

fn read_exact_bounded<R: Read>(
    reader: &mut R,
    bytes: &mut [u8],
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<(), (io::Error, usize)> {
    let mut offset = 0;
    while offset < bytes.len() {
        if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err((
                io::Error::new(io::ErrorKind::TimedOut, "sink deadline"),
                offset,
            ));
        }
        match reader.read(&mut bytes[offset..]) {
            Ok(0) => {
                return Err((
                    io::Error::new(io::ErrorKind::UnexpectedEof, "short member"),
                    offset,
                ))
            }
            Ok(read) => offset += read,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err((error, offset)),
        }
    }
    Ok(())
}
