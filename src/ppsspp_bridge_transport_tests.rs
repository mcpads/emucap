use super::*;
use std::net::TcpListener;
use std::thread;
use std::time::Duration;
use tungstenite::handshake::server::{
    ErrorResponse as WsErrorResponse, Request as WsRequest, Response as WsResponse,
};
use tungstenite::{accept_hdr, Message};

#[allow(clippy::result_large_err)]
fn select_ppsspp_subprotocol(
    _request: &WsRequest,
    mut response: WsResponse,
) -> Result<WsResponse, WsErrorResponse> {
    response.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        tungstenite::http::HeaderValue::from_static(PPSSPP_SUBPROTOCOL),
    );
    Ok(response)
}

fn websocket_server(
    exchange: impl FnOnce(&mut tungstenite::WebSocket<std::net::TcpStream>) + Send + 'static,
) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = accept_hdr(stream, select_ppsspp_subprotocol).unwrap();
        exchange(&mut socket);
    });
    (port, handle)
}

fn read_json(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) -> Value {
    match socket.read().unwrap() {
        Message::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
        other => panic!("expected a text request, got {other:?}"),
    }
}

#[test]
fn delayed_error_cannot_become_the_next_calls_error() {
    let (port, server) = websocket_server(|socket| {
        let first = read_json(socket);
        assert_eq!(first["event"], "slow.command");
        let first_ticket = first["ticket"].as_str().unwrap().to_owned();

        thread::sleep(Duration::from_millis(80));
        socket
            .send(Message::Text(
                json!({
                    "event": "error",
                    "ticket": first_ticket,
                    "message": "late failure from the timed-out command",
                })
                .to_string()
                .into(),
            ))
            .unwrap();

        let second = read_json(socket);
        assert_eq!(second["event"], "version");
        let second_ticket = second["ticket"].as_str().unwrap().to_owned();
        socket
            .send(Message::Text(
                json!({
                    "event": "version",
                    "ticket": second_ticket,
                    "version": "test",
                })
                .to_string()
                .into(),
            ))
            .unwrap();
    });

    let mut ws = TungsteniteWs::connect(port, Duration::from_secs(1)).unwrap();
    let first = ws.call_with_timeout("slow.command", json!({}), Duration::from_millis(30));
    assert!(first.as_ref().is_err_and(is_timeout_error));
    assert!(
        !ws.is_terminal(),
        "a ticketed read timeout remains recoverable"
    );

    let second = ws.call("version", json!({})).unwrap();
    assert_eq!(second["version"], "test");
    assert!(!ws.is_terminal());

    let queued = ws.drain_events();
    assert!(queued.iter().any(|event| {
        event["event"] == "error" && event["message"] == "late failure from the timed-out command"
    }));
    server.join().unwrap();
}

#[test]
fn asynchronous_ack_must_echo_the_requests_ticket() {
    let (port, server) = websocket_server(|socket| {
        let request = read_json(socket);
        assert_eq!(request["event"], "cpu.stepInto");
        socket
            .send(Message::Text(
                json!({
                    "event": "cpu.stepping",
                    "ticket": request["ticket"],
                    "pc": 0x0880_4004u64,
                })
                .to_string()
                .into(),
            ))
            .unwrap();
    });

    let mut ws = TungsteniteWs::connect(port, Duration::from_secs(1)).unwrap();
    let response = ws
        .call_and_wait_for("cpu.stepInto", json!({}), "cpu.stepping")
        .unwrap();
    assert_eq!(response["pc"], 0x0880_4004u64);
    server.join().unwrap();
}

#[test]
fn websocket_close_marks_the_transport_terminal() {
    let (port, server) = websocket_server(|socket| {
        let request = read_json(socket);
        assert_eq!(request["event"], "version");
        socket.close(None).unwrap();
    });

    let mut ws = TungsteniteWs::connect(port, Duration::from_secs(1)).unwrap();
    assert!(ws.call("version", json!({})).is_err());
    assert!(ws.is_terminal());
    server.join().unwrap();
}

fn frame_protocol_capability(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) {
    let request = read_json(socket);
    assert_eq!(request["event"], "emucap.frameStep.capability");
    socket
        .send(Message::Text(
            json!({
                "event":"emucap.frameStep.capability", "ticket":request["ticket"],
                "version":1, "memory_park_version":1, "connection_owned":true
            })
            .to_string()
            .into(),
        ))
        .unwrap();
}

#[test]
fn frame_cancel_drains_both_tickets_in_either_order_and_keeps_breakpoint_event() {
    for terminal_first in [false, true] {
        let cancellation = crate::live::link::RequestCancellation::default();
        let signal = cancellation.clone();
        let (port, server) = websocket_server(move |socket| {
            frame_protocol_capability(socket);
            let step = read_json(socket);
            assert_eq!(step["event"], "emucap.frameStep");
            signal.cancel();
            let cancel = read_json(socket);
            assert_eq!(cancel["event"], "emucap.frameStep.cancel");
            assert_eq!(cancel["operation_id"], step["operation_id"]);
            assert_ne!(cancel["ticket"], step["ticket"]);
            let terminal = json!({"event":"emucap.frameStep", "ticket":step["ticket"],
                "operation_id":step["operation_id"],"unit":"frames","count":120,
                "completed":3,"status":"interrupted","state":"frozen"});
            let ack = if terminal_first {
                json!({"event":"error","ticket":cancel["ticket"],"message":"already completed"})
            } else {
                json!({"event":"emucap.frameStep.cancel","ticket":cancel["ticket"],"status":"requested"})
            };
            socket
                .send(Message::Text(
                    json!({"event":"cpu.stepping","reason":"cpu.breakpoint"})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            for reply in if terminal_first {
                [terminal, ack]
            } else {
                [ack, terminal]
            } {
                socket
                    .send(Message::Text(reply.to_string().into()))
                    .unwrap();
            }
            let next = read_json(socket);
            assert_eq!(next["event"], "version");
            socket
                .send(Message::Text(
                    json!({"event":"version","ticket":next["ticket"],"version":"after-cancel"})
                        .to_string()
                        .into(),
                ))
                .unwrap();
        });
        let mut ws = TungsteniteWs::connect(port, Duration::from_secs(1)).unwrap();
        let result = ws
            .frame_step_cancellable(120, Duration::from_secs(3), &cancellation)
            .unwrap();
        assert_eq!(result["completed"], 3);
        assert!(!ws.is_terminal());
        assert_eq!(
            ws.call("version", json!({})).unwrap()["version"],
            "after-cancel"
        );
        assert!(ws
            .drain_events()
            .iter()
            .any(|v| v["reason"] == "cpu.breakpoint"));
        server.join().unwrap();
    }
}

#[test]
fn frame_protocol_rejects_old_native_before_guest_dispatch() {
    let (port, server) = websocket_server(|socket| {
        let query = read_json(socket);
        assert_eq!(query["event"], "emucap.frameStep.capability");
        socket
            .send(Message::Text(
                json!({"event":"error","ticket":query["ticket"],"message":"unknown event"})
                    .to_string()
                    .into(),
            ))
            .unwrap();
        let next = read_json(socket);
        assert_eq!(next["event"], "version");
        socket
            .send(Message::Text(
                json!({"event":"version","ticket":next["ticket"]})
                    .to_string()
                    .into(),
            ))
            .unwrap();
    });
    let mut ws = TungsteniteWs::connect(port, Duration::from_secs(1)).unwrap();
    assert!(ws
        .frame_step_cancellable(120, Duration::from_secs(3), &Default::default())
        .is_err());
    assert!(!ws.is_terminal());
    ws.call("version", json!({})).unwrap();
    server.join().unwrap();
}

#[test]
fn unverified_frame_terminal_retires_native_channel() {
    for fault in ["wrong_operation", "running", "too_many", "false_completed"] {
        let (port, server) = websocket_server(move |socket| {
            frame_protocol_capability(socket);
            let step = read_json(socket);
            let mut result = json!({"event":"emucap.frameStep","ticket":step["ticket"],
                "operation_id":step["operation_id"],"unit":"frames","count":120,
                "completed":3,"status":"interrupted","state":"frozen"});
            match fault {
                "wrong_operation" => result["operation_id"] = json!("stale"),
                "running" => result["state"] = json!("running"),
                "too_many" => result["completed"] = json!(121),
                _ => result["status"] = json!("completed"),
            }
            socket
                .send(Message::Text(result.to_string().into()))
                .unwrap();
            assert!(
                socket.read().is_err(),
                "uncertain producer channel must close"
            );
        });
        let mut ws = TungsteniteWs::connect(port, Duration::from_secs(1)).unwrap();
        assert!(
            ws.frame_step_cancellable(120, Duration::from_secs(3), &Default::default())
                .is_err(),
            "{fault}"
        );
        assert!(ws.is_terminal(), "{fault}");
        server.join().unwrap();
    }
}

#[test]
fn cancellation_ack_without_native_terminal_cannot_certify_stop() {
    let cancellation = crate::live::link::RequestCancellation::default();
    let signal = cancellation.clone();
    let (port, server) = websocket_server(move |socket| {
        frame_protocol_capability(socket);
        let _step = read_json(socket);
        signal.cancel();
        let cancel = read_json(socket);
        socket.send(Message::Text(json!({"event":"emucap.frameStep.cancel", "ticket":cancel["ticket"], "status":"requested"}).to_string().into())).unwrap();
        assert!(
            socket.read().is_err(),
            "unverified stop must close the native connection"
        );
    });
    let mut ws = TungsteniteWs::connect(port, Duration::from_secs(1)).unwrap();
    let result = ws.frame_step_cancellable(120, Duration::from_secs(5), &cancellation);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("stop was not verified"));
    assert!(ws.is_terminal());
    server.join().unwrap();
}

#[test]
fn cancelled_before_admission_does_not_send_native_request() {
    let (port, server) = websocket_server(|socket| {
        let next = read_json(socket);
        assert_eq!(next["event"], "version");
        socket
            .send(Message::Text(
                json!({"event":"version","ticket":next["ticket"]})
                    .to_string()
                    .into(),
            ))
            .unwrap();
    });
    let mut ws = TungsteniteWs::connect(port, Duration::from_secs(1)).unwrap();
    let cancellation = crate::live::link::RequestCancellation::default();
    cancellation.cancel();
    assert!(ws
        .frame_step_cancellable(120, Duration::from_secs(3), &cancellation)
        .is_err());
    assert!(!ws.is_terminal());
    ws.call("version", json!({})).unwrap();
    server.join().unwrap();
}

#[test]
fn pacing_reply_timeout_keeps_bridge_retired_after_late_native_success() {
    let (release, wait) = std::sync::mpsc::channel();
    let (sent, delivered) = std::sync::mpsc::channel();
    let (port, server) = websocket_server(move |socket| {
        let probe = read_json(socket);
        socket
            .send(Message::Text(
                json!({"event":"emucap.pacing","ticket":probe["ticket"],
            "percent":100,"fast_forward":false,"fps_limit":0,"network_forced":false,
            "revision":1,"vblank":7,"transaction_version":1})
                .to_string()
                .into(),
            ))
            .unwrap();
        let setter = read_json(socket);
        assert_eq!(setter["event"], "emucap.pacing");
        assert_eq!(setter["percent"], 50);
        // Controlled native commit precedes the lost reply; no native rollback is attempted.
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        socket
            .send(Message::Text(
                json!({"event":"emucap.pacing","ticket":setter["ticket"],
            "percent":50,"fast_forward":false,"fps_limit":0,"network_forced":false,
            "revision":2,"vblank":7,"transaction_version":1,"clock_domain":"psp_vblank",
            "begin_vblank":7,"outcome":"completed",
            "previous":{"percent":100,"fast_forward":false,"fps_limit":0,"network_forced":false,
                        "revision":1,"vblank":7}})
                .to_string()
                .into(),
            ))
            .unwrap();
        sent.send(()).unwrap();
    });
    let ws = TungsteniteWs::connect(port, Duration::from_millis(100)).unwrap();
    let mut bridge = PpssppBridge::new(ws);
    let result = bridge.execution_speed(&json!({"mode":"limited","percent":50}));
    assert!(result.is_err());
    assert!(bridge.control_unverified && bridge.backend_terminal());
    release.send(()).unwrap();
    delivered.recv_timeout(Duration::from_secs(2)).unwrap();
    let _ = bridge.ws.drain_events();
    assert!(
        bridge.control_unverified && bridge.backend_terminal(),
        "late success cannot restore healthy control"
    );
    server.join().unwrap();
}
