use super::*;

const FEATURES: &str = "peek_block,throttle_wait_hook,fastforward_readback";

fn lua(name: &str, arg: &str) -> String {
    format!("qEmucap,{name},{}", hex::encode(arg))
}

fn featured(replies: Vec<(String, String)>) -> Bridge<FakeGdb> {
    let mut all = vec![("?".to_string(), "S05".to_string())];
    all.extend(replies);
    let mut fake = FakeGdb::from_pairs(all);
    fake.features = Some(FEATURES.into());
    Bridge::new(fake, GdbBridgeEnv::default())
}

fn pacing(revision: u64, throttled: u8, per_mille: u64) -> String {
    format!("{revision}|{throttled}|{per_mille}|1.0|0|0|0.017734")
}

#[test]
fn hello_pairs_methods_with_capabilities_only_on_a_capable_host() {
    let replies = || {
        vec![
            (
                "qEmucap,mediastatus".to_string(),
                media_status("flop1", Some("/tmp/d.hdm")),
            ),
            ("qEmucap,pointerstatus".to_string(), "NONE".to_string()),
        ]
    };
    let hello = featured(replies()).handle_request(Request::new(1, "hello", json!({})));
    let hello = hello.result.unwrap();
    let methods: Vec<String> = serde_json::from_value(hello["methods"].clone()).unwrap();
    let types: Vec<String> = serde_json::from_value(hello["memory_types"].clone()).unwrap();
    let features =
        crate::live::link::FeatureCapabilities::from_hello(&hello, &methods, &types).unwrap();
    assert!(features.memory_batch.is_some() && features.execution_speed.is_some());

    let mut all = vec![("?".to_string(), "S05".to_string())];
    all.extend(replies());
    let mut plain = Bridge::new(FakeGdb::from_pairs(all), GdbBridgeEnv::default());
    let hello = plain
        .handle_request(Request::new(1, "hello", json!({})))
        .result
        .unwrap();
    let methods: Vec<String> = serde_json::from_value(hello["methods"].clone()).unwrap();
    assert!(!methods
        .iter()
        .any(|m| m == "read_memory_batch" || m == "execution_speed"));
    assert!(hello.get("execution_speed_capability").is_none());
}

#[test]
fn batch_resolves_regions_in_one_side_effect_free_native_read() {
    let mut bridge = featured(vec![(
        lua("peekbatch", "a8010:2,400:1"),
        "OK|5@3.000000000000000000|120|abcd,ff".into(),
    )]);
    let value = bridge
        .read_memory_batch(&json!({"ranges":[
            {"memory_type":"gvram_b","address":16,"length":2},
            {"memory_type":"cpu","address":1024,"length":1}]}))
        .unwrap();
    assert_eq!(value["reads"][0]["hex"], "abcd");
    assert_eq!(value["boundary"]["stop_epoch"], "5@3.000000000000000000");
    assert_eq!(value["boundary"]["clocks"][0]["value"], 120);

    for (reply, ranges) in [
        (
            "OK|5@3.0|120|abcd",
            json!([{"memory_type":"cpu","address":0,"length":2},
                                     {"memory_type":"cpu","address":8,"length":1}]),
        ),
        (
            "E1E",
            json!([{"memory_type":"cpu","address":0,"length":2},
                       {"memory_type":"cpu","address":8,"length":1}]),
        ),
    ] {
        let mut bridge = featured(vec![(lua("peekbatch", "0:2,8:1"), reply.into())]);
        assert!(
            bridge
                .read_memory_batch(&json!({"ranges": ranges}))
                .is_err(),
            "{reply}"
        );
    }
    let mut bridge = featured(vec![]);
    assert!(bridge
        .read_memory_batch(
            &json!({"ranges":[{"memory_type":"gvram_b","address":0x7fff,"length":2}]})
        )
        .is_err());
}

#[test]
fn pacing_change_is_one_native_transaction_with_verified_restore() {
    let transaction = |applied: String| format!("{};{applied};1@2.0|9;1@2.0|9", pacing(1, 1, 1000));
    let mut bridge = featured(vec![(
        lua("setpacing", "limited|500"),
        transaction(pacing(2, 1, 500)),
    )]);
    let value = bridge
        .execution_speed(&json!({"mode":"limited","percent":50}))
        .unwrap();
    assert_eq!(value["execution_speed"]["percent"], 50);
    assert_eq!(value["previous"]["percent"], 100);
    assert_eq!(value["state"], "frozen");

    // A clamped native value is rolled back with compare-and-set and reported.
    let mut bridge = featured(vec![
        (
            lua("setpacing", "limited|500"),
            transaction(pacing(2, 1, 400)),
        ),
        (lua("restorepacing", "2|1|1000|1.0"), pacing(3, 1, 1000)),
    ]);
    let error = bridge
        .execution_speed(&json!({"mode":"limited","percent":50}))
        .unwrap_err();
    assert!(error.to_string().contains("failed_restored"), "{error}");
    assert!(!bridge.backend_terminal());

    // A newer human change is kept.
    let mut bridge = featured(vec![
        (
            lua("setpacing", "limited|500"),
            transaction(pacing(2, 1, 400)),
        ),
        (
            lua("restorepacing", "2|1|1000|1.0"),
            format!("CONFLICT|{}", pacing(3, 0, 400)),
        ),
    ]);
    let error = bridge
        .execution_speed(&json!({"mode":"limited","percent":50}))
        .unwrap_err();
    assert!(error.to_string().contains("conflict"), "{error}");
    assert!(!bridge.backend_terminal());

    // Unverifiable restoration ends the control channel.
    let mut bridge = featured(vec![
        (
            lua("setpacing", "limited|500"),
            transaction(pacing(2, 1, 400)),
        ),
        (
            lua("restorepacing", "2|1|1000|1.0"),
            "E1D:unrestored".into(),
        ),
    ]);
    assert!(bridge
        .execution_speed(&json!({"mode":"limited","percent":50}))
        .is_err());
    assert!(bridge.backend_terminal());

    // A frozen stop that moved during the transaction is not a successful change.
    let mut bridge = featured(vec![
        (
            lua("setpacing", "unlimited"),
            format!(
                "{};{};1@2.0|9;2@2.1|10",
                pacing(1, 1, 1000),
                pacing(2, 0, 1000)
            ),
        ),
        (lua("restorepacing", "2|1|1000|1.0"), pacing(3, 1, 1000)),
    ]);
    assert!(bridge
        .execution_speed(&json!({"mode":"unlimited"}))
        .is_err());

    let mut bridge = featured(vec![]);
    for bad in [
        json!({"mode":"limited","percent":0.05}),
        json!({"mode":"limited","percent":12.34}),
    ] {
        assert!(bridge.execution_speed(&bad).is_err(), "{bad}");
    }
}

#[test]
fn slow_frame_step_carries_a_deadline_and_reports_partial_progress() {
    let mut bridge = featured(vec![
        ("qEmucap,pacing".into(), pacing(4, 1, 10)),
        (lua("opdeadline", "245000"), "OK".into()),
        (lua("framestep", "600"), "DEADLINE:118".into()),
        ("qEmucap,frame".into(), "418".into()),
    ]);
    let value = bridge
        .handle_request(Request::new(3, "step", json!({"frames": 600})))
        .result
        .unwrap();
    assert_eq!(value["status"], "interrupted");
    assert_eq!(value["reason"], "host_deadline");
    assert_eq!(value["completed"], 118);
}

#[test]
fn unverified_pacing_transaction_retires_control() {
    for bad in [
        "broken",
        "E77",
        "1|1|1000|1.0|0|0|0.1;broken;1@2.0|9;1@2.0|9",
    ] {
        let mut bridge = featured(vec![(lua("setpacing", "limited|500"), bad.into())]);
        let reply = bridge.handle_request(Request::new(
            901,
            "execution_speed",
            json!({"mode":"limited","percent":50}),
        ));
        assert!(!reply.ok && reply.result.is_none(), "{bad}");
        assert!(bridge.backend_terminal(), "{bad}");
    }
}

#[test]
fn pacing_reports_a_stop_arriving_during_the_transaction() {
    for boundary_after in ["1@2.0|9", "2@2.1|10"] {
        let command = lua("setpacing", "limited|500");
        let mut bridge = featured(vec![(
            command.clone(),
            format!(
                "{};{};1@2.0|9;{boundary_after}",
                pacing(1, 1, 1000),
                pacing(2, 1, 500)
            ),
        )]);
        bridge.frozen = false;
        let interrupts_before = bridge.gdb.interrupts;
        bridge.gdb.nonblocking_after.push((command, "S05".into()));
        let result = bridge
            .execution_speed(&json!({"mode":"limited","percent":50}))
            .unwrap();
        assert_eq!(result["state"], "frozen");
        assert_eq!(result["execution_speed"]["percent"], 50);
        assert_eq!(bridge.events.len(), 1);
        assert!(bridge.gdb.nonblocking.is_empty());
        assert!(bridge.gdb.replies.is_empty());
        assert!(bridge.gdb.no_reply.is_empty());
        assert_eq!(bridge.gdb.interrupts, interrupts_before);
    }
}

#[test]
fn batch_rejection_precedes_native_dispatch() {
    let mut bridge = featured(vec![]);
    bridge.frozen = true;
    let before = bridge.gdb.calls.clone();
    let interrupts_before = bridge.gdb.interrupts;
    let good = json!({"memory_type":"cpu","address":0,"length":1});
    let invalid = [
        json!({"memory_type":"cpu","address":0,"length":0}),
        json!({"memory_type":"cpu","address":-1,"length":1}),
        json!({"memory_type":"cpu","address":u64::MAX,"length":2}),
        json!({"memory_type":"cpu","address":0,"length":65536}),
        json!({"memory_type":"device","address":0,"length":1}),
    ];
    let mut requests: Vec<Value> = invalid
        .into_iter()
        .map(|last| json!([good, last]))
        .collect();
    requests.push(json!([]));
    requests.push(Value::Array(vec![good; 65]));
    for ranges in requests {
        assert!(matches!(
            bridge.read_memory_batch(&json!({"ranges":ranges})),
            Err(BridgeError::BadParams(_))
        ));
        assert_eq!(bridge.gdb.calls, before);
        assert!(bridge.gdb.no_reply.is_empty());
        assert_eq!(bridge.gdb.interrupts, interrupts_before);
        assert!(bridge.frozen);
    }
}
