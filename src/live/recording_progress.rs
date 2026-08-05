use super::link::{LinkError, WorkingProgress, WorkingProgressPhase};
use crate::bundle::recording_manifest::RecordingLimits;

pub(super) struct ProgressState<'a> {
    capture_id: &'a str,
    limits: &'a RecordingLimits,
    next_sequence: u64,
    last_frame: Option<u64>,
    last_completed_frames: Option<u64>,
    last_events: u64,
    last_bytes: u64,
    pub(super) first: bool,
    requested_frames: u64,
    require_explicit_frames: bool,
    last_phase: Option<WorkingProgressPhase>,
}

impl<'a> ProgressState<'a> {
    pub(super) fn new(
        capture_id: &'a str,
        limits: &'a RecordingLimits,
        requested_frames: u64,
        require_explicit_frames: bool,
    ) -> Self {
        Self {
            capture_id,
            limits,
            next_sequence: 0,
            last_frame: None,
            last_completed_frames: None,
            last_events: 0,
            last_bytes: 0,
            first: true,
            requested_frames,
            require_explicit_frames,
            last_phase: None,
        }
    }

    pub(super) fn validate(&mut self, progress: &WorkingProgress) -> Result<(), LinkError> {
        if progress.capture_id != self.capture_id
            || progress.sequence != self.next_sequence
            || self
                .last_frame
                .is_some_and(|previous| progress.frame < previous)
            || self.require_explicit_frames && progress.frames.is_none()
            || self.last_completed_frames.is_some() && progress.frames.is_none()
            || progress
                .frames
                .is_some_and(|frames| frames > self.requested_frames)
            || matches!(
                (self.last_completed_frames, progress.frames),
                (Some(previous), Some(current)) if current < previous
            )
            || progress.events < self.last_events
            || progress.bytes < self.last_bytes
            || progress.events > self.limits.max_events
            || progress.bytes > self.limits.max_bytes
            || !phase_transition_is_valid(self.last_phase, progress.phase)
        {
            return Err(LinkError::Protocol(
                "recording progress identity, sequence, or bounds mismatch".into(),
            ));
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.last_frame = Some(progress.frame);
        if progress.frames.is_some() {
            self.last_completed_frames = progress.frames;
        }
        self.last_events = progress.events;
        self.last_bytes = progress.bytes;
        self.last_phase = progress.phase;
        Ok(())
    }
}

fn phase_transition_is_valid(
    previous: Option<WorkingProgressPhase>,
    current: Option<WorkingProgressPhase>,
) -> bool {
    use WorkingProgressPhase::{Aligning, Recording, Warming};
    matches!(
        (previous, current),
        (None, _)
            | (Some(Warming), Some(Warming | Aligning | Recording))
            | (Some(Aligning), Some(Aligning | Recording))
            | (Some(Recording), Some(Recording))
    )
}
