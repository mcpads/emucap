use super::link::WorkingProgress;
use super::link::WorkingProgressPhase;
use super::recording_progress::ProgressState;
use crate::bundle::recording_manifest::RecordingLimits;

fn limits() -> RecordingLimits {
    RecordingLimits {
        max_frames: 10,
        max_events: 20,
        max_bytes: 1024,
        max_line_bytes: 512,
        max_host_ms: 1000,
        progress_interval_ms: 100,
    }
}

fn progress(sequence: u64, frames: Option<u64>, events: u64) -> WorkingProgress {
    WorkingProgress {
        status: "working".into(),
        capture_id: "capture-test".into(),
        sequence,
        frame: 100 + frames.unwrap_or(events),
        frames,
        events,
        bytes: events * 10,
        phase: None,
    }
}

#[test]
fn legacy_single_event_progress_may_omit_completed_frames() {
    let limits = limits();
    let mut state = ProgressState::new("capture-test", &limits, 10, false);
    state.validate(&progress(0, None, 0)).unwrap();
    state.validate(&progress(1, None, 1)).unwrap();
}

#[test]
fn extended_progress_requires_monotonic_completed_frames() {
    let limits = limits();
    let mut state = ProgressState::new("capture-test", &limits, 10, true);
    assert!(state.validate(&progress(0, None, 0)).is_err());

    let mut state = ProgressState::new("capture-test", &limits, 10, true);
    state.validate(&progress(0, Some(0), 1)).unwrap();
    state.validate(&progress(1, Some(1), 3)).unwrap();
    assert!(state.validate(&progress(2, Some(0), 4)).is_err());
}

#[test]
fn progress_cannot_drop_completed_frames_after_advertising_them() {
    let limits = limits();
    let mut state = ProgressState::new("capture-test", &limits, 10, false);
    state.validate(&progress(0, Some(0), 0)).unwrap();
    assert!(state.validate(&progress(1, None, 1)).is_err());
}

#[test]
fn alignment_progress_may_advance_but_cannot_regress() {
    let limits = limits();
    let mut state = ProgressState::new("capture-test", &limits, 10, true);
    let mut value = progress(0, Some(0), 1);
    value.phase = Some(WorkingProgressPhase::Warming);
    state.validate(&value).unwrap();
    value.sequence = 1;
    value.phase = Some(WorkingProgressPhase::Aligning);
    state.validate(&value).unwrap();
    value.sequence = 2;
    value.phase = Some(WorkingProgressPhase::Recording);
    state.validate(&value).unwrap();
    value.sequence = 3;
    value.phase = Some(WorkingProgressPhase::Aligning);
    assert!(state.validate(&value).is_err());
}
