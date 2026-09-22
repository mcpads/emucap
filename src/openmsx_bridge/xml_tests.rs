use super::*;

fn fixture() -> (XmlControl, mpsc::Sender<XmlEvent>) {
    let mut child = Command::new("sh")
        .args(["-c", "exec sleep 0.2"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let (sender, events) = mpsc::channel();
    (
        XmlControl {
            child,
            stdin,
            events,
            pause: None,
            terminal: Arc::new(AtomicBool::new(false)),
            finished: false,
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
        .command_reply("restore_machine", Instant::now())
        .is_err());
    assert!(!control.is_terminal());
    sender
        .send(XmlEvent::Reply {
            ok: true,
            text: "original".into(),
        })
        .unwrap();
    assert_eq!(
        control.command_reply("machine", Instant::now()).unwrap(),
        "original"
    );
}
