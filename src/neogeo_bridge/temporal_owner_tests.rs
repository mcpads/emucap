use super::*;
struct Native {
    held: bool,
    halted: bool,
    missing_stop: bool,
    stuck_release: bool,
    exchanges: usize,
}
impl GdbTransport for Native {
    fn send(&mut self, payload: &str) -> crate::gdb_rsp::GdbResult<String> {
        assert_eq!(payload, "?");
        Ok("S05".into())
    }
    fn send_no_reply(&mut self, _: &str) -> crate::gdb_rsp::GdbResult<()> {
        panic!("unbounded mutation")
    }
    fn interrupt(&mut self) -> crate::gdb_rsp::GdbResult<String> {
        Ok("S05".into())
    }
    fn interrupt_with_timeout(&mut self, _: Duration) -> crate::gdb_rsp::GdbResult<String> {
        self.halted = true;
        Ok("S05".into())
    }
    fn send_with_timeout(
        &mut self,
        payload: &str,
        budget: Duration,
    ) -> crate::gdb_rsp::GdbResult<String> {
        assert!(budget <= Duration::from_secs(2));
        self.exchanges += 1;
        Ok(match payload {
            "qEmucap,features," => "owned_frames_v1,halt_state_v1".into(),
            "qEmucap,haltstate," => if self.missing_stop {
                "unknown"
            } else if self.halted {
                "frozen"
            } else {
                "running"
            }
            .into(),
            "qEmucap,setinput," => {
                if !self.stuck_release {
                    self.held = false;
                }
                "OK".into()
            }
            "qEmucap,setinput,7374617274" => {
                self.held = true;
                "OK".into()
            }
            "qEmucap,setinput,73656c656374" => "E08:select".into(),
            "qEmucap,inputstatus," => if self.held { "-1" } else { "0" }.into(),
            other => panic!("unexpected exchange {other}"),
        })
    }
}
fn fixture() -> (NeoGeoBridge<Native>, Attachment, OperationKey) {
    let native = Native {
        held: false,
        halted: true,
        missing_stop: false,
        stuck_release: false,
        exchanges: 0,
    };
    let mut bridge = NeoGeoBridge::new(
        native,
        GdbBridgeEnv {
            launch_id: Some("runtime".into()),
            ..Default::default()
        },
        "neogeo_aes",
    )
    .unwrap();
    bridge.enable_owned_control().unwrap();
    let attachment = Attachment {
        broker_instance: "broker".into(),
        registration: 1,
        session: 1,
    };
    bridge
        .apply_control_session(SessionEvent {
            runtime: "runtime".into(),
            attachment: attachment.clone(),
            kind: EventKind::Attach,
        })
        .unwrap();
    let key = OperationKey {
        runtime: "runtime".into(),
        owner_id: "owner".into(),
        operation_id: "parent".into(),
    };
    bridge
        .begin_temporal_operation(&attachment, key.clone())
        .unwrap();
    (bridge, attachment, key)
}
fn hold(bridge: &mut NeoGeoBridge<Native>, attachment: &Attachment, key: &OperationKey) {
    assert!(
        bridge
            .handle_parent_request(
                attachment,
                key,
                Request::new(1, "set_input", json!({"buttons":["start"]})),
                Default::default()
            )
            .unwrap()
            .ok
    );
    assert!(bridge.gdb.held);
}
#[test]
fn parent_finish_verifies_native_halt_and_input_release_idempotently() {
    let (mut bridge, attachment, key) = fixture();
    hold(&mut bridge, &attachment, &key);
    bridge.gdb.halted = false; // Cached bridge.frozen is insufficient.
    let terminal = bridge.finish_temporal_operation(&attachment, &key).unwrap();
    assert_eq!(terminal["cleanup_verified"], true);
    assert_eq!(terminal["released_ports"], json!([0]));
    assert!(bridge.gdb.halted && !bridge.gdb.held);
    let exchanges = bridge.gdb.exchanges;
    assert_eq!(
        bridge.finish_temporal_operation(&attachment, &key).unwrap(),
        terminal
    );
    assert_eq!(bridge.gdb.exchanges, exchanges);
}
#[test]
fn detach_runs_the_same_native_parent_cleanup() {
    let (mut bridge, attachment, key) = fixture();
    hold(&mut bridge, &attachment, &key);
    bridge
        .apply_control_session(SessionEvent {
            runtime: "runtime".into(),
            attachment,
            kind: EventKind::Detach,
        })
        .unwrap();
    assert!(!bridge.gdb.held);
    assert!(bridge
        .producer_ownership
        .as_ref()
        .unwrap()
        .active_parent_key()
        .is_none());
    assert!(!bridge.backend_terminal());
}
#[test]
fn cleanup_requires_stop_proof_and_release_readback() {
    for missing_stop in [true, false] {
        let (mut bridge, attachment, key) = fixture();
        hold(&mut bridge, &attachment, &key);
        bridge.gdb.missing_stop = missing_stop;
        bridge.gdb.stuck_release = !missing_stop;
        assert!(bridge.finish_temporal_operation(&attachment, &key).is_err());
        assert!(bridge.backend_terminal());
        assert!(bridge.gdb.held);
        assert!(bridge.finish_temporal_operation(&attachment, &key).is_err());
    }
}
#[test]
fn foreign_parent_and_instruction_step_cannot_start_native_effects() {
    let (mut bridge, attachment, key) = fixture();
    let exchanges = bridge.gdb.exchanges;
    let mut foreign = key.clone();
    foreign.owner_id = "other".into();
    assert!(bridge
        .handle_parent_request(
            &attachment,
            &foreign,
            Request::new(1, "set_input", json!({"buttons":["start"]})),
            Default::default()
        )
        .is_err());
    assert!(bridge
        .handle_parent_request(
            &attachment,
            &key,
            Request::new(2, "step", json!({"unit":"instructions","frames":1})),
            Default::default()
        )
        .is_err());
    assert_eq!(bridge.gdb.exchanges, exchanges);
    assert_eq!(
        bridge.finish_temporal_operation(&attachment, &key).unwrap()["effects_started"],
        false
    );
}
#[test]
fn missing_native_input_is_rejected_without_retiring_healthy_control() {
    let (mut bridge, attachment, key) = fixture();
    let response = bridge
        .handle_parent_request(
            &attachment,
            &key,
            Request::new(1, "set_input", json!({"buttons":["select"]})),
            Default::default(),
        )
        .unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert!(!bridge.backend_terminal());
    assert!(!bridge.gdb.held);
    assert_eq!(
        bridge.finish_temporal_operation(&attachment, &key).unwrap()["cleanup_verified"],
        true
    );
}
