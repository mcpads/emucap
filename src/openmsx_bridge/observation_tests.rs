use super::*;
use crate::live::memory_batch::{self, BatchRange};
use crate::live::pacing::{self, SpeedRequest};

fn batch(bridge: &mut OpenMsxBridge<FakeControl>, ranges: Value) -> BridgeResult<Value> {
    bridge.read_memory_batch(&json!({ "ranges": ranges }))
}

fn policy_mode(bridge: &mut OpenMsxBridge<FakeControl>) -> Value {
    bridge.speed_readback().unwrap()["mode"].clone()
}

#[test]
fn hello_pairs_batch_and_pacing_methods_with_valid_capabilities() {
    let (mut bridge, _, _temp) = fixture(false);
    let hello = result(bridge.handle_request(Request::new(1, "hello", json!({}))));
    let methods: Vec<String> = serde_json::from_value(hello["methods"].clone()).unwrap();
    let types: Vec<String> = serde_json::from_value(hello["memory_types"].clone()).unwrap();
    let features =
        crate::live::link::FeatureCapabilities::from_hello(&hello, &methods, &types).unwrap();
    let batch = features.memory_batch.unwrap();
    assert_eq!(batch.windows.len(), 3);
    let speed = features.execution_speed.unwrap();
    assert!(speed.percent.contains(1) && speed.percent.contains(1_000_000));
    assert!(!speed.percent.contains(1_000_001));
}

#[test]
fn batch_returns_ordered_exact_bytes_at_one_frozen_boundary() {
    let (mut bridge, commands, _temp) = fixture(false);
    let stop = "machine1|10.5|10|true|1";
    bridge.control.batch_reply = Some(format!("{stop};abcd;123456;{stop}"));
    let value = batch(
        &mut bridge,
        json!([
        {"memory_type":"ram", "address":4, "length":2},
        {"memory_type":"vram", "address":8, "length":3}]),
    )
    .unwrap();
    assert_eq!(value["reads"][0]["hex"], "abcd");
    assert_eq!(value["reads"][1]["hex"], "123456");
    assert_eq!(value["total_bytes"], 5);
    assert_eq!(value["boundary"]["stop_epoch"], "machine1@10.5#0");
    assert_eq!(
        value["boundary"]["runtime_generation"],
        "unmanaged-pid:4242"
    );
    assert_eq!(value["boundary"]["clocks"][0]["value"], 10);
    let ranges = [
        BatchRange {
            memory_type: "ram".into(),
            address: 4,
            length: 2,
        },
        BatchRange {
            memory_type: "vram".into(),
            address: 8,
            length: 3,
        },
    ];
    assert!(memory_batch::verify_reply(&ranges, &value, None).is_ok());
    let commands = commands.lock().unwrap();
    let batches: Vec<_> = commands
        .iter()
        .filter(|s| s.contains("set emucap_batch"))
        .collect();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].matches("debug read_block").count(), 2);
}

#[test]
fn bridge_mutations_at_the_same_emulated_time_change_the_stop_epoch() {
    let (mut bridge, _, _temp) = fixture(false);
    let range = json!([{"memory_type":"memory","address":0xffff,"length":1}]);
    let first = batch(&mut bridge, range.clone()).unwrap()["boundary"]["stop_epoch"].clone();
    bridge
        .write_memory(&json!({"memory_type":"memory","address":0xffff,"hex":"aa"}))
        .unwrap();
    let second = batch(&mut bridge, range.clone()).unwrap()["boundary"]["stop_epoch"].clone();
    assert_ne!(first, second);
    result(bridge.handle_request(Request::new(2, "reset", json!({}))));
    let third = batch(&mut bridge, range).unwrap()["boundary"]["stop_epoch"].clone();
    assert_ne!(second, third);
}

#[test]
fn batch_prevalidates_all_ranges_and_rejects_running_without_reading() {
    let (mut bridge, commands, _temp) = fixture(false);
    commands.lock().unwrap().clear();
    let good = json!({"memory_type":"ram","address":0,"length":2});
    for bad in [
        json!({"memory_type":"unknown","address":0,"length":2}),
        json!({"memory_type":"ram","address":u64::MAX,"length":2}),
        json!({"memory_type":"ram","address":0,"length":0}),
        json!({"memory_type":"ram","address":0,"length":16385}),
        json!({"memory_type":"memory","address":0xffff,"length":2}),
    ] {
        assert!(batch(&mut bridge, json!([good, bad])).is_err());
    }
    for ranges in [
        vec![],
        vec![good.clone(); 65],
        vec![json!({"memory_type":"ram","address":0,"length":16384}); 5],
    ] {
        assert!(batch(&mut bridge, json!(ranges)).is_err());
    }
    bridge.control.paused = false;
    bridge.control.breaked = false;
    assert!(batch(&mut bridge, json!([good])).is_err());
    assert!(!commands
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.contains("debug read_block")));
}

#[test]
fn invalid_native_batch_cannot_be_published_as_partial_success() {
    let stop = "machine1|10.5|10|true|1";
    for reply in [
        "machine1|NaN|10|true|1;abcd;machine1|NaN|10|true|1".into(),
        "machine1|inf|10|true|1;abcd;machine1|inf|10|true|1".into(),
        "machine1|-1|10|true|1;abcd;machine1|-1|10|true|1".into(),
        format!("{stop};00;{stop}"),
        format!("{stop};abcd;machine1|10.6|11|true|1"),
        format!("{stop};zzzz;{stop}"),
        format!("{stop};abcd;ffff;{stop}"),
        "machine1|10.5|10|false|1;abcd;machine1|10.5|10|false|1".into(),
    ] {
        let (mut bridge, _, _temp) = fixture(false);
        bridge.control.batch_reply = Some(reply.clone());
        assert!(batch(
            &mut bridge,
            json!([{"memory_type":"ram","address":0,"length":2}])
        )
        .is_err());
        assert!(bridge.backend_terminal(), "{reply}");
    }
}

#[test]
fn speed_supports_agent_selected_rates_without_execution_transition() {
    let (mut bridge, commands, _temp) = fixture(false);
    let capability = bridge.execution_speed_capability();
    for frozen in [true, false] {
        bridge.control.paused = frozen;
        bridge.control.breaked = frozen;
        for percent in [0.01, 50.0, 100.0, 137.25, 200.0, 400.0, 10000.0] {
            let request = SpeedRequest::Limited {
                centi_percent: pacing::centi_percent(percent).unwrap(),
            };
            let value = bridge
                .execution_speed(&json!({"mode":"limited","percent":percent}))
                .unwrap();
            assert!(capability.verify_change(request, &value).is_ok(), "{value}");
            assert_eq!(value["state"], if frozen { "frozen" } else { "running" });
            assert_eq!(
                (bridge.control.paused, bridge.control.breaked),
                (frozen, frozen)
            );
            assert_eq!(bridge.control.frame, 10);
        }
    }
    let value = bridge
        .execution_speed(&json!({"mode":"unlimited"}))
        .unwrap();
    assert!(capability
        .verify_change(SpeedRequest::Unlimited, &value)
        .is_ok());
    assert_eq!(value["execution_speed"]["percent"], Value::Null);
    assert_eq!(value["execution_speed"]["diagnostics"]["speed"], "10000.00");
    assert_eq!(
        bridge.execution_speed(&json!({})).unwrap(),
        value["execution_speed"]
    );
    // Pacing changes never pause, continue or step the guest.
    assert!(!commands.lock().unwrap().iter().any(|s| s == "debug cont"
        || s == "set pause off"
        || s == "debug step"
        || s.starts_with("::emucap::next_frame")));
}

#[test]
fn speed_rejects_ambiguous_requests_and_reads_external_native_changes() {
    let (mut bridge, _, _temp) = fixture(false);
    let previous = bridge.speed_readback().unwrap();
    for params in [
        json!({"mode":"limited"}),
        json!({"mode":"limited","percent":0}),
        json!({"mode":"limited","percent":10000.01}),
        json!({"mode":"limited","percent":33.333}),
        json!({"percent":200}),
        json!({"mode":"unlimited","percent":200}),
        json!({"mode":null}),
    ] {
        assert!(bridge.execution_speed(&params).is_err(), "{params}");
        assert_eq!(bridge.speed_readback().unwrap(), previous);
    }
    bridge.control.pacing = [
        "true".into(),
        "250.0".into(),
        "false".into(),
        "false".into(),
    ];
    bridge.control.policy_revision += 2;
    let external = bridge.speed_readback().unwrap();
    assert_eq!(
        (external["mode"].clone(), external["percent"].clone()),
        (json!("limited"), json!(250))
    );
    assert_ne!(external["policy_revision"], previous["policy_revision"]);
    for pacing in [
        ["true", "33.333", "false", "false"],
        ["true", "100", "true", "false"],
        ["false", "100", "false", "true"],
    ] {
        bridge.control.pacing = pacing.map(String::from);
        let policy = bridge.speed_readback().unwrap();
        assert_eq!(
            (policy["mode"].clone(), policy["percent"].clone()),
            (json!("custom"), Value::Null)
        );
    }
}

#[test]
fn speed_failure_restores_or_fails_loudly_without_overwriting_external_changes() {
    // The native command reports that it restored a partial update itself.
    let (mut bridge, _, _temp) = fixture(false);
    let native_failure = "emucap-policy-apply-failed restored=1: bad; clock_domains=openmsx_emutime_seconds,emucap_frame_seq; before=machine1|10.5|10|false|0; after=machine1|10.75|12|false|0; previous=0|true|100|false|false; final=8|true|100|false|false";
    bridge.control.pacing_apply_error = Some(native_failure.into());
    let error = bridge
        .execution_speed(&json!({"mode":"limited","percent":200}))
        .unwrap_err();
    assert!(error.to_string().contains("failed_restored"));
    assert!(error.to_string().contains(native_failure));
    assert!(!bridge.backend_terminal());

    // A native setter that silently clamps is restored and verified.
    let (mut bridge, _, _temp) = fixture(false);
    let previous = bridge.speed_readback().unwrap();
    bridge.control.pacing_clamp =
        Some(["true".into(), "150".into(), "false".into(), "false".into()]);
    let error = bridge
        .execution_speed(&json!({"mode":"limited","percent":200}))
        .unwrap_err();
    assert!(error.to_string().contains("failed_restored"), "{error}");
    assert_eq!(policy_mode(&mut bridge), previous["mode"]);
    assert!(!bridge.backend_terminal());

    // An external change after the transaction is kept instead of being rolled back.
    let (mut bridge, _, _temp) = fixture(false);
    bridge.control.pacing_clamp =
        Some(["true".into(), "150".into(), "false".into(), "false".into()]);
    bridge.control.external_write_after_apply = true;
    let error = bridge
        .execution_speed(&json!({"mode":"limited","percent":200}))
        .unwrap_err();
    assert!(error.to_string().contains("conflict"), "{error}");
    assert_eq!(bridge.control.pacing[1], "150");
    assert!(!bridge.backend_terminal());

    // Unverifiable restoration and unrestored partial updates end the debugger channel.
    for unrestored in [true, false] {
        let (mut bridge, _, _temp) = fixture(false);
        if unrestored {
            bridge.control.pacing_apply_error =
                Some("emucap-policy-apply-failed restored=0: bad".into());
        } else {
            bridge.control.pacing_clamp =
                Some(["true".into(), "150".into(), "false".into(), "false".into()]);
            bridge.control.restore_failure = true;
        }
        assert!(bridge
            .execution_speed(&json!({"mode":"limited","percent":200}))
            .is_err());
        assert!(bridge.backend_terminal());
    }
}

#[test]
fn slow_frame_step_reports_partial_progress_at_the_host_deadline() {
    let (mut bridge, _, _temp) = fixture(false);
    bridge.control.advance_deadline_after = Some(3);
    let value =
        result(bridge.handle_request(Request::new(1, "step", json!({"unit":"frames","count":60}))));
    assert_eq!(value["status"], "interrupted");
    assert_eq!(value["reason"], "host_deadline");
    assert_eq!(value["count"], 3);
    assert_eq!(value["requested"], 60);
    assert_eq!(value["state"], "frozen");
    assert!(!bridge.backend_terminal());
}

#[test]
fn cancelled_frame_step_verifies_stop_and_does_not_poison_next_request() {
    let (mut bridge, commands, _temp) = fixture(false);
    let cancellation = crate::live::link::RequestCancellation::default();
    cancellation.cancel();
    commands.lock().unwrap().clear();
    let value =
        result(bridge.handle_request_cancellable(
            Request::new(1, "step", json!({"frames":60})),
            cancellation,
        ));
    assert_eq!(value["status"], "interrupted");
    assert_eq!(value["reason"], "cancelled");
    assert_eq!(value["count"], 0);
    assert_eq!(value["state"], "frozen");
    assert!(bridge.control.paused && bridge.control.breaked);
    assert!(commands
        .lock()
        .unwrap()
        .iter()
        .any(|s| s == "::emucap::cancel_frame"));
    let next = result(bridge.handle_request(Request::new(2, "step", json!({"frames":2}))));
    assert_eq!(next["status"], "completed");
    assert_eq!(next["count"], 2);
}

#[test]
fn cancellation_cleanup_failure_retires_native_control() {
    let (mut bridge, _, _temp) = fixture(false);
    bridge.control.fail_once = Some("::emucap::cancel_frame".into());
    let cancellation = crate::live::link::RequestCancellation::default();
    cancellation.cancel();
    let response = bridge
        .handle_request_cancellable(Request::new(1, "step", json!({"frames":60})), cancellation);
    assert!(!response.ok);
    assert!(bridge.backend_terminal());
}

#[test]
fn cancellation_envelope_cannot_enable_instruction_stepping() {
    let (mut bridge, commands, _temp) = fixture(false);
    commands.lock().unwrap().clear();
    let response = bridge.handle_request(Request::new(
        1,
        "step",
        json!({
            "unit":"instructions", "count":10, "_control":{}
        }),
    ));
    assert_eq!(response.error.unwrap().kind, "unsupported");
    assert!(commands.lock().unwrap().is_empty());
}

#[test]
fn malformed_post_apply_receipt_retires_control_without_guessing_a_restore() {
    let boundary = "machine1|10.5|10|true|1";
    let previous = "0|false|100|false|false";
    let applied = "4|true|200|false|false";
    let valid = [boundary, previous, applied, boundary];
    for (index, invalid) in [
        (0, "machine1|NaN|10|true|1"),
        (1, "bad|false|100|false|false"),
        (2, "4|true|NaN|false|false"),
        (3, "machine1|10.5|bad|true|1"),
    ] {
        let (mut bridge, commands, _temp) = fixture(false);
        let mut parts = valid;
        parts[index] = invalid;
        bridge.control.pacing_apply_reply = Some(parts.join(";"));
        let response = bridge.handle_request(Request::new(
            1,
            "execution_speed",
            json!({"mode":"limited","percent":200}),
        ));
        assert!(!response.ok && response.result.is_none());
        assert_eq!(bridge.control.pacing[1], "200.00");
        assert!(bridge.backend_terminal(), "invalid receipt field {index}");
        assert!(!commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("::emucap::restore_policy ")));
    }
}

#[test]
fn unclassified_apply_error_retires_control() {
    let (mut bridge, _, _temp) = fixture(false);
    bridge.control.pacing_apply_error = Some("failure after native setters".into());
    let error = bridge
        .execution_speed(&json!({"mode":"limited","percent":200}))
        .unwrap_err();
    assert!(error.to_string().contains("unverified"));
    assert!(bridge.backend_terminal());
}

#[test]
fn pacing_observes_concurrent_stop_and_preserves_its_event() {
    for diverged in [false, true] {
        let (mut bridge, commands, _temp) = fixture(false);
        result(bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind":"exec","start":0x4000,"snapshot":["memory:0x10:2"]}),
        )));
        bridge.control.paused = false;
        bridge.control.breaked = false;
        bridge.control.stop_after_policy = Some((
            true,
            !diverged,
            hex::encode(concat!(
                "0\n",
                "1|1|x|16384|-|-|1,2,3,4,5,6,7,8,9,10,16384,65534,11,12,1,1|m,16,2,aabb|"
            )),
        ));
        commands.lock().unwrap().clear();
        let changed = bridge.execution_speed(&json!({"mode":"limited","percent":50}));
        if diverged {
            assert!(changed.is_err());
            assert!(bridge.backend_terminal());
        } else {
            assert_eq!(changed.unwrap()["state"], "frozen");
            let first = result(bridge.handle_request(Request::new(2, "poll_events", json!({}))));
            assert_eq!(first["events"].as_array().unwrap().len(), 1);
            assert_eq!(first["events"][0]["breakpoint_id"], 1);
            assert_eq!(first["events"][0]["snapshot"][0]["hex"], "aabb");
            let second = result(bridge.handle_request(Request::new(3, "poll_events", json!({}))));
            assert!(second["events"].as_array().unwrap().is_empty());
        }
        assert!(!commands
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "debug cont" || s == "set pause off" || s == "debug step"));
    }
}

#[test]
fn nested_or_malformed_restore_claim_cannot_keep_control_healthy() {
    for message in [
        "emucap-policy-apply-failed restored=0: emucap-policy-apply-failed restored=1: nested setter error",
        "emucap-policy-apply-failed restored=10: invalid status",
        "unrelated error quoting emucap-policy-apply-failed restored=1: old diagnostic",
    ] {
        let (mut bridge, commands, _temp) = fixture(false);
        bridge.control.pacing_apply_error = Some(message.into());
        let error = bridge
            .execution_speed(&json!({"mode":"limited","percent":200}))
            .unwrap_err();
        assert!(error.to_string().contains("unverified"), "{error}");
        assert!(bridge.backend_terminal(), "{message}");
        assert!(!commands.lock().unwrap().iter()
            .any(|command| command.starts_with("::emucap::restore_policy ")));
    }
}

#[test]
fn lost_policy_reply_retains_observation_without_retry_or_rollback() {
    let (mut bridge, commands, _temp) = fixture(false);
    bridge.control.lose_policy_reply = true;
    let response = bridge.handle_request(Request::new(
        73,
        "execution_speed",
        json!({"mode":"limited","percent":200}),
    ));
    assert_eq!(response.id, 73);
    assert!(!response.ok && bridge.backend_terminal());
    let message = response.error.unwrap().message;
    for expected in [
        "pre_command_observation=",
        "native_result=unavailable",
        "final_boundary=unknown",
        "openmsx_emutime_seconds",
        "guest progress may have occurred",
    ] {
        assert!(message.contains(expected), "{message}");
    }
    assert_eq!(bridge.control.pacing[1], "200.00");
    let commands = commands.lock().unwrap();
    assert_eq!(
        commands
            .iter()
            .filter(|c| c.starts_with("::emucap::apply_policy "))
            .count(),
        1
    );
    assert!(!commands
        .iter()
        .any(|c| c.starts_with("::emucap::restore_policy ")));
}

#[test]
fn bridge_policy_restore_reports_original_interval_and_restore_endpoint() {
    let (mut bridge, _, _temp) = fixture(false);
    bridge.control.pacing_clamp =
        Some(["true".into(), "150".into(), "false".into(), "false".into()]);
    let error = bridge
        .execution_speed(&json!({"mode":"limited","percent":200}))
        .unwrap_err()
        .to_string();
    for expected in [
        "failed_restored",
        "native_transaction=",
        "restore_end=",
        "restored_policy=",
        "openmsx_emutime_seconds",
    ] {
        assert!(error.contains(expected), "{error}");
    }
    assert!(!bridge.backend_terminal());
}

/// Actual native stdio, XML reader and bridge path; requires an operator-owned prepared session.
#[test]
#[ignore = "requires EMUCAP_TEST_OPENMSX_BIN and EMUCAP_TEST_OPENMSX_SESSION"]
fn native_pacing_reply_loss_retires_transport_after_proven_commit() {
    use crate::openmsx_bridge::XmlControl;
    use std::time::{Duration, Instant};
    let binary = PathBuf::from(std::env::var_os("EMUCAP_TEST_OPENMSX_BIN").unwrap());
    let manifest = PathBuf::from(std::env::var_os("EMUCAP_TEST_OPENMSX_SESSION").unwrap());
    let session: PreparedSession = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    for running in [false, true] {
        // The bridge's one-second host deadline retires the deliberately stuck child.
        // openMSX replaces Tcl `after` and does not expose Tcl `clock`.
        for fault in ["timeout", "disconnect"] {
            let temp = TempDir::new().unwrap();
            // Runtime disk writes invalidate the old mounted copy. Start each case
            // from admitted source bytes and verify the fresh identity before spawn.
            let prepared = crate::launch::openmsx::prepare_session(
                crate::launch::openmsx::OpenMsxProfile::for_system(&session.system).unwrap(),
                &session.media.source_path,
                temp.path(),
                "pacing-reply-loss",
                Some(&session.user_data.join("systemroms")),
            )
            .unwrap();
            let session = prepared.session;
            session.verify().unwrap();
            let proof = temp.path().join("policy-proof.txt");
            let control = XmlControl::spawn(&binary, &session, temp.path(), false).unwrap();
            let mut bridge =
                OpenMsxBridge::new(control, &session, temp.path(), false, false).unwrap();
            if running {
                bridge.resume(&json!({})).unwrap();
            }
            let script = format!(
                r#"
rename ::emucap::apply_policy ::emucap::apply_policy_original
proc ::emucap::apply_policy {{throttle speed}} {{
    set result [::emucap::apply_policy_original $throttle $speed]
    set fd [open {{{}}} a]
    puts $fd "apply:$result"
    close $fd
    {}
    return $result
}}
rename ::emucap::restore_policy ::emucap::restore_policy_original
proc ::emucap::restore_policy {{args}} {{
    set fd [open {{{}}} a]
    puts $fd restore
    close $fd
    return [::emucap::restore_policy_original {{*}}$args]
}}
"#,
                proof.display(),
                "while {1} {}",
                proof.display()
            );
            bridge.control.command(&script).unwrap();
            bridge
                .control
                .set_command_deadline(Some(Instant::now() + Duration::from_secs(1)));
            let disconnect = if fault == "disconnect" {
                let proof = proof.clone();
                let pid = bridge.child_pid();
                Some(std::thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    while Instant::now() < deadline {
                        if fs::read_to_string(&proof).is_ok_and(|s| s.ends_with('\n')) {
                            return std::process::Command::new("kill")
                                .args(["-KILL", &pid.to_string()])
                                .status()
                                .unwrap()
                                .success();
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    false
                }))
            } else {
                None
            };
            let started = Instant::now();
            let error = bridge
                .execution_speed(&json!({"mode":"limited","percent":200}))
                .unwrap_err()
                .to_string();
            if let Some(disconnect) = disconnect {
                assert!(disconnect.join().unwrap());
            }
            assert!(started.elapsed() < Duration::from_secs(4), "{error}");
            assert!(bridge.backend_terminal());
            assert!(bridge.control.is_terminal(), "{error}");
            assert!(error.contains("native_result=unavailable"), "{error}");
            assert!(error.contains("pre_command_observation="), "{error}");
            let committed = fs::read_to_string(&proof).unwrap();
            assert_eq!(committed.lines().count(), 1, "{committed}");
            let fields: Vec<_> = committed
                .trim()
                .strip_prefix("apply:")
                .unwrap()
                .split(';')
                .collect();
            assert_eq!(fields.len(), 4);
            assert_eq!(
                fields[2].split('|').nth(2).unwrap().parse::<f64>().unwrap(),
                200.0
            );
            bridge.control.set_command_deadline(None);
            assert!(bridge.control.command("::emucap::policy").is_err());
            assert_eq!(fs::read_to_string(&proof).unwrap(), committed);
            eprintln!(
                "native pacing loss passed: running={running}, fault={fault}, owned_pid={}",
                bridge.child_pid()
            );
        }
    }
}
