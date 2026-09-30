use super::*;

fn pacing(percent: u64, fast_forward: bool, fps_limit: u64, network_forced: bool) -> PpssppPacing {
    PpssppPacing::parse(&json!({
        "percent": percent, "fast_forward": fast_forward, "fps_limit": fps_limit,
        "network_forced": network_forced, "revision": 3, "vblank": 900,
    }))
    .expect("valid readback")
}

#[test]
fn pacing_readback_maps_native_limits() {
    let limited = pacing(250, false, 0, false);
    assert_eq!(limited.vblank, 900);
    assert_eq!(limited.public()["mode"], "limited");
    assert_eq!(limited.public()["percent"], 250);
    assert_eq!(limited.public()["policy_revision"], "3");
    assert_eq!(pacing(0, false, 0, false).public()["mode"], "unlimited");
    assert_eq!(pacing(100, true, 0, false).public()["mode"], "unlimited");
    // A custom or analog limit and netplay's forced rate replace the agent speed.
    assert_eq!(pacing(100, false, 1, false).public()["mode"], "custom");
    assert_eq!(pacing(100, false, 0, true).public()["mode"], "custom");
    assert!(PpssppPacing::parse(&json!({"percent": 100})).is_none());
}

#[test]
fn advertised_capabilities_satisfy_the_common_contract() {
    let batch =
        serde_json::to_value(PpssppBridge::<TungsteniteWs>::memory_batch_capability()).unwrap();
    MemoryBatchCapability::from_hello(&batch, &["main".into()])
        .expect("batch capability validates");
    let speed =
        serde_json::to_value(PpssppBridge::<TungsteniteWs>::execution_speed_capability()).unwrap();
    ExecutionSpeedCapability::from_hello(&speed).expect("speed capability validates");
}

#[test]
fn network_governor_precedes_unlimited_and_fast_forward() {
    for fast_forward in [false, true] {
        let policy = pacing(0, fast_forward, 0, true).public();
        assert_eq!(policy["mode"], "custom");
        assert!(policy["percent"].is_null());
        let cap = PpssppBridge::<TungsteniteWs>::execution_speed_capability();
        assert!(cap
            .verify_change(
                SpeedRequest::Unlimited,
                &json!({
                    "status":"completed", "state":"frozen",
                    "previous":policy, "execution_speed":policy,
                })
            )
            .is_err());
    }
}
