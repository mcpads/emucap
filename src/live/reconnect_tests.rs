use super::*;
use std::io::BufRead;
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[test]
fn reconnects_front_session_without_recreating_handler_state() {
    const SESSIONS: u64 = 32;
    #[cfg(unix)]
    let count_fds = || {
        let limit = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        (0..limit)
            .filter(|fd| unsafe { libc::fcntl(*fd as libc::c_int, libc::F_GETFD) } >= 0)
            .count()
    };
    #[cfg(unix)]
    let before = count_fds();
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
            Some(SESSIONS as usize),
        )
    });

    for expected in 1..=SESSIONS {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .write_all(
                format!("{{\"v\":1,\"id\":{expected},\"method\":\"status\",\"params\":{{}}}}\n")
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
    drop(listener);
    #[cfg(unix)]
    eprintln!(
        "{SESSIONS} bridge reconnects descriptor count: {before} -> {}",
        count_fds()
    );
}

#[test]
fn slow_backend_emits_working_before_terminal_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap();
    let client = TcpStream::connect(endpoint).unwrap();
    let (server, _) = listener.accept().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (release_tx, release_rx) = std::sync::mpsc::channel();

    let worker = std::thread::spawn(move || {
        let mut handle = move |request: Request| {
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Response {
                id: request.id,
                ok: true,
                result: Some(serde_json::json!({"status": "completed"})),
                error: None,
            }
        };
        serve_one_with_interval(server, &mut handle, Duration::from_millis(10))
    });

    let mut client = client;
    client
        .write_all(b"{\"v\":1,\"id\":7,\"method\":\"run_frames\",\"params\":{\"n\":60}}\n")
        .unwrap();
    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut saw_working = false;
    let mut release_tx = Some(release_tx);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], 7);
        if response["result"]["status"] == "working" {
            saw_working = true;
            if let Some(release) = release_tx.take() {
                release.send(()).unwrap();
            }
            continue;
        }
        assert_eq!(response["result"]["status"], "completed");
        break;
    }
    assert!(saw_working);
    drop(reader);
    drop(client);
    worker.join().unwrap().unwrap();
}

#[test]
fn frontend_disconnect_does_not_abandon_backend_terminal_cleanup() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap();
    let client = TcpStream::connect(endpoint).unwrap();
    let (server, _) = listener.accept().unwrap();
    let write_control = server.try_clone().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_in_handler = Arc::clone(&completed);

    let worker = std::thread::spawn(move || {
        let mut handle = move |request: Request| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            completed_in_handler.store(true, Ordering::SeqCst);
            Response {
                id: request.id,
                ok: true,
                result: Some(serde_json::json!({"status": "completed"})),
                error: None,
            }
        };
        serve_one_with_interval(server, &mut handle, Duration::from_millis(10))
    });

    let mut client = client;
    client
        .write_all(b"{\"v\":1,\"id\":9,\"method\":\"press_buttons\",\"params\":{}}\n")
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // A peer FIN can still allow the final write to succeed. Close the local
    // write half so this fixture deterministically exercises transport failure
    // while a backend handler owns terminal cleanup.
    write_control.shutdown(std::net::Shutdown::Write).unwrap();
    drop(client);
    release_tx.send(()).unwrap();

    assert!(worker.join().unwrap().is_err());
    assert!(
        completed.load(Ordering::SeqCst),
        "a dead frontend must not detach a still-mutating backend handler"
    );
}

#[test]
fn backend_terminal_reply_closes_front_and_exits_instead_of_reconnecting() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let worker = std::thread::spawn(move || {
        serve_reconnecting_controlled_inner(
            port,
            "test-bridge",
            |request| {
                BridgeReply::terminate_with(Response {
                    id: request.id,
                    ok: false,
                    result: None,
                    error: Some(ProtocolError {
                        kind: "bridge_error".into(),
                        message: "backend closed".into(),
                    }),
                })
            },
            || None,
            None,
        )
    });

    let (mut socket, _) = listener.accept().unwrap();
    socket
        .write_all(b"{\"v\":1,\"id\":11,\"method\":\"status\",\"params\":{}}\n")
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 11);
    assert_eq!(response["error"]["kind"], "bridge_error");
    let mut eof = String::new();
    assert_eq!(reader.read_line(&mut eof).unwrap(), 0);
    worker.join().unwrap().unwrap();
}

#[test]
fn idle_front_is_closed_when_process_dependency_ends() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let terminal = Arc::new(AtomicBool::new(false));
    let terminal_in_probe = Arc::clone(&terminal);
    let worker = std::thread::spawn(move || {
        serve_reconnecting_controlled_inner(
            port,
            "test-bridge",
            |request| {
                BridgeReply::continue_with(Response {
                    id: request.id,
                    ok: true,
                    result: Some(serde_json::json!({})),
                    error: None,
                })
            },
            move || {
                terminal_in_probe
                    .load(Ordering::SeqCst)
                    .then(|| "emulator exited".to_string())
            },
            None,
        )
    });

    let (socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    terminal.store(true, Ordering::SeqCst);
    let mut reader = BufReader::new(socket);
    let mut eof = String::new();
    assert_eq!(reader.read_line(&mut eof).unwrap(), 0);
    worker.join().unwrap().unwrap();
}
