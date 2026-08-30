use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn accept_with_greeting(listener: TcpListener) -> (TcpStream, BufReader<TcpStream>) {
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .write_all(b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\r\n")
        .unwrap();
    let reader = BufReader::new(stream.try_clone().unwrap());
    (stream, reader)
}

fn read_json(reader: &mut BufReader<TcpStream>) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn write_json(stream: &mut TcpStream, value: Value) {
    let mut line = serde_json::to_vec(&value).unwrap();
    line.push(b'\n');
    stream.write_all(&line).unwrap();
}

fn answer_capabilities(stream: &mut TcpStream, reader: &mut BufReader<TcpStream>) {
    let request = read_json(reader);
    assert_eq!(request["execute"], "qmp_capabilities");
    write_json(stream, json!({"return": {}, "id": request["id"]}));
}

#[test]
fn handshake_executes_command_and_demultiplexes_events() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, mut reader) = accept_with_greeting(listener);
        answer_capabilities(&mut stream, &mut reader);
        let request = read_json(&mut reader);
        assert_eq!(request["execute"], "query-status");
        assert_eq!(request["arguments"], json!({"verbose": true}));
        write_json(
            &mut stream,
            json!({"event":"STOP", "data":{"reason":"host"}}),
        );
        write_json(
            &mut stream,
            json!({"return":{"status":"paused","running":false}, "id":request["id"]}),
        );
    });

    let mut client = QmpClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    let result = client
        .execute("query-status", Some(json!({"verbose": true})))
        .unwrap();
    assert_eq!(result["status"], "paused");
    assert_eq!(client.drain_events()[0]["event"], "STOP");
    assert!(!client.is_terminal());
    handle.join().unwrap();
}

#[test]
fn handshake_retries_when_a_listener_is_not_ready_to_greet() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (first, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(250));
        drop(first);

        let (mut stream, mut reader) = accept_with_greeting(listener);
        answer_capabilities(&mut stream, &mut reader);
        let request = read_json(&mut reader);
        assert_eq!(request["execute"], "query-status");
        write_json(
            &mut stream,
            json!({"return":{"status":"paused","running":false}, "id":request["id"]}),
        );
    });

    let mut client = QmpClient::connect(
        "127.0.0.1",
        port,
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(
        client.execute("query-status", None).unwrap()["status"],
        "paused"
    );
    handle.join().unwrap();
}

#[test]
fn emulator_error_does_not_poison_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, mut reader) = accept_with_greeting(listener);
        answer_capabilities(&mut stream, &mut reader);
        let bad = read_json(&mut reader);
        write_json(
            &mut stream,
            json!({"error":{"class":"DeviceNotFound","desc":"missing tray"},"id":bad["id"]}),
        );
        let good = read_json(&mut reader);
        write_json(
            &mut stream,
            json!({"return":{"running":false},"id":good["id"]}),
        );
    });

    let mut client = QmpClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(matches!(
        client.execute("blockdev-open-tray", None),
        Err(QmpError::Emulator { ref class, .. }) if class == "DeviceNotFound"
    ));
    assert!(!client.is_terminal());
    assert_eq!(
        client.execute("query-status", None).unwrap()["running"],
        false
    );
    handle.join().unwrap();
}

#[test]
fn malformed_response_poisons_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, mut reader) = accept_with_greeting(listener);
        answer_capabilities(&mut stream, &mut reader);
        let _request = read_json(&mut reader);
        stream.write_all(b"not-json\n").unwrap();
    });

    let mut client = QmpClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(matches!(
        client.execute("query-status", None),
        Err(QmpError::Json(_))
    ));
    assert!(client.is_terminal());
    assert!(matches!(
        client.execute("query-status", None),
        Err(QmpError::Poisoned)
    ));
    handle.join().unwrap();
}

#[test]
fn truncated_response_poisons_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, mut reader) = accept_with_greeting(listener);
        answer_capabilities(&mut stream, &mut reader);
        let _request = read_json(&mut reader);
        stream.write_all(b"{\"return\":{}").unwrap();
    });

    let mut client = QmpClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(matches!(
        client.execute("query-status", None),
        Err(QmpError::Io(_))
    ));
    assert!(client.is_terminal());
    handle.join().unwrap();
}
