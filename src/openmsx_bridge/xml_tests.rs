use super::*;

fn fixture() -> (XmlControl, mpsc::Sender<XmlEvent>) {
    let mut child = Command::new("sh")
        .args(["-c", "exec sleep 0.2"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    configure_stdin(&stdin).unwrap();
    let (sender, events) = mpsc::channel();
    (
        XmlControl {
            child,
            stdin,
            events,
            pause: None,
            terminal: Arc::new(AtomicBool::new(false)),
            finished: false,
            command_deadline: None,
            request_cancellation: Default::default(),
            cancellation_deadline: None,
        },
        sender,
    )
}

#[test]
fn missing_reply_closes_owner_before_late_reply_can_reach_rollback() {
    let (mut control, sender) = fixture();
    assert!(control
        .command_reply("restore_machine", Instant::now())
        .is_err());
    assert!(control.is_terminal());
    assert!(control.child.try_wait().unwrap().is_some());
    sender
        .send(XmlEvent::Reply {
            ok: true,
            text: "late-candidate".into(),
        })
        .unwrap();
    assert!(control.command("activate_machine original").is_err());
}

#[test]
fn native_rejection_is_recoverable_because_reply_was_consumed() {
    let (mut control, sender) = fixture();
    sender
        .send(XmlEvent::Reply {
            ok: false,
            text: "invalid disk checksum".into(),
        })
        .unwrap();
    assert!(control
        .command_reply("restore_machine", Instant::now() + Duration::from_secs(1))
        .is_err());
    assert!(!control.is_terminal());
    sender
        .send(XmlEvent::Reply {
            ok: true,
            text: "original".into(),
        })
        .unwrap();
    assert_eq!(
        control
            .command_reply("machine", Instant::now() + Duration::from_secs(1))
            .unwrap(),
        "original"
    );
}

#[test]
fn expired_shared_deadline_rejects_even_a_queued_success_and_retires_owner() {
    let (mut control, sender) = fixture();
    let deadline = Instant::now() - Duration::from_millis(1);
    control.set_command_deadline(Some(deadline));
    sender
        .send(XmlEvent::Reply {
            ok: true,
            text: "late".into(),
        })
        .unwrap();
    assert!(control.command("set pause on").is_err());
    assert!(control.is_terminal());
    assert!(control.child.try_wait().unwrap().is_some());
    control.set_command_deadline(None);
    assert!(control.command("debug cont").is_err());
}

#[test]
fn cleanup_commands_share_the_original_deadline() {
    let (mut control, sender) = fixture();
    let deadline = Instant::now() + Duration::from_millis(20);
    control.set_command_deadline(Some(deadline));
    sender
        .send(XmlEvent::Reply {
            ok: true,
            text: String::new(),
        })
        .unwrap();
    control.command("set pause on").unwrap();
    thread::sleep(deadline.saturating_duration_since(Instant::now()));
    sender
        .send(XmlEvent::Reply {
            ok: true,
            text: String::new(),
        })
        .unwrap();
    assert!(control.command("debug break").is_err());
    assert!(control.is_terminal());
}

#[test]
fn full_native_pipe_expires_and_retires_instead_of_blocking_cleanup() {
    let (mut control, _sender) = fixture();
    control.set_command_deadline(Some(Instant::now() + Duration::from_millis(20)));
    let started = Instant::now();
    assert!(control.command(&"x".repeat(1024 * 1024)).is_err());
    assert!(control.is_terminal());
    assert!(control.child.try_wait().unwrap().is_some());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn cancellation_bounds_an_already_pending_xml_reply() {
    let (mut control, _sender) = fixture();
    let cancellation = crate::live::link::RequestCancellation::default();
    control.set_request_cancellation(cancellation.clone());
    // Simulate an already-written setup/arming command. No response is provided.
    let cancel = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        cancellation.cancel();
    });
    let started = Instant::now();
    assert!(control
        .command_reply("::emucap::next_frame 60", started + COMMAND_TIMEOUT)
        .is_err());
    cancel.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(control.is_terminal());
    assert!(control.child.try_wait().unwrap().is_some());
}

#[test]
fn cancellation_consumes_pending_reply_before_cleanup_and_keeps_its_deadline() {
    let (mut control, sender) = fixture();
    let cancellation = crate::live::link::RequestCancellation::default();
    control.set_request_cancellation(cancellation.clone());
    cancellation.cancel();
    sender
        .send(XmlEvent::Reply {
            ok: true,
            text: "arming-reply".into(),
        })
        .unwrap();
    assert_eq!(
        control
            .command_reply("::emucap::next_frame 60", Instant::now() + COMMAND_TIMEOUT)
            .unwrap(),
        "arming-reply"
    );
    control.set_command_deadline(Some(Instant::now() + Duration::from_secs(5)));
    let started = Instant::now();
    assert!(control.command("::emucap::cancel_frame").is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(control.is_terminal());
}
