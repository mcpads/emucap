use super::*;

#[test]
fn pacing_readback_maps_the_speed_limiter() {
    let limited = N64Pacing::observe(250, 1);
    assert_eq!(limited.public()["mode"], "limited");
    assert_eq!(limited.public()["percent"], 250);
    let unlimited = N64Pacing::observe(250, 0);
    assert_eq!(unlimited.public()["mode"], "unlimited");
    assert!(
        unlimited.revision > limited.revision,
        "a changed tuple is a new revision"
    );
    assert_eq!(N64Pacing::observe(250, 0).revision, unlimited.revision);
}

#[test]
fn advertised_capabilities_satisfy_the_common_contract() {
    let batch = serde_json::to_value(Mupen64PlusHost::memory_batch_capability()).unwrap();
    MemoryBatchCapability::from_hello(&batch, &["rdram".into()])
        .expect("batch capability validates");
    let speed = serde_json::to_value(Mupen64PlusHost::execution_speed_capability()).unwrap();
    ExecutionSpeedCapability::from_hello(&speed).expect("speed capability validates");
}

#[test]
fn observation_requests_keep_the_stop_epoch() {
    let before = BOUNDARY_SEQ.load(Ordering::Acquire);
    note_request("read_memory_batch");
    note_request("execution_speed");
    assert_eq!(BOUNDARY_SEQ.load(Ordering::Acquire), before);
    note_request("write_memory");
    assert!(BOUNDARY_SEQ.load(Ordering::Acquire) > before);
}

#[test]
fn frame_wait_stretches_with_a_slow_target() {
    let base = Duration::from_secs(3);
    assert_eq!(
        N64Pacing::observe(50, 1).frame_wait(base),
        Duration::from_secs(6)
    );
    assert_eq!(
        N64Pacing::observe(1, 1).frame_wait(base),
        Duration::from_secs(300)
    );
    assert_eq!(N64Pacing::observe(400, 1).frame_wait(base), base);
    assert_eq!(
        N64Pacing::observe(1, 0).frame_wait(base),
        base,
        "unlimited is not paced"
    );
}
