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
        use std::io::Read;
        let (mut first, _) = listener.accept().unwrap();
        first
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // The client must abandon the greeting-less connection before the retry.
        let mut byte = [0];
        assert_eq!(first.read(&mut byte).unwrap(), 0);
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
        Duration::from_secs(1),
        Duration::from_secs(5),
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

#[test]
fn pacing_reply_timeout_fences_a_late_committed_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut stream, mut reader) = accept_with_greeting(listener);
        answer_capabilities(&mut stream, &mut reader);
        let request = read_json(&mut reader);
        assert_eq!(request["execute"], "xemu-emucap-pacing");
        let committed = request["arguments"]["percent"].as_u64().unwrap();
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        write_json(
            &mut stream,
            json!({"return":{"percent":committed},"id":request["id"]}),
        );
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut next = String::new();
        let result = reader.read_line(&mut next);
        assert!(result.is_err() || matches!(result, Ok(0)));
        assert!(
            next.is_empty(),
            "terminal client dispatched a second mutation"
        );
        committed
    });
    let mut client =
        QmpClient::connect("127.0.0.1", port, Duration::from_secs(1), Duration::ZERO).unwrap();
    client
        .stream
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    assert!(client
        .execute("xemu-emucap-pacing", Some(json!({"percent":50})))
        .is_err());
    assert!(client.is_terminal());
    release.send(()).unwrap();
    assert!(matches!(
        client.execute("xemu-emucap-pacing", None),
        Err(QmpError::Poisoned)
    ));
    assert_eq!(worker.join().unwrap(), 50);
}
