//! Reconnecting adapter-to-MCP session loop used by bridge processes.
//!
//! The backend connection (GDB/WebSocket) is owned by the caller and therefore survives an MCP
//! socket EOF. Only the front-side TCP session is recreated, preserving emulator state across MCP
//! restarts and consecutive-timeout connection drops.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use super::protocol::{ProtocolError, Request, Response};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MIN_RETRY: Duration = Duration::from_millis(50);
const MAX_RETRY: Duration = Duration::from_secs(2);

pub fn serve_reconnecting<F>(port: u16, label: &str, handle: F) -> io::Result<()>
where
    F: FnMut(Request) -> Response,
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
    F: FnMut(Request) -> Response,
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
    F: FnMut(Request) -> Response,
{
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        let count = reader.read_line(&mut line)?;
        if count == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => handle(request),
            Err(error) => Response {
                id: 0,
                ok: false,
                result: None,
                error: Some(ProtocolError {
                    kind: "protocol_error".into(),
                    message: error.to_string(),
                }),
            },
        };
        serde_json::to_writer(&mut writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn reconnects_front_session_without_recreating_handler_state() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let mut calls = 0u64;
            serve_reconnecting_inner(
                port,
                "test-bridge",
                move |request| {
                    calls += 1;
                    Response {
                        id: request.id,
                        ok: true,
                        result: Some(serde_json::json!({"calls": calls})),
                        error: None,
                    }
                },
                Some(2),
            )
        });

        for expected in [1, 2] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .write_all(
                    format!(
                        "{{\"v\":1,\"id\":{expected},\"method\":\"status\",\"params\":{{}}}}\n"
                    )
                    .as_bytes(),
                )
                .unwrap();
            let mut response = String::new();
            BufReader::new(socket.try_clone().unwrap())
                .read_line(&mut response)
                .unwrap();
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["result"]["calls"], expected);
            drop(socket);
        }
        worker.join().unwrap().unwrap();
    }
}
