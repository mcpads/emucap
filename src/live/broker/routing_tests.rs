use super::*;
use std::io::Read;
use std::sync::mpsc;

fn sockets() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    (listener.accept().unwrap().0, client)
}
fn registry() -> (Shared, mpsc::Receiver<String>, TcpStream, TcpStream) {
    let (writer, peer) = sockets();
    let (session, frontend) = sockets();
    let (to_emu, receiver) = outbound::held_queue(writer);
    let reg = Arc::new(Mutex::new(Registry::default()));
    lock(&reg).emus.insert(
        "emu".into(),
        Emu {
            to_emu,
            lifecycle_runtime: None,
            methods: vec![],
            identity: serde_json::json!({}),
            session: Some(session),
            gen: 7,
            session_gen: 8,
        },
    );
    (reg, receiver, peer, frontend)
}

#[test]
fn stale_frontend_and_producer_cannot_use_a_replacement_route() {
    let (reg, receiver, _peer, _frontend) = registry();
    assert!(!enqueue_request(&reg, "emu", 6, 8, "old registration".into()).unwrap());
    assert!(!enqueue_request(&reg, "emu", 7, 6, "old session".into()).unwrap());
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(response_target(&reg, "emu", 6).is_none());
    assert!(response_target(&reg, "emu", 7).is_some());
    assert!(enqueue_request(&reg, "emu", 7, 8, "first".into()).unwrap());
    lock(&reg).emus.get_mut("emu").unwrap().session_gen = 9;
    assert!(!enqueue_request(&reg, "emu", 7, 8, "buffered stale input".into()).unwrap());
    assert!(enqueue_request(&reg, "emu", 7, 9, "second".into()).unwrap());
    assert_eq!(receiver.recv().unwrap(), "first");
    assert_eq!(receiver.recv().unwrap(), "second");
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn saturation_closes_the_exact_transport_without_blocking_registry() {
    let (reg, _receiver, mut peer, _frontend) = registry();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut refused = false;
    for _ in 0..100 {
        if enqueue_request(&reg, "emu", 7, 8, "payload".into()).is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert!(reg.try_lock().is_ok());
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn producer_writer_preserves_whole_message_order() {
    let (writer, peer) = sockets();
    let outbound = outbound::Outbound::new(writer).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut reader = BufReader::new(peer);
    let mut pending = vec![];
    for line in ["attach", "request", "detach"] {
        outbound.enqueue(line.into()).unwrap();
    }
    for expected in ["attach", "request", "detach"] {
        assert_eq!(
            read_ndjson_frame(&mut reader, &mut pending)
                .unwrap()
                .unwrap()
                .trim(),
            expected
        );
    }
    drop(outbound);
    assert!(read_ndjson_frame(&mut reader, &mut pending)
        .unwrap()
        .is_none());
}

#[test]
fn lifecycle_and_requests_share_order_and_stale_detach_is_inert() {
    let (reg, receiver, _peer, _frontend) = registry();
    {
        let mut g = lock(&reg);
        let instance = g.instance.clone();
        let emu = g.emus.get_mut("emu").unwrap();
        emu.lifecycle_runtime = Some("runtime".into());
        emu.session = None;
        let (writer, _reader) = sockets();
        pair_session(emu, &instance, writer, 10).unwrap();
    }
    let first: serde_json::Value = serde_json::from_str(&receiver.recv().unwrap()).unwrap();
    assert_eq!(first["_control_session"]["kind"], "attach");
    assert_eq!(first["_control_session"]["runtime"], "runtime");
    assert!(enqueue_request(
        &reg,
        "emu",
        7,
        10,
        r#"{"id":1,"method":"step","params":{"frames":2}}"#.into()
    )
    .unwrap());
    let request: serde_json::Value = serde_json::from_str(&receiver.recv().unwrap()).unwrap();
    assert_eq!(
        request["params"]["_control_attachment"],
        first["_control_session"]["attachment"]
    );
    {
        let mut g = lock(&reg);
        let instance = g.instance.clone();
        let (writer, _reader) = sockets();
        pair_session(g.emus.get_mut("emu").unwrap(), &instance, writer, 11).unwrap();
    }
    let old: serde_json::Value = serde_json::from_str(&receiver.recv().unwrap()).unwrap();
    let new: serde_json::Value = serde_json::from_str(&receiver.recv().unwrap()).unwrap();
    assert_eq!(old["_control_session"]["kind"], "detach");
    assert_eq!(
        old["_control_session"]["attachment"],
        first["_control_session"]["attachment"]
    );
    assert_eq!(new["_control_session"]["kind"], "attach");
    assert_eq!(new["_control_session"]["attachment"]["session"], 11);
    detach_session(&reg, "emu", 7, 10);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    detach_session(&reg, "emu", 7, 11);
    let detached: serde_json::Value = serde_json::from_str(&receiver.recv().unwrap()).unwrap();
    assert_eq!(
        detached["_control_session"]["attachment"],
        new["_control_session"]["attachment"]
    );
    assert!(!enqueue_request(&reg, "emu", 7, 11, "later".into()).unwrap());
}

#[test]
fn caller_cannot_inject_attachment_or_lifecycle_even_on_a_legacy_route() {
    let (reg, receiver, _peer, _frontend) = registry();
    for line in [
        r#"{"_control_session":{"kind":"detach"}}"#,
        r#"{"method":"_control_session","params":{}}"#,
        r#"{"method":"step","params":{"_control_attachment":{}}}"#,
        r#"{"method":"step","_control_attachment":{},"params":{}}"#,
    ] {
        assert!(enqueue_request(&reg, "emu", 7, 8, line.into()).is_err());
    }
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn lifecycle_opt_in_requires_a_bounded_runtime_identity() {
    use serde_json::json;
    assert_eq!(control_session::advertised_runtime(&json!({})), Ok(None));
    assert_eq!(
        control_session::advertised_runtime(
            &json!({"control_session_lifecycle":true,"launch_id":"live"})
        ),
        Ok(Some("live".into()))
    );
    for hello in [
        json!({"control_session_lifecycle":false}),
        json!({"control_session_lifecycle":true}),
        json!({"control_session_lifecycle":true,"launch_id":""}),
        json!({"control_session_lifecycle":true,"launch_id":"x".repeat(257)}),
    ] {
        assert!(control_session::advertised_runtime(&hello).is_err());
    }
}

#[test]
fn pacing_routes_to_one_instance_and_stale_sessions_mutate_neither() {
    let (reg, first, _peer, _frontend) = registry();
    let (writer, _other_peer) = sockets();
    let (session, _other_frontend) = sockets();
    let (to_emu, second) = outbound::held_queue(writer);
    lock(&reg).emus.insert(
        "other".into(),
        Emu {
            to_emu,
            lifecycle_runtime: None,
            methods: vec![],
            identity: serde_json::json!({}),
            session: Some(session),
            gen: 17,
            session_gen: 18,
        },
    );
    let speed = r#"{"id":1,"method":"execution_speed","params":{"mode":"limited","percent":50}}"#;
    for (name, generation, session) in [
        ("emu", 6, 8),
        ("emu", 7, 6),
        ("other", 7, 8),
        ("other", 17, 8),
    ] {
        assert!(!enqueue_request(&reg, name, generation, session, speed.into()).unwrap());
    }
    assert!(first.try_recv().is_err() && second.try_recv().is_err());
    assert!(enqueue_request(&reg, "emu", 7, 8, speed.into()).unwrap());
    let first_change: serde_json::Value = serde_json::from_str(&first.recv().unwrap()).unwrap();
    assert_eq!(first_change["params"]["percent"], 50);
    assert!(
        second.try_recv().is_err(),
        "the other producer must receive no mutation"
    );
    assert!(enqueue_request(&reg, "other", 17, 18, speed.into()).unwrap());
    let second_change: serde_json::Value = serde_json::from_str(&second.recv().unwrap()).unwrap();
    assert_eq!(second_change["params"]["percent"], 50);
    assert!(first.try_recv().is_err() && second.try_recv().is_err());
}
