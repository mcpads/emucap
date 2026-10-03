use super::*;

fn pacing(mode: u32, nominal_centi: u32, target: f32) -> Pcsx2Pacing {
    let mut payload = Vec::new();
    payload.extend_from_slice(&mode.to_le_bytes());
    payload.extend_from_slice(&nominal_centi.to_le_bytes());
    payload.extend_from_slice(&target.to_bits().to_le_bytes());
    payload.extend_from_slice(&4u64.to_le_bytes());
    payload.extend_from_slice(&321u64.to_le_bytes());
    Pcsx2Pacing::parse(&payload).expect("valid readback")
}

#[test]
fn pacing_readback_maps_limiter_modes() {
    let limited = pacing(LIMITER_NOMINAL, 5_000, 0.5);
    assert_eq!(limited.frame, 321);
    assert_eq!(limited.public()["mode"], "limited");
    assert_eq!(limited.public()["percent"], 50);
    assert_eq!(limited.public()["policy_revision"], "4");
    assert_eq!(
        pacing(LIMITER_UNLIMITED, 10_000, 0.0).public()["mode"],
        "unlimited"
    );
    // Turbo, slow motion, a fractional nominal and a fast-boot override are custom.
    assert_eq!(pacing(1, 10_000, 2.0).public()["mode"], "custom");
    assert_eq!(
        pacing(LIMITER_NOMINAL, 3_333, 0.3333).public()["mode"],
        "custom"
    );
    assert_eq!(
        pacing(LIMITER_NOMINAL, 10_000, 0.0).public()["mode"],
        "custom"
    );
    // The wire rounds nominal to hundredths of a percent; compare target in that domain.
    for (percent, target) in [(1, 0.01), (137, 1.37), (10_000, 100.0)] {
        assert_eq!(
            pacing(LIMITER_NOMINAL, percent * 100, target).public()["percent"],
            percent
        );
    }
    for (mode, target) in [
        (LIMITER_NOMINAL, 1.0),
        (LIMITER_NOMINAL, f32::INFINITY),
        (LIMITER_NOMINAL, f32::NAN),
        (LIMITER_UNLIMITED, 0.5),
    ] {
        assert_eq!(pacing(mode, 5_000, target).public()["mode"], "custom");
    }
    assert!(Pcsx2Pacing::parse(&[0; 27]).is_none());
}

#[test]
fn advertised_capabilities_satisfy_the_common_contract() {
    let batch =
        serde_json::to_value(Pcsx2Bridge::<FakeTransport>::memory_batch_capability()).unwrap();
    MemoryBatchCapability::from_hello(&batch, &["ee".into()]).expect("batch capability validates");
    let speed =
        serde_json::to_value(Pcsx2Bridge::<FakeTransport>::execution_speed_capability()).unwrap();
    ExecutionSpeedCapability::from_hello(&speed).expect("speed capability validates");
}

struct FakeTransport;

impl PineTransport for FakeTransport {
    fn transact(&mut self, _request: &[u8]) -> BridgeResult<Vec<u8>> {
        unreachable!("capabilities are static")
    }
}
