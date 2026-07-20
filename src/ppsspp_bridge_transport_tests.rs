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
