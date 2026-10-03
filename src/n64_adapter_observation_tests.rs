use super::*;

fn observed(factor: i32, limiter: i32) -> N64Pacing {
    NativePolicy {
        factor,
        limiter,
        revision: 42,
        ..NativePolicy::default()
    }
    .pacing()
    .unwrap()
}

#[test]
fn pacing_readback_preserves_the_native_revision() {
    let limited = observed(250, 1);
    assert_eq!(limited.public()["mode"], "limited");
    assert_eq!(limited.public()["percent"], 250);
    assert_eq!(limited.public()["policy_revision"], "42");
    assert_eq!(observed(250, 0).public()["mode"], "unlimited");
}

#[test]
fn native_transaction_revision_and_rejection_are_checked() {
    let previous = NativePolicy {
        factor: 100,
        limiter: 1,
        revision: u64::MAX,
        ..NativePolicy::default()
    };
    let applied = NativePolicy {
        factor: 50,
        revision: 0,
        ..previous
    };
    let result = NativePacingResult {
        version: 1,
        outcome: 0,
        previous,
        applied,
    };
    assert!(result.validate(1).is_ok(), "native revision wraps");
    assert!(result.validate(0).is_err(), "query cannot change state");
    assert!(
        NativePacingResult {
            outcome: 1,
            ..result
        }
        .validate(1)
        .is_err(),
        "rejection cannot change state"
    );
    assert!(NativePacingResult {
        applied: previous,
        outcome: 1,
        ..result
    }
    .validate(1)
    .is_ok());
    assert!(
        NativePacingResult {
            applied: previous,
            ..result
        }
        .validate(1)
        .is_ok(),
        "idempotence retains revision"
    );
    assert!(NativePacingResult {
        applied: NativePolicy {
            revision: 1,
            ..applied
        },
        ..result
    }
    .validate(1)
    .is_err());
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
    assert_eq!(observed(50, 1).frame_wait(base), Duration::from_secs(6));
    assert_eq!(observed(1, 1).frame_wait(base), Duration::from_secs(300));
    assert_eq!(observed(400, 1).frame_wait(base), base);
    assert_eq!(
        observed(1, 0).frame_wait(base),
        base,
        "unlimited is not paced"
    );
}
