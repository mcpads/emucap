use std::path::Path;

use serde::{Deserialize, Serialize};

use super::event::{
    validate_event_stream_at_start, EventStreamReport, EventValidationError, EventValidationSpec,
    StreamCompleteness,
};
use super::recording_manifest::{
    CleanupFacts, CleanupState, EventArmingScope, EventClassTerminalFacts, EventStopFacts,
    ExecutionOutcome, FinalExecutionState, FrameInterval, Integrity, LossFacts,
    ObservationStartFacts, OperationOutcome, RecordingCounters, RecordingOrigin, RecordingRequest,
};
use crate::event_contracts::{EventContractError, EventContractRegistry};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerTerminalReport {
    pub operation_outcome: OperationOutcome,
    pub execution_outcome: ExecutionOutcome,
    pub claimed_integrity: Integrity,
    pub final_execution_state: FinalExecutionState,
    pub final_frame: u64,
    #[serde(default)]
    pub f_origin: Option<u64>,
    pub counters: RecordingCounters,
    pub loss: LossFacts,
    pub cleanup: CleanupFacts,
    #[serde(default)]
    pub stop_event: Option<EventStopFacts>,
    pub reason: Option<String>,
    #[serde(default)]
    pub event_classes: Vec<EventClassTerminalFacts>,
}

#[derive(Debug, Clone)]
pub struct RecordingValidationInput {
    pub request: RecordingRequest,
    pub origin: RecordingOrigin,
    pub f_start: u64,
    pub f_end: u64,
    pub observation_start: Option<ObservationStartFacts>,
    pub terminal: ProducerTerminalReport,
}

#[derive(Debug, Clone)]
pub struct ValidatedRecording {
    stream: EventStreamReport,
    integrity: Integrity,
    terminal: ProducerTerminalReport,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingValidationError {
    #[error("recording event stream is invalid: {0}")]
    Event(#[from] EventValidationError),
    #[error("recording event contract is invalid: {0}")]
    Contract(#[from] EventContractError),
    #[error("recording frame count must be in 1..={0}")]
    FrameLimit(u64),
    #[error("recording frame interval overflow")]
    FrameOverflow,
    #[error("recording scope mismatch: expected end {expected}, got {actual}")]
    Scope { expected: u64, actual: u64 },
    #[error("recording complete terminal is inconsistent: {0}")]
    InconsistentComplete(String),
    #[error("recording stop-event facts do not match the validated stream")]
    StopEventMismatch,
    #[error("recording event-class terminal facts are inconsistent")]
    EventClassFacts,
    #[error("recording observation-start facts are inconsistent")]
    ObservationStartFacts,
}

fn cleanup_complete(cleanup: &CleanupFacts) -> bool {
    matches!(
        cleanup.hooks,
        CleanupState::Released | CleanupState::NotAcquired | CleanupState::GenerationTerminated
    ) && matches!(
        cleanup.transient_input,
        CleanupState::Released | CleanupState::NotAcquired | CleanupState::GenerationTerminated
    ) && matches!(
        cleanup.sink,
        CleanupState::Released | CleanupState::GenerationTerminated
    )
}

pub fn validate_recording(
    events_path: &Path,
    registry: &EventContractRegistry,
    input: RecordingValidationInput,
) -> Result<ValidatedRecording, RecordingValidationError> {
    let total_frames = input
        .request
        .warmup_frames
        .checked_add(input.request.frames)
        .ok_or(RecordingValidationError::FrameOverflow)?;
    if input.request.frames == 0 || total_frames > input.request.limits.max_frames {
        return Err(RecordingValidationError::FrameLimit(
            input.request.limits.max_frames,
        ));
    }
    let f_origin = input.terminal.f_origin.unwrap_or(input.f_start);
    let expected_f_start = f_origin
        .checked_add(input.request.warmup_frames)
        .ok_or(RecordingValidationError::FrameOverflow)?;
    let maximum_end = input
        .f_start
        .checked_add(input.request.frames)
        .ok_or(RecordingValidationError::FrameOverflow)?;
    let requested_ids: std::collections::BTreeSet<_> = input
        .request
        .event_classes
        .iter()
        .map(|identity| identity.id.as_str())
        .collect();
    let arming_by_id: std::collections::BTreeMap<_, _> = input
        .request
        .event_arming
        .iter()
        .map(|arming| (arming.id.as_str(), arming.scope))
        .collect();
    if requested_ids.len() != input.request.event_classes.len()
        || expected_f_start != input.f_start
        || input.f_end < f_origin
        || (input.request.warmup_frames == 0
            && input.request.start_on.is_none()
            && (!input.request.event_arming.is_empty()
                || input
                    .terminal
                    .f_origin
                    .is_some_and(|origin| origin != input.f_start)))
        || ((input.request.warmup_frames > 0 || input.request.start_on.is_some())
            && (input.terminal.f_origin.is_none()
                || input.terminal.event_classes.is_empty()
                || input.request.event_arming.len() != requested_ids.len()
                || arming_by_id.len() != requested_ids.len()
                || !requested_ids.iter().all(|id| arming_by_id.contains_key(id))))
    {
        return Err(RecordingValidationError::EventClassFacts);
    }
    let actual_frames = input.f_end.saturating_sub(input.f_start);
    if input.f_end > maximum_end {
        return Err(RecordingValidationError::Scope {
            expected: maximum_end,
            actual: input.f_end,
        });
    }
    for identity in &input.request.event_classes {
        registry.validate_identity(identity)?;
    }
    match (&input.request.start_on, &input.observation_start) {
        (None, None) if input.request.initial_snapshots.is_empty() => {}
        (Some(requested), Some(observed))
            if requested.event_class == observed.event_class
                && input.f_start == observed.frame
                && input.request.event_classes.iter().any(|identity| {
                    identity.id == observed.event_class
                        && identity.contract_sha256 == observed.contract_sha256
                }) => {}
        _ => return Err(RecordingValidationError::ObservationStartFacts),
    }
    let class_intervals = input
        .request
        .event_classes
        .iter()
        .map(|identity| {
            let scope = arming_by_id
                .get(identity.id.as_str())
                .copied()
                .unwrap_or(EventArmingScope::Observation);
            let start = match scope {
                EventArmingScope::Transaction => f_origin,
                EventArmingScope::Observation => input.f_start,
            };
            (
                identity.id.clone(),
                FrameInterval {
                    f_start: start,
                    f_end: input.f_end.max(start),
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let observation_classes = arming_by_id
        .iter()
        .filter(|(_, scope)| **scope == EventArmingScope::Observation)
        .map(|(id, _)| (*id).to_string())
        .collect();
    let stream = validate_event_stream_at_start(
        events_path,
        registry,
        &EventValidationSpec {
            f_start: f_origin,
            f_end: input.f_end,
            observation_start: input.f_start,
            class_intervals: class_intervals.clone(),
            event_classes: input.request.event_classes.clone(),
            limits: input.request.limits.clone(),
            stop_on: input.request.stop_on.clone(),
        },
        input.observation_start.as_ref(),
        &observation_classes,
    )?;

    if input.terminal.stop_event != stream.stop_event {
        return Err(RecordingValidationError::StopEventMismatch);
    }

    if !input.terminal.event_classes.is_empty() {
        let mut reported = std::collections::BTreeSet::new();
        let mut reported_dropped = 0u64;
        for facts in &input.terminal.event_classes {
            if !requested_ids.contains(facts.id.as_str())
                || !reported.insert(facts.id.as_str())
                || stream.class_counts.get(&facts.id).copied().unwrap_or(0) != facts.observed
                || facts.dropped > input.terminal.counters.dropped
            {
                return Err(RecordingValidationError::EventClassFacts);
            }
            if input.request.warmup_frames == 0 && input.request.start_on.is_none() {
                if facts.armed_interval.is_some() {
                    return Err(RecordingValidationError::EventClassFacts);
                }
            } else {
                let expected = class_intervals
                    .get(&facts.id)
                    .ok_or(RecordingValidationError::EventClassFacts)?;
                let observation_not_armed = arming_by_id.get(facts.id.as_str())
                    == Some(&EventArmingScope::Observation)
                    && input.f_end <= input.f_start
                    && !facts.armed;
                if observation_not_armed {
                    if facts.armed_interval.is_some() {
                        return Err(RecordingValidationError::EventClassFacts);
                    }
                } else if !facts.armed || facts.armed_interval.as_ref() != Some(expected) {
                    return Err(RecordingValidationError::EventClassFacts);
                }
            }
            reported_dropped = reported_dropped
                .checked_add(facts.dropped)
                .ok_or(RecordingValidationError::EventClassFacts)?;
        }
        if reported != requested_ids || reported_dropped != input.terminal.counters.dropped {
            return Err(RecordingValidationError::EventClassFacts);
        }
    }

    let counters_match = input.terminal.counters.events == stream.records
        && input.terminal.counters.bytes == stream.complete_bytes
        && input.terminal.counters.frames == actual_frames;
    let stop_matches = match input.terminal.execution_outcome {
        ExecutionOutcome::TargetReached => {
            input.f_end == maximum_end
                && input.terminal.stop_event.is_none()
                && stream.stop_event.is_none()
        }
        ExecutionOutcome::EventStop => {
            input.request.stop_on.is_some()
                && input.terminal.stop_event.is_some()
                && input.terminal.stop_event == stream.stop_event
                && stream
                    .stop_event
                    .as_ref()
                    .is_some_and(|event| event.clock_tick == input.f_end)
        }
        _ => input.terminal.stop_event.is_none(),
    };
    let complete = input.terminal.operation_outcome == OperationOutcome::Completed
        && matches!(
            input.terminal.execution_outcome,
            ExecutionOutcome::TargetReached | ExecutionOutcome::EventStop
        )
        && input.terminal.claimed_integrity == Integrity::Complete
        && input.terminal.final_frame == input.f_end
        && actual_frames > 0
        && input.terminal.counters.dropped == 0
        && input.terminal.loss.dropped == 0
        && !input.terminal.loss.truncated
        && input.terminal.loss.first_sequence_gap.is_none()
        && input
            .terminal
            .event_classes
            .iter()
            .all(|facts| facts.armed && facts.dropped == 0)
        && counters_match
        && stream.completeness == StreamCompleteness::Complete
        && stream.frame_boundary_records
            == class_intervals
                .get("frame_boundary")
                .map_or(actual_frames, |scope| scope.f_end - scope.f_start)
        && (!input
            .request
            .event_classes
            .iter()
            .any(|identity| identity.id == "frame_completed")
            || stream.frame_completed_records
                == class_intervals
                    .get("frame_completed")
                    .map_or(actual_frames, |scope| scope.f_end - scope.f_start))
        && stop_matches
        && cleanup_complete(&input.terminal.cleanup)
        && matches!(
            input.terminal.final_execution_state,
            FinalExecutionState::Frozen | FinalExecutionState::Terminated
        );

    if input.terminal.operation_outcome == OperationOutcome::Completed && !complete {
        return Err(RecordingValidationError::InconsistentComplete(
            "producer claimed completion without exact scope, counters, cleanup, or stream"
                .to_string(),
        ));
    }

    let integrity = if complete {
        Integrity::Complete
    } else if stream.completeness == StreamCompleteness::PartialFinalLine
        || !counters_match
        || input.terminal.claimed_integrity == Integrity::Unverifiable
        || input.terminal.loss.truncated
        || input.terminal.loss.first_sequence_gap.is_some()
        || !cleanup_complete(&input.terminal.cleanup)
    {
        Integrity::Unverifiable
    } else {
        Integrity::Lossy
    };

    Ok(ValidatedRecording {
        stream,
        integrity,
        terminal: input.terminal,
    })
}

impl ValidatedRecording {
    pub fn stream(&self) -> &EventStreamReport {
        &self.stream
    }

    pub fn integrity(&self) -> Integrity {
        self.integrity
    }

    pub fn terminal(&self) -> &ProducerTerminalReport {
        &self.terminal
    }
}
