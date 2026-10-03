use super::*;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
struct Native {
    log: Arc<Mutex<Vec<String>>>,
    fail_cleanup: bool,
}
impl OwnedHandler for Native {
    fn active_parent_key(&self) -> Option<OperationKey> {
        Some(OperationKey {
            runtime: "native".into(),
            owner_id: "core".into(),
            operation_id: "parent".into(),
        })
    }
    fn request(
        &mut self,
        _: &Attachment,
        request: Request,
        cancel: RequestCancellation,
    ) -> BridgeReply {
        self.log.lock().unwrap().push(request.method.clone());
        if request.method == "step" || request.method == "status" {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !cancel.is_cancelled() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            self.log.lock().unwrap().push("child-stopped".into());
        }
        BridgeReply::continue_with(Response {
            id: request.id,
            ok: true,
            error: None,
            result: Some(serde_json::json!({"status":"completed"})),
        })
    }
    fn lifecycle(&mut self, event: SessionEvent) -> io::Result<BridgeDirective> {
        self.log.lock().unwrap().push(
            match event.kind {
                EventKind::Attach => "attach",
                EventKind::Detach => "parent-cleanup",
            }
            .into(),
        );
        if event.kind == EventKind::Detach && self.fail_cleanup {
            return Err(invalid("injected cleanup failure"));
        }
        Ok(BridgeDirective::Continue)
    }
}
type OwnedSessionFixture = (
    TcpStream,
    Arc<Mutex<Vec<String>>>,
    std::thread::JoinHandle<io::Result<SessionEnd>>,
);
fn fixture(fail_cleanup: bool) -> OwnedSessionFixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let server = listener.accept().unwrap().0;
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let log = Arc::new(Mutex::new(vec![]));
    let copy = log.clone();
    let worker = std::thread::spawn(move || {
        serve_one(
            server,
            &mut Native {
                log: copy,
                fail_cleanup,
            },
            &mut || None,
            &TemporalAdmission {
                runtime: "native".into(),
                methods: vec!["step".into()],
            },
        )
    });
    (client, log, worker)
}
#[test]
fn idle_eof_always_cleans_parent_and_cleanup_failure_prevents_reconnect() {
    for failed in [false, true] {
        let (client, log, worker) = fixture(failed);
        drop(client);
        let end = worker.join().unwrap().unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["attach", "parent-cleanup"]);
        assert_eq!(matches!(end, SessionEnd::DependencyTerminal(_)), failed);
    }
}
#[test]
fn active_direct_eof_joins_child_before_parent_cleanup() {
    let (mut client, log, worker) = fixture(false);
    writeln!(client,"{}",serde_json::json!({"v":1,"id":1,"method":"step","params":{"frames":60,"_control":{"runtime":"native","owner_id":"core","operation_id":"child"}}})).unwrap();
    // Wait for native entry so this is active-child EOF, not pre-dispatch cancellation.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !log.lock().unwrap().iter().any(|v| v == "step") {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop(client);
    let _ = worker.join().unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        vec!["attach", "step", "child-stopped", "parent-cleanup"]
    );
}
#[test]
fn malformed_frame_still_finalizes_parent() {
    let (mut client, log, worker) = fixture(false);
    writeln!(client, "not json").unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(*log.lock().unwrap(), vec!["attach", "parent-cleanup"]);
}

#[test]
fn eof_during_parent_observation_still_interrupts_wait_and_cleans_input_owner() {
    let (mut client, log, worker) = fixture(false);
    writeln!(
        client,
        "{}",
        serde_json::json!({"v":1,"id":1,"method":"status","params":{}})
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !log.lock().unwrap().iter().any(|v| v == "status") {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop(client);
    let _ = worker.join().unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        vec!["attach", "status", "child-stopped", "parent-cleanup"]
    );
}
