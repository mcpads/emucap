use serde::Deserialize;
use serde_json::Value;

use super::recording::RecordingError;
use crate::bundle::recording::{ProducerTerminalReport, RecordingValidationInput};
use crate::bundle::recording_manifest::{
    CleanupFacts, EventClassTerminalFacts, EventStopFacts, ExecutionOutcome, FinalExecutionState,
    Integrity, LossFacts, ObservationStartFacts, OperationOutcome, RecordingCounters,
    RecordingOrigin, RecordingRequest,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTerminal {
    status: String,
    capture_id: String,
    operation_outcome: OperationOutcome,
    execution_outcome: ExecutionOutcome,
    integrity: Integrity,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    f_origin: Option<u64>,
    f_start: u64,
    f_end: u64,
    final_frame: u64,
    frames: u64,
    events: u64,
    bytes: u64,
    physical_bytes: u64,
    dropped: u64,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    first_sequence_gap: Option<u64>,
    wall_ms: u64,
    final_execution_state: FinalExecutionState,
    cleanup: CleanupFacts,
    #[serde(default)]
    stop_event: Option<EventStopFacts>,
    #[serde(default)]
    event_classes: Vec<EventClassTerminalFacts>,
    #[serde(default)]
    observation_start: Option<ObservationStartFacts>,
}

pub(crate) fn terminal_validation(
    capture_id: &str,
    origin: RecordingOrigin,
    request: &RecordingRequest,
    require_class_accounting: bool,
    value: Value,
) -> Result<RecordingValidationInput, RecordingError> {
    let terminal: WireTerminal = serde_json::from_value(value)
        .map_err(|error| RecordingError::Terminal(error.to_string()))?;
    let f_origin = terminal.f_origin.unwrap_or(terminal.f_start);
    let expected_f_start = f_origin
        .checked_add(request.warmup_frames)
        .ok_or_else(|| RecordingError::Terminal("terminal warmup scope overflow".into()))?;
    let maximum_f_end = terminal
        .f_start
        .checked_add(request.frames)
        .ok_or_else(|| RecordingError::Terminal("terminal frame scope overflow".into()))?;
    let actual_frames = terminal.f_end.saturating_sub(terminal.f_start);
    let scope_matches_outcome = match terminal.execution_outcome {
        ExecutionOutcome::TargetReached => terminal.f_end == maximum_f_end,
        ExecutionOutcome::EventStop => {
            request.stop_on.is_some()
                && actual_frames > 0
                && terminal.f_end <= maximum_f_end
                && terminal.stop_event.is_some()
        }
        _ => terminal.f_end <= maximum_f_end,
    };
    if terminal.capture_id != capture_id
        || !matches!(
            terminal.status.as_str(),
            "completed" | "failed" | "interrupted"
        )
        || terminal.f_end < f_origin
        || terminal.f_start != expected_f_start
        || (request.warmup_frames == 0
            && terminal
                .f_origin
                .is_some_and(|origin| origin != terminal.f_start))
        || (request.warmup_frames > 0 && terminal.f_origin.is_none())
        || terminal.frames != actual_frames
        || !scope_matches_outcome
        || terminal.events > request.limits.max_events
        || terminal.physical_bytes > request.limits.max_bytes
        || terminal.bytes > terminal.physical_bytes
        || terminal.wall_ms > request.limits.max_host_ms
        || (require_class_accounting && terminal.event_classes.is_empty())
        || (terminal.status == "completed")
            != (terminal.operation_outcome == OperationOutcome::Completed)
    {
        return Err(RecordingError::Terminal(
            "terminal identity, scope, counters, or bounds mismatch".into(),
        ));
    }
    Ok(RecordingValidationInput {
        request: request.clone(),
        origin,
        f_start: terminal.f_start,
        f_end: terminal.f_end,
        observation_start: terminal.observation_start,
        terminal: ProducerTerminalReport {
            operation_outcome: terminal.operation_outcome,
            execution_outcome: terminal.execution_outcome,
            claimed_integrity: terminal.integrity,
            final_execution_state: terminal.final_execution_state,
            final_frame: terminal.final_frame,
            f_origin: terminal.f_origin,
            counters: RecordingCounters {
                frames: terminal.frames,
                events: terminal.events,
                bytes: terminal.bytes,
                dropped: terminal.dropped,
            },
            loss: LossFacts {
                dropped: terminal.dropped,
                truncated: terminal.truncated,
                first_sequence_gap: terminal.first_sequence_gap,
            },
            cleanup: terminal.cleanup,
            stop_event: terminal.stop_event,
            reason: terminal.reason,
            event_classes: terminal.event_classes,
        },
    })
}
