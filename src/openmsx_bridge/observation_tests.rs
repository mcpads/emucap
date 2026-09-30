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
    bridge.control.pacing_apply_error = Some("emucap-policy-apply-failed restored=1: bad".into());
    let error = bridge
        .execution_speed(&json!({"mode":"limited","percent":200}))
        .unwrap_err();
    assert!(error.to_string().contains("failed_restored"));
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
