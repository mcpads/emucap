use super::*;
use std::io::BufRead;
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

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
}

#[test]
fn slow_backend_emits_working_before_terminal_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap();
    let client = TcpStream::connect(endpoint).unwrap();
    let (server, _) = listener.accept().unwrap();

    let worker = std::thread::spawn(move || {
        let mut handle = |request: Request| {
            std::thread::sleep(Duration::from_millis(45));
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
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], 7);
        if response["result"]["status"] == "working" {
            saw_working = true;
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
    let completed = Arc::new(AtomicBool::new(false));
    let completed_in_handler = Arc::clone(&completed);

    let worker = std::thread::spawn(move || {
        let mut handle = move |request: Request| {
            std::thread::sleep(Duration::from_millis(35));
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
    drop(client);

    assert!(worker.join().unwrap().is_err());
    assert!(
        completed.load(Ordering::SeqCst),
        "a dead frontend must not detach a still-mutating backend handler"
    );
}
