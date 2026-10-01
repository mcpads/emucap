use super::*;
use crate::live::link::{AbortRequest, RequestCancellation};
use std::io::BufRead;
use std::net::TcpListener;

pub(crate) fn control() -> ProgressCallControl {
    ProgressCallControl {
        cancellation: RequestCancellation::default(),
        abort: Some(AbortRequest {
            method: "cancel_operation".into(),
            params: serde_json::json!({
                "runtime":"launch-a", "owner_id":"owner-a", "operation_id":"operation-a"
            }),
        }),
        max_host_ms: Some(2_000),
        temporal_stop_ms: Some(300),
        temporal_deadline: None,
    }
}

fn read(reader: &mut BufReader<TcpStream>) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
fn reply(writer: &mut TcpStream, id: &Value, result: Value) {
    writeln!(
        writer,
        "{}",
        serde_json::json!({"id":id,"ok":true,"result":result})
    )
    .unwrap();
}

/// Shared adversarial peer for both real link implementations, after their hello/attach.
pub(crate) fn peer(
    mut reader: BufReader<TcpStream>,
    mut writer: TcpStream,
    cancellation: RequestCancellation,
    terminal_first: bool,
) {
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let request = read(&mut reader);
    reply(
        &mut writer,
        &request["id"],
        serde_json::json!({"status":"working"}),
    );
    cancellation.cancel();
    let abort = read(&mut reader);
    assert_eq!(abort["method"], "cancel_operation");
    assert_eq!(
        &abort["params"],
        request["params"]
            .get("parent")
            .unwrap_or(&request["params"]["_control"])
    );
    assert_ne!(abort["id"], request["id"]);
    let terminal = if request["method"] == "begin_temporal_operation" {
        serde_json::json!({"status":"admitted","parent":request["params"]["parent"]})
    } else {
        serde_json::json!({"status":"interrupted","reason":"cancelled","count":1,"state":"frozen"})
    };
    if terminal_first {
        reply(&mut writer, &request["id"], terminal.clone());
    }
    // Split the acknowledgement to exercise the shared persistent NDJSON buffer.
    let ack =
        serde_json::json!({"id":abort["id"],"ok":true,"result":{"status":"requested"}}).to_string();
    let split = ack.len() / 2;
    writer.write_all(&ack.as_bytes()[..split]).unwrap();
    writer.write_all(&ack.as_bytes()[split..]).unwrap();
    writer.write_all(b"\n").unwrap();
    if !terminal_first {
        reply(&mut writer, &request["id"], terminal);
    }
    let next = read(&mut reader);
    assert_eq!(next["method"], "status");
    reply(
        &mut writer,
        &next["id"],
        serde_json::json!({"state":"frozen"}),
    );
}

fn pair() -> (BufReader<TcpStream>, TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    (BufReader::new(client.try_clone().unwrap()), client, server)
}

#[test]
fn both_reply_orders_are_drained_before_stream_reuse() {
    for terminal_first in [false, true] {
        let (mut reader, mut writer, server) = pair();
        let control = control();
        let trigger = control.cancellation.clone();
        let worker = std::thread::spawn(move || {
            peer(
                BufReader::new(server.try_clone().unwrap()),
                server,
                trigger,
                terminal_first,
            )
        });
        let result = exchange(
            &mut reader,
            &mut writer,
            &mut Vec::new(),
            Request::new(
                1,
                "step",
                serde_json::json!({"_control":control.abort.as_ref().unwrap().params}),
            ),
            2,
            &control,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(result["count"], 1);
        writer
            .write_all(to_line(&Request::new(3, "status", serde_json::json!({}))).as_bytes())
            .unwrap();
        assert_eq!(read(&mut reader)["id"], 3);
        assert_eq!(
            reader.get_ref().read_timeout().unwrap(),
            Some(Duration::from_secs(2))
        );
        worker.join().unwrap();
    }
}

#[test]
fn cancellation_acknowledgement_alone_never_proves_stop() {
    let (mut reader, mut writer, mut server) = pair();
    let mut control = control();
    control.temporal_stop_ms = Some(50);
    let trigger = control.cancellation.clone();
    let worker = std::thread::spawn(move || {
        let mut input = BufReader::new(server.try_clone().unwrap());
        read(&mut input);
        trigger.cancel();
        let abort = read(&mut input);
        reply(
            &mut server,
            &abort["id"],
            serde_json::json!({"status":"requested"}),
        );
        // Keep the socket live without a terminal beyond the stop budget.
        let mut line = String::new();
        let _ = input.read_line(&mut line);
    });
    let start = Instant::now();
    let result = exchange(
        &mut reader,
        &mut writer,
        &mut Vec::new(),
        Request::new(
            1,
            "step",
            serde_json::json!({"_control":control.abort.as_ref().unwrap().params}),
        ),
        2,
        &control,
        Duration::from_secs(2),
    );
    assert!(
        matches!(result,Err(LinkError::Emulator { ref kind,.. }) if kind=="temporal_unverified")
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    writer.shutdown(std::net::Shutdown::Both).unwrap();
    worker.join().unwrap();
}

#[test]
fn pre_dispatch_cancellation_and_identity_mismatch_write_nothing() {
    for cancelled in [false, true] {
        let (mut reader, mut writer, server) = pair();
        let control = control();
        if cancelled {
            control.cancellation.cancel();
        }
        let key = if cancelled {
            control.abort.as_ref().unwrap().params.clone()
        } else {
            serde_json::json!({})
        };
        assert!(exchange(
            &mut reader,
            &mut writer,
            &mut Vec::new(),
            Request::new(1, "step", serde_json::json!({"_control":key})),
            2,
            &control,
            Duration::from_secs(2)
        )
        .is_err());
        writer.shutdown(std::net::Shutdown::Both).unwrap();
        let mut input = BufReader::new(server);
        let mut line = String::new();
        assert_eq!(input.read_line(&mut line).unwrap(), 0);
    }
}

#[test]
fn parent_admission_drains_cancel_ack_before_following_request() {
    for terminal_first in [false, true] {
        let (mut reader, mut writer, server) = pair();
        let control = control();
        let trigger = control.cancellation.clone();
        let worker = std::thread::spawn(move || {
            peer(
                BufReader::new(server.try_clone().unwrap()),
                server,
                trigger,
                terminal_first,
            )
        });
        let key = &control.abort.as_ref().unwrap().params;
        let result = exchange(
            &mut reader,
            &mut writer,
            &mut Vec::new(),
            Request::new(
                1,
                "begin_temporal_operation",
                serde_json::json!({"parent":key,"_temporal_owner":key}),
            ),
            2,
            &control,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(result["status"], "admitted");
        assert_eq!(result["parent"], *key);
        writer
            .write_all(to_line(&Request::new(3, "status", serde_json::json!({}))).as_bytes())
            .unwrap();
        assert_eq!(read(&mut reader)["id"], 3);
        worker.join().unwrap();
    }
}

/// A partial native terminal keeps the socket live; expiry must close it, not reuse it.
pub(crate) fn stalled_parent_peer(mut reader: BufReader<TcpStream>, mut writer: TcpStream) {
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let request = read(&mut reader);
    assert_eq!(request["method"], "finish_temporal_operation");
    writer.write_all(b"{\"id\":").unwrap();
    let mut next = String::new();
    assert_eq!(
        reader.read_line(&mut next).unwrap(),
        0,
        "expired parent transport was reused"
    );
}

#[test]
fn expired_absolute_deadline_writes_no_native_request() {
    let (mut reader, mut writer, server) = pair();
    let mut control = control();
    control.temporal_deadline = Some(Instant::now());
    let key = &control.abort.as_ref().unwrap().params;
    let result = exchange(
        &mut reader,
        &mut writer,
        &mut Vec::new(),
        Request::new(
            1,
            "finish_temporal_operation",
            serde_json::json!({"parent":key,"_temporal_owner":key}),
        ),
        2,
        &control,
        Duration::from_secs(2),
    );
    assert!(
        matches!(result,Err(LinkError::Emulator { ref kind,.. }) if kind=="temporal_unverified")
    );
    writer.shutdown(std::net::Shutdown::Both).unwrap();
    let mut input = BufReader::new(server);
    let mut line = String::new();
    assert_eq!(input.read_line(&mut line).unwrap(), 0);
}
