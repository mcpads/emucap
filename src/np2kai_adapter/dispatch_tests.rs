use super::*;
#[test]
fn cancellation_reaches_the_owner_without_moving_native_state_to_the_server() {
    let (tx, rx) = mpsc::channel();
    let mut control = Control::new(tx);
    let token = RequestCancellation::default();
    let signal = token.clone();
    let key = OperationKey {
        runtime: "run".into(),
        owner_id: "owner".into(),
        operation_id: "parent".into(),
    };
    let expected = key.clone();
    let worker = std::thread::spawn(move || {
        let Command::Request {
            attachment,
            request,
            cancellation,
            reply,
        } = rx.recv().unwrap()
        else {
            panic!("request expected")
        };
        assert_eq!(attachment.unwrap().broker_instance, "broker");
        signal.cancel();
        assert!(cancellation.is_cancelled());
        reply
            .send((
                BridgeReply::continue_with(Response {
                    id: request.id,
                    ok: true,
                    result: Some(json!({})),
                    error: None,
                }),
                Some(key),
            ))
            .unwrap();
    });
    let response = control.request(
        &Attachment {
            broker_instance: "broker".into(),
            registration: 1,
            session: 1,
        },
        Request::new(7, "step", json!({"frames":2})),
        token,
    );
    assert!(response.response.ok);
    assert_eq!(response.response.id, 7);
    assert_eq!(control.active_parent_key(), Some(expected));
    worker.join().unwrap();
}
#[test]
fn missing_owner_reply_retires_the_control_channel() {
    let (tx, rx) = mpsc::channel();
    drop(rx);
    let mut control = Control::new(tx);
    let response = control.legacy(Request::new(9, "status", json!({})));
    assert!(!response.response.ok);
    assert_eq!(response.response.id, 9);
    assert!(matches!(response.directive, BridgeDirective::Terminate));
}

#[test]
fn cancelled_owner_wait_expires_without_a_native_reply() {
    let (_keep_sender, receiver) = mpsc::sync_channel(1);
    let token = RequestCancellation::default();
    token.cancel();
    let start = Instant::now();
    assert!(wait_reply(
        receiver,
        &token,
        start + Duration::from_secs(30),
        Duration::from_millis(20)
    )
    .is_none());
    assert!(start.elapsed() < Duration::from_secs(1));
}
#[test]
fn late_owner_reply_cannot_revalidate_an_expired_operation() {
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .send((
            BridgeReply::continue_with(Response {
                id: 1,
                ok: true,
                result: Some(json!({})),
                error: None,
            }),
            None,
        ))
        .unwrap();
    assert!(wait_reply(
        receiver,
        &Default::default(),
        Instant::now(),
        Duration::from_secs(5)
    )
    .is_none());
}
