use super::*;
use crate::live::protocol::to_line;
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn key() -> OperationKey {
    OperationKey {
        runtime: "launch-a".into(),
        owner_id: "owner-a".into(),
        operation_id: "op-a".into(),
    }
}
fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    (client, server)
}
fn terminal(id: u64) -> BridgeReply {
    BridgeReply::continue_with(Response {
        id,
        ok: true,
        error: None,
        result: Some(serde_json::json!({"status":"interrupted", "reason":"cancelled"})),
    })
}
fn send(client: &mut TcpStream, id: u64, method: &str, params: serde_json::Value) {
    client
        .write_all(to_line(&Request::new(id, method, params)).as_bytes())
        .unwrap();
}
fn receive(reader: &mut BufReader<TcpStream>, id: u64) -> Response {
    let mut pending = Vec::new();
    loop {
        let line = read_ndjson_frame(reader, &mut pending).unwrap().unwrap();
        let response: Response = serde_json::from_str(&line).unwrap();
        if response
            .result
            .as_ref()
            .is_some_and(|r| r["status"] == "working")
        {
            continue;
        }
        assert_eq!(response.id, id);
        return response;
    }
}

#[test]
fn exact_identity_only_and_cleanup_precedes_original_terminal() {
    let (mut client, mut server) = pair();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let cleaned = Arc::new(AtomicBool::new(false));
    let worker_cleaned = cleaned.clone();
    let worker = std::thread::spawn(move || {
        let mut reader = BufReader::new(server.try_clone().unwrap());
        execute_cancellable(
            &mut reader,
            &mut server,
            &mut Vec::new(),
            Request::new(1, "step", serde_json::json!({})),
            &key(),
            Duration::from_secs(1),
            move |request, cancel| {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !cancel.is_cancelled() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                observed_tx.send(()).unwrap();
                finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                worker_cleaned.store(true, Ordering::SeqCst);
                terminal(request.id)
            },
        )
        .unwrap()
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    for (id, field) in [(2, "runtime"), (3, "owner_id"), (4, "operation_id")] {
        let mut stale = serde_json::to_value(key()).unwrap();
        stale[field] = "stale".into();
        send(&mut client, id, "cancel_operation", stale);
        assert_eq!(
            receive(&mut reader, id).result.unwrap()["status"],
            "not_active"
        );
        assert!(observed_rx.try_recv().is_err());
    }
    send(&mut client, 5, "set_execution_speed", serde_json::json!({}));
    assert_eq!(receive(&mut reader, 5).error.unwrap().kind, "busy");
    for id in [6, 7] {
        send(
            &mut client,
            id,
            "cancel_operation",
            serde_json::to_value(key()).unwrap(),
        );
        assert_eq!(
            receive(&mut reader, id).result.unwrap()["status"],
            "requested"
        );
    }
    observed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(!cleaned.load(Ordering::SeqCst));
    finish_tx.send(()).unwrap();
    assert_eq!(
        receive(&mut reader, 1).result.unwrap()["reason"],
        "cancelled"
    );
    assert!(cleaned.load(Ordering::SeqCst));
    assert_eq!(worker.join().unwrap().directive, BridgeDirective::Continue);
}

#[test]
fn disconnect_cancels_and_joins_native_cleanup() {
    let (client, mut server) = pair();
    let cleaned = Arc::new(AtomicBool::new(false));
    let worker_cleaned = cleaned.clone();
    let worker = std::thread::spawn(move || {
        let mut reader = BufReader::new(server.try_clone().unwrap());
        execute_cancellable(
            &mut reader,
            &mut server,
            &mut Vec::new(),
            Request::new(1, "step", serde_json::json!({})),
            &key(),
            Duration::from_secs(1),
            move |request, cancel| {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !cancel.is_cancelled() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                worker_cleaned.store(true, Ordering::SeqCst);
                BridgeReply::terminate_with(terminal(request.id).response)
            },
        )
    });
    drop(client);
    let completion = worker.join().unwrap().unwrap();
    assert!(completion.transport_error.is_some());
    assert_eq!(completion.directive, BridgeDirective::Terminate);
    assert!(cleaned.load(Ordering::SeqCst));
}

#[test]
fn partial_control_frame_preserves_bytes_without_waiting_for_newline() {
    let (mut client, server) = pair();
    server
        .set_read_timeout(Some(Duration::from_millis(25)))
        .unwrap();
    let mut reader = BufReader::new(server);
    let mut pending = Vec::new();
    client.write_all(b"{\"v\":1,").unwrap();
    assert_eq!(
        read_control_chunk(&mut reader, &mut pending)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(pending, b"{\"v\":1,");
    // No newline has arrived; the executor can now inspect worker completion.
    client.write_all(b"\"id\":3}\n{}").unwrap();
    assert_eq!(
        read_control_chunk(&mut reader, &mut pending)
            .unwrap()
            .unwrap(),
        "{\"v\":1,\"id\":3}\n"
    );
    assert!(pending.is_empty());
    assert_eq!(
        read_control_chunk(&mut reader, &mut pending)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(pending, b"{}");
    drop(client);
    assert_eq!(
        read_control_chunk(&mut reader, &mut pending)
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn session_rejects_wrong_runtime_then_cancels_and_accepts_next_request() {
    let (mut client, server) = pair();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let native_calls = calls.clone();
    let worker = std::thread::spawn(move || {
        let mut handle = move |request: Request, cancel: RequestCancellation| {
            native_calls.fetch_add(1, Ordering::SeqCst);
            if request.method == "step" {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !cancel.is_cancelled() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            terminal(request.id)
        };
        super::super::serve_one_cancellable(
            server,
            &mut handle,
            &mut || None,
            Duration::from_millis(10),
            Some(&TemporalAdmission {
                runtime: "launch-a".into(),
                methods: vec!["step".into()],
            }),
        )
        .unwrap()
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut stale = key();
    stale.runtime = "old-launch".into();
    send(
        &mut client,
        1,
        "step",
        serde_json::json!({"_control":stale}),
    );
    assert_eq!(receive(&mut reader, 1).error.unwrap().kind, "bad_params");
    send(
        &mut client,
        2,
        "reset",
        serde_json::json!({"_control":key()}),
    );
    assert_eq!(receive(&mut reader, 2).error.unwrap().kind, "bad_params");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    send(
        &mut client,
        3,
        "step",
        serde_json::json!({"_control":key()}),
    );
    send(
        &mut client,
        4,
        "cancel_operation",
        serde_json::to_value(key()).unwrap(),
    );
    assert_eq!(
        receive(&mut reader, 4).result.unwrap()["status"],
        "requested"
    );
    assert_eq!(
        receive(&mut reader, 3).result.unwrap()["reason"],
        "cancelled"
    );
    send(
        &mut client,
        5,
        "cancel_operation",
        serde_json::to_value(key()).unwrap(),
    );
    assert_eq!(
        receive(&mut reader, 5).result.unwrap()["status"],
        "not_active"
    );
    send(&mut client, 6, "status", serde_json::json!({}));
    assert!(receive(&mut reader, 6).ok);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(reader);
    drop(client);
    assert_eq!(
        worker.join().unwrap(),
        super::super::SessionEnd::FrontDisconnected
    );
}

fn attachment(session: u64) -> Attachment {
    Attachment {
        broker_instance: "broker-a".into(),
        registration: 1,
        session,
    }
}
#[test]
fn active_detach_cancels_child_then_returns_owner_loss_before_reading_new_attach() {
    let (mut client, mut server) = pair();
    let (seen_tx, seen_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut reader = BufReader::new(server.try_clone().unwrap());
        let mut pending = vec![];
        let completion = execute_cancellable_attached(
            &mut reader,
            &mut server,
            &mut pending,
            Request::new(1, "step", serde_json::json!({})),
            &key(),
            Some(&attachment(1)),
            Duration::from_secs(1),
            move |request, cancel| {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !cancel.is_cancelled() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                seen_tx.send(()).unwrap();
                finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                terminal(request.id)
            },
        )
        .unwrap();
        assert!(completion.transport_error.is_none());
        let event = completion.session_event.unwrap();
        assert_eq!(event.attachment, attachment(1));
        assert_eq!(event.kind, EventKind::Detach);
        // The outer owner handles detach/cleanup before seeing this queued replacement attach.
        let next = read_ndjson_frame(&mut reader, &mut pending)
            .unwrap()
            .unwrap();
        assert_eq!(
            SessionEvent::from_envelope(&serde_json::from_str(&next).unwrap())
                .unwrap()
                .unwrap()
                .attachment,
            attachment(2)
        );
    });
    let stale = SessionEvent {
        kind: EventKind::Detach,
        runtime: key().runtime,
        attachment: attachment(9),
    };
    writeln!(client, "{}", stale.envelope()).unwrap();
    let mut params = serde_json::to_value(key()).unwrap();
    params["operation_id"] = "stale".into();
    params[ATTACHMENT_FIELD] = serde_json::to_value(attachment(1)).unwrap();
    send(&mut client, 2, "cancel_operation", params);
    let mut reader = BufReader::new(client.try_clone().unwrap());
    assert_eq!(
        receive(&mut reader, 2).result.unwrap()["status"],
        "not_active"
    );
    assert!(seen_rx.try_recv().is_err());
    let detach = SessionEvent {
        kind: EventKind::Detach,
        runtime: key().runtime,
        attachment: attachment(1),
    };
    let attach = SessionEvent {
        kind: EventKind::Attach,
        runtime: key().runtime,
        attachment: attachment(2),
    };
    writeln!(client, "{}\n{}", detach.envelope(), attach.envelope()).unwrap();
    seen_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    finish_tx.send(()).unwrap();
    assert_eq!(
        receive(&mut reader, 1).result.unwrap()["reason"],
        "cancelled"
    );
    worker.join().unwrap();
}

#[test]
fn missing_attachment_cancels_native_owner_instead_of_accepting_forged_abort() {
    let (mut client, mut server) = pair();
    let worker = std::thread::spawn(move || {
        let mut reader = BufReader::new(server.try_clone().unwrap());
        execute_cancellable_attached(
            &mut reader,
            &mut server,
            &mut vec![],
            Request::new(1, "step", serde_json::json!({})),
            &key(),
            Some(&attachment(1)),
            Duration::from_secs(1),
            move |request, cancel| {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !cancel.is_cancelled() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                terminal(request.id)
            },
        )
        .unwrap()
    });
    send(
        &mut client,
        2,
        "cancel_operation",
        serde_json::to_value(key()).unwrap(),
    );
    let result = worker.join().unwrap();
    assert!(result.transport_error.is_some());
    assert!(result.session_event.is_none());
}
