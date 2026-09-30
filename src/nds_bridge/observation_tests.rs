use super::*;

#[test]
fn pacing_readback_parses_hex_fields() {
    let pacing = NdsPacing::parse("32,0,7,1f4").expect("valid readback");
    assert_eq!(pacing.percent, 50);
    assert!(!pacing.native_unlimited);
    assert_eq!(pacing.revision, 7);
    assert_eq!(pacing.clock, 500);
    for invalid in ["", "32,0,7", "32,2,7,1", "32,0,7,1,9", "zz,0,1,1"] {
        assert!(NdsPacing::parse(invalid).is_none(), "{invalid}");
    }
}

#[test]
fn pacing_policy_maps_native_state() {
    let limited = NdsPacing::parse("190,0,3,0").unwrap().public();
    assert_eq!(limited["mode"], "limited");
    assert_eq!(limited["percent"], 400);
    assert_eq!(limited["policy_revision"], "3");
    let agent_unlimited = NdsPacing::parse("0,0,4,0").unwrap().public();
    assert_eq!(agent_unlimited["mode"], "unlimited");
    assert!(agent_unlimited["percent"].is_null());
    // A disabled limiter or a held boost key overrides any agent target.
    let native = NdsPacing::parse("64,1,5,0").unwrap().public();
    assert_eq!(native["mode"], "unlimited");
    assert_eq!(native["diagnostics"]["agent_percent"], 100);
}

#[test]
fn advertised_capabilities_satisfy_the_common_contract() {
    let batch =
        serde_json::to_value(NdsBridge::<crate::gdb_rsp::GdbRspClient>::memory_batch_capability())
            .unwrap();
    MemoryBatchCapability::from_hello(&batch, &["main".into(), "arm9".into(), "arm7".into()])
        .expect("batch capability validates");
    let speed = serde_json::to_value(
        NdsBridge::<crate::gdb_rsp::GdbRspClient>::execution_speed_capability(),
    )
    .unwrap();
    ExecutionSpeedCapability::from_hello(&speed).expect("speed capability validates");
}
