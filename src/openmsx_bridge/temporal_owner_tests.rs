use super::*;
use crate::live::{
    control_session::{Attachment, EventKind, SessionEvent},
    reconnect::cancellation::OperationKey,
};
fn attachment(session: u64) -> Attachment {
    Attachment {
        broker_instance: "test-broker".into(),
        registration: 1,
        session,
    }
}
fn parent() -> OperationKey {
    OperationKey {
        runtime: "test-runtime".into(),
        owner_id: "core".into(),
        operation_id: "parent".into(),
    }
}
fn event(kind: EventKind, session: u64) -> SessionEvent {
    SessionEvent {
        kind,
        runtime: "test-runtime".into(),
        attachment: attachment(session),
    }
}
fn begin(bridge: &mut OpenMsxBridge<FakeControl>) {
    bridge.launch_id = Some("test-runtime".into());
    bridge
        .apply_control_session(event(EventKind::Attach, 1))
        .unwrap();
    bridge
        .begin_temporal_operation(&attachment(1), parent())
        .unwrap();
}
fn press(bridge: &mut OpenMsxBridge<FakeControl>, port: u64, button: &str) {
    result(
        bridge
            .handle_parent_request(
                &attachment(1),
                &parent(),
                Request::new(1, "set_input", json!({"port":port,"buttons":[button]})),
                Default::default(),
            )
            .unwrap(),
    );
}
#[test]
fn detach_releases_only_acquired_ports_and_verifies_native_stop() {
    let (mut bridge, _, _temp) = fixture(false);
    bridge
        .set_input(&json!({"port":1,"buttons":["left"]}))
        .unwrap();
    begin(&mut bridge);
    press(&mut bridge, 0, "space");
    press(&mut bridge, 2, "a");
    bridge.control.paused = false;
    bridge.control.breaked = false;
    bridge
        .apply_control_session(event(EventKind::Detach, 1))
        .unwrap();
    assert!(bridge.control.paused && bridge.control.breaked);
    assert_eq!(bridge.control.keymatrix[8] & 1, 1);
    assert_eq!(bridge.control.joystick_owners, [Some(0x3b), None]);
    assert!(!bridge.backend_terminal());
    bridge
        .apply_control_session(event(EventKind::Attach, 2))
        .unwrap();
    let mut next = parent();
    next.operation_id = "next".into();
    bridge
        .begin_temporal_operation(&attachment(2), next)
        .unwrap();
}
#[test]
fn step_only_and_no_effect_parent_preserve_persistent_input() {
    for effect in [false, true] {
        let (mut bridge, commands, _temp) = fixture(false);
        bridge
            .set_input(&json!({"port":0,"buttons":["space"]}))
            .unwrap();
        begin(&mut bridge);
        if effect {
            result(
                bridge
                    .handle_parent_request(
                        &attachment(1),
                        &parent(),
                        Request::new(1, "pause", json!({})),
                        Default::default(),
                    )
                    .unwrap(),
            );
        }
        commands.lock().unwrap().clear();
        let terminal = bridge
            .finish_temporal_operation(&attachment(1), &parent())
            .unwrap();
        assert_eq!(terminal["cleanup_verified"], true);
        assert_eq!(bridge.control.keymatrix[8] & 1, 0);
        assert!(!commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("keymatrix")));
        commands.lock().unwrap().clear();
        assert_eq!(
            bridge
                .finish_temporal_operation(&attachment(1), &parent())
                .unwrap(),
            terminal
        );
        assert!(commands.lock().unwrap().is_empty());
    }
}
#[test]
fn failed_native_release_retires_control_across_attachment_change() {
    let (mut bridge, _, _temp) = fixture(false);
    begin(&mut bridge);
    press(&mut bridge, 2, "a");
    bridge.control.fail_once = Some("debug write emucap_joystick_override 1 255".into());
    assert!(bridge
        .apply_control_session(event(EventKind::Detach, 1))
        .is_err());
    assert!(bridge.backend_terminal());
    assert!(bridge
        .apply_control_session(event(EventKind::Attach, 2))
        .is_err());
}
#[test]
fn keyboard_release_acknowledgement_without_readback_is_not_cleanup() {
    let (mut bridge, _, _temp) = fixture(false);
    begin(&mut bridge);
    press(&mut bridge, 0, "space");
    bridge.control.ignore_keyboard_release = true;
    assert!(bridge
        .finish_temporal_operation(&attachment(1), &parent())
        .is_err());
    assert!(bridge.backend_terminal());
}
#[test]
fn malformed_or_foreign_parent_requests_never_touch_native_input() {
    let (mut bridge, commands, _temp) = fixture(false);
    begin(&mut bridge);
    commands.lock().unwrap().clear();
    for (route, params) in [
        (attachment(2), json!({"buttons":["space"]})),
        (attachment(1), json!({"port":8,"buttons":["space"]})),
        (attachment(1), json!({"buttons":["unknown"]})),
    ] {
        assert!(bridge
            .handle_parent_request(
                &route,
                &parent(),
                Request::new(1, "set_input", params),
                Default::default()
            )
            .is_err());
    }
    assert!(commands.lock().unwrap().is_empty());
    bridge
        .finish_temporal_operation(&attachment(1), &parent())
        .unwrap();
    assert!(commands.lock().unwrap().is_empty());
}

#[test]
fn live_dispatch_refuses_unscoped_mutation_but_allows_observation_during_parent() {
    use crate::live::reconnect::owned::OwnedHandler;
    let (mut bridge, commands, _temp) = fixture(false);
    begin(&mut bridge);
    commands.lock().unwrap().clear();
    let denied = bridge.request(
        &attachment(1),
        Request::new(2, "resume", json!({})),
        Default::default(),
    );
    assert_eq!(denied.response.error.unwrap().kind, "busy");
    assert!(commands.lock().unwrap().is_empty());
    let observed = bridge.request(
        &attachment(1),
        Request::new(3, "status", json!({})),
        Default::default(),
    );
    assert!(observed.response.ok);
}

#[test]
fn lifecycle_advertisement_requires_owned_session_dispatch() {
    use crate::live::reconnect::owned::OwnedHandler;
    let (mut bridge, _, _temp) = fixture(false);
    bridge.launch_id = Some("test-runtime".into());
    assert!(bridge
        .hello()
        .unwrap()
        .get("control_session_lifecycle")
        .is_none());
    bridge
        .apply_control_session(event(EventKind::Attach, 1))
        .unwrap();
    assert!(bridge
        .hello()
        .unwrap()
        .get("temporal_cancellation_capability")
        .is_none());
    for method in ["hello", "status"] {
        let reply = OwnedHandler::request(
            &mut bridge,
            &attachment(1),
            Request::new(1, method, json!({})),
            Default::default(),
        );
        assert!(reply.response.ok);
        let result = reply.response.result.unwrap();
        assert_eq!(result["control_session_lifecycle"], true);
        if method == "hello" {
            let ad = crate::contracts::ContractAdvertisement::Reported(
                serde_json::from_value(result["contracts"].clone()).unwrap(),
            );
            let methods: Vec<String> = serde_json::from_value(result["methods"].clone()).unwrap();
            let status = crate::contracts::validate_advertisement(
                &ad,
                result["adapter"].as_str(),
                result["system"].as_str(),
                &methods,
            );
            assert_eq!(status.state, "validated", "{:?}", status.errors);
        }
        #[cfg(unix)]
        {
            assert!(result["methods"]
                .as_array()
                .unwrap()
                .contains(&json!("cancel_operation")));
            assert_eq!(
                result["temporal_cancellation_capability"],
                json!({
                    "methods":["step"], "control_service_ms":25, "stop_host_ms":1000
                })
            );
        }
        #[cfg(not(unix))]
        assert!(result.get("temporal_cancellation_capability").is_none());
    }
}

#[test]
fn core_composition_closes_the_real_producer_parent_and_preserves_other_ports() {
    use crate::live::link::{
        Capabilities, EmulatorLink, LinkError, ProgressCallControl, ProgressObserver,
    };
    use crate::live::reconnect::owned::OwnedHandler;
    struct Link {
        bridge: OpenMsxBridge<FakeControl>,
        caps: Capabilities,
        verified: Option<bool>,
    }
    impl EmulatorLink for Link {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn begin_temporal_control(&mut self, _: &OperationKey) -> Result<(), LinkError> {
            Ok(())
        }
        fn finish_temporal_control(
            &mut self,
            _: &OperationKey,
            verified: bool,
        ) -> Result<(), LinkError> {
            self.verified = Some(verified);
            Ok(())
        }
        fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
            let reply = OwnedHandler::request(
                &mut self.bridge,
                &attachment(1),
                Request::new(1, method, params),
                Default::default(),
            )
            .response;
            if let Some(error) = reply.error {
                return Err(LinkError::Emulator {
                    kind: error.kind,
                    message: error.message,
                });
            }
            Ok(reply.result.unwrap())
        }
        fn call_with_progress(
            &mut self,
            method: &str,
            params: Value,
            _: &mut ProgressObserver<'_>,
            _: &ProgressCallControl,
        ) -> Result<Value, LinkError> {
            self.call(method, params)
        }
    }
    let (mut bridge, _, _temp) = fixture(false);
    bridge.launch_id = Some("test-runtime".into());
    bridge
        .apply_control_session(event(EventKind::Attach, 1))
        .unwrap();
    bridge
        .set_input(&json!({"port":1,"buttons":["left"]}))
        .unwrap();
    let mut caps = Capabilities::empty();
    caps.methods = ["pause", "step", "set_input", "status"]
        .map(String::from)
        .to_vec();
    caps.identity.launch_id = Some("test-runtime".into());
    caps.features.temporal_cancellation = Some(crate::live::temporal::CancellationCapability {
        methods: vec!["step".into()],
        control_service_ms: 25,
        stop_host_ms: 500,
    });
    let mut link = Link {
        bridge,
        caps,
        verified: None,
    };
    crate::live::tools::tap_with_cancellation(
        &mut link,
        0,
        &["space".into()],
        2,
        2,
        Default::default(),
        "core",
    )
    .unwrap();
    assert_eq!(link.verified, Some(true));
    assert!(link
        .bridge
        .producer_ownership
        .as_ref()
        .unwrap()
        .active_parent_key()
        .is_none());
    assert_eq!(link.bridge.control.keymatrix[8] & 1, 1);
    assert_eq!(link.bridge.control.joystick_owners, [Some(0x3b), None]);
}

#[test]
fn parent_observation_checks_identity_without_acquiring_native_effects() {
    let (mut bridge, commands, _temp) = fixture(false);
    begin(&mut bridge);
    commands.lock().unwrap().clear();
    let mut wrong = parent();
    wrong.owner_id = "other".into();
    assert!(bridge
        .handle_parent_request(
            &attachment(1),
            &wrong,
            Request::new(1, "status", json!({})),
            Default::default()
        )
        .is_err());
    assert!(commands.lock().unwrap().is_empty());
    bridge
        .handle_parent_request(
            &attachment(1),
            &parent(),
            Request::new(2, "status", json!({})),
            Default::default(),
        )
        .unwrap();
    let terminal = bridge
        .finish_temporal_operation(&attachment(1), &parent())
        .unwrap();
    assert_eq!(terminal["effects_started"], false);
    assert_eq!(terminal["released_ports"], json!([]));
}
