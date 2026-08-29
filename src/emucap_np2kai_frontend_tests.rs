use super::*;
use std::time::{Duration, Instant};

#[test]
fn observation_while_running_preserves_the_pending_frame_deadline() {
    let now = Instant::now();
    let pending = now + Duration::from_millis(3);

    assert_eq!(
        deadline_after_command(pending, true, true, now, Duration::from_millis(20)),
        pending
    );
}

#[test]
fn resume_starts_one_new_frame_interval() {
    let now = Instant::now();
    let stale = now - Duration::from_millis(100);
    let frame_duration = Duration::from_millis(20);

    assert_eq!(
        deadline_after_command(stale, false, true, now, frame_duration),
        now + frame_duration
    );
}

#[test]
fn pausing_does_not_rewrite_the_unused_deadline() {
    let now = Instant::now();
    let pending = now + Duration::from_millis(3);

    assert_eq!(
        deadline_after_command(pending, true, false, now, Duration::from_millis(20)),
        pending
    );
}
