use super::*;
use crate::gdb_rsp::GdbRspClient;
use crate::qmp::QmpClient;

#[test]
fn pacing_readback_maps_native_percent() {
    let limited = XemuPacing::from_reply(&json!({"percent": 250, "revision": 3})).unwrap();
    assert_eq!(limited.public()["mode"], "limited");
    assert_eq!(limited.public()["percent"], 250);
    assert_eq!(limited.public()["policy_revision"], "3");
    let unlimited = XemuPacing::from_reply(&json!({"percent": 0, "revision": 4})).unwrap();
    assert_eq!(unlimited.public()["mode"], "unlimited");
    assert_eq!(unlimited.public()["percent"], Value::Null);
    let status = json!({"pacing-percent": 1001, "pacing-revision": 5});
    assert_eq!(
        XemuPacing::from_status(&status).unwrap().public()["mode"],
        "custom"
    );
    assert!(XemuPacing::from_reply(&json!({"percent": 100})).is_none());
}

#[test]
fn advertised_capabilities_satisfy_the_common_contract() {
    let batch =
        serde_json::to_value(XemuBridge::<QmpClient, GdbRspClient>::memory_batch_capability())
            .unwrap();
    MemoryBatchCapability::from_hello(&batch, &["main".into(), "cpu".into()])
        .expect("batch capability validates");
    let speed =
        serde_json::to_value(XemuBridge::<QmpClient, GdbRspClient>::execution_speed_capability())
            .unwrap();
    ExecutionSpeedCapability::from_hello(&speed).expect("speed capability validates");
}
