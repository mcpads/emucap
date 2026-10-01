use std::time::{Duration, Instant};

use super::*;

const FRAME: Duration = Duration::from_millis(20);

fn limited(percent: u64) -> FramePacer {
    let mut pacer = FramePacer::new();
    pacer.set(PacingPolicy::Limited {
        centi_percent: percent * 100,
    });
    pacer
}

#[test]
fn frame_starts_follow_guest_frames_from_the_anchor_without_drift() {
    let now = Instant::now();
    let mut pacer = limited(50);
    assert_eq!(pacer.frame_start(100, FRAME, now), Some(now));
    // Late wakeups do not push later deadlines: frame 110 starts 10 half-speed periods later.
    let late = now + Duration::from_millis(45);
    assert_eq!(pacer.frame_start(110, FRAME, late), Some(now + FRAME * 20));
    let mut double = limited(200);
    double.frame_start(0, FRAME, now);
    assert_eq!(double.frame_start(4, FRAME, now), Some(now + FRAME * 2));
}

#[test]
fn unlimited_never_waits_and_changes_bump_the_revision() {
    let mut pacer = FramePacer::new();
    let revision = pacer.revision;
    pacer.set(PacingPolicy::Unlimited);
    assert_eq!(pacer.frame_start(7, FRAME, Instant::now()), None);
    assert_eq!(pacer.revision, revision + 1);
}

#[test]
fn falling_far_behind_reanchors_instead_of_bursting() {
    let now = Instant::now();
    let mut pacer = limited(100);
    pacer.frame_start(0, FRAME, now);
    // Frame 1 was due at +20 ms; a host stall to +500 ms must not replay 24 frames at once.
    let stalled = now + Duration::from_millis(500);
    assert_eq!(pacer.frame_start(1, FRAME, stalled), Some(stalled));
    assert_eq!(pacer.frame_start(2, FRAME, stalled), Some(stalled + FRAME));
}

#[test]
fn frozen_time_is_not_paced_debt_after_reanchor() {
    let now = Instant::now();
    let mut pacer = limited(100);
    pacer.frame_start(0, FRAME, now);
    pacer.reanchor();
    let resumed = now + Duration::from_secs(60);
    assert_eq!(pacer.frame_start(1, FRAME, resumed), Some(resumed));
}

#[test]
fn advertised_windows_are_exactly_the_native_peek_view() {
    let hello = json!({"memory_batch_capability": Np2kaiHost::memory_batch_capability(),
        "execution_speed_capability": Np2kaiHost::execution_speed_capability()});
    let methods = METHODS.iter().map(|m| m.to_string()).collect::<Vec<_>>();
    let types = debug::memory_type_names()
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>();
    let features =
        crate::live::link::FeatureCapabilities::from_hello(&hello, &methods, &types).unwrap();
    let batch = features.memory_batch.unwrap();
    assert!(batch
        .admit(&[BatchRange {
            memory_type: "ram".into(),
            address: 0xA3FFF,
            length: 1
        }])
        .is_ok());
    for (memory_type, address, admitted) in [
        ("ram", 0xA4000, false),
        ("ram", 0xA7FFF, false),
        ("ram", 0xA8000, true),
        ("ram", 0xC0000, false),
        ("ram", 0xE7FFF, true),
        ("ram", 0xE8000, false),
        ("gvram_b", 0, true),
        ("gvram_i", 0x7FFF, true),
        ("tvram", 0x3FFF, true),
    ] {
        let range = BatchRange {
            memory_type: memory_type.into(),
            address,
            length: 1,
        };
        assert_eq!(
            batch.admit(&[range]).is_ok(),
            admitted,
            "{memory_type} {address:#x}"
        );
    }
}

#[test]
fn policy_changes_and_short_parks_start_a_new_frame_schedule() {
    let initial = Instant::now();
    let mut pacer = limited(100);
    pacer.frame_start(100, FRAME, initial);
    // Native centi-percent domain: minimum, 1%, 100%, maximum.
    for (centi_percent, period) in [
        (1, Duration::from_secs(200)),
        (100, Duration::from_secs(2)),
        (10_000, FRAME),
        (1_000_000, Duration::from_micros(200)),
    ] {
        let changed = initial + Duration::from_millis(3);
        pacer.set(PacingPolicy::Limited { centi_percent });
        assert_eq!(pacer.frame_start(101, FRAME, changed), Some(changed));
        assert_eq!(pacer.frame_start(102, FRAME, changed), Some(changed + period));
        let policy = pacer.policy;
        let revision = pacer.revision;
        pacer.reanchor();
        let resumed = changed + Duration::from_millis(3);
        assert_eq!(pacer.frame_start(102, FRAME, resumed), Some(resumed));
        assert_eq!(pacer.frame_start(103, FRAME, resumed), Some(resumed + period));
        assert_eq!(pacer.policy, policy);
        assert_eq!(pacer.revision, revision);
    }
    pacer.set(PacingPolicy::Unlimited);
    pacer.reanchor();
    assert_eq!(pacer.frame_start(104, FRAME, initial), None);
}
