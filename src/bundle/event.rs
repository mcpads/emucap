use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::recording_manifest::{
    EventClassIdentity, EventStopCondition, EventStopFacts, FrameInterval, ObservationStartFacts,
    RecordingLimits,
};
use crate::event_contracts::{
    ClockOrder, EventContract, EventContractError, EventContractRegistry, FrameRelation,
    PayloadKind, PayloadValueType,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub sequence: u64,
    pub class: String,
    pub contract_sha256: String,
    pub clock: ClockPoint,
    pub frame: u64,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockPoint {
    pub domain: String,
    pub tick: u64,
}

#[derive(Debug, Clone)]
pub struct EventValidationSpec {
    pub f_start: u64,
    pub f_end: u64,
    pub observation_start: u64,
    pub class_intervals: BTreeMap<String, FrameInterval>,
    pub event_classes: Vec<EventClassIdentity>,
    pub limits: RecordingLimits,
    pub stop_on: Option<EventStopCondition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamCompleteness {
    Complete,
    Prefix,
    PartialFinalLine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStreamReport {
    pub sha256: String,
    pub physical_bytes: u64,
    pub complete_bytes: u64,
    pub records: u64,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
    pub frame_boundary_records: u64,
    pub frame_completed_records: u64,
    pub class_counts: BTreeMap<String, u64>,
    pub stop_event: Option<EventStopFacts>,
    pub completeness: StreamCompleteness,
}

#[derive(Debug, thiserror::Error)]
pub enum EventValidationError {
    #[error("event stream I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("event contract is invalid: {0}")]
    Contract(#[from] EventContractError),
    #[error("event line {line} exceeds limit {limit}")]
    LineLimit { line: u64, limit: u64 },
    #[error("event stream exceeds byte limit {0}")]
    ByteLimit(u64),
    #[error("event stream exceeds event limit {0}")]
    EventLimit(u64),
    #[error("event line {line} is invalid JSON: {source}")]
    Json {
        line: u64,
        source: serde_json::Error,
    },
    #[error("event sequence mismatch: expected {expected}, got {actual}")]
    Sequence { expected: u64, actual: u64 },
    #[error("event counter overflow")]
    CounterOverflow,
    #[error("event class was not selected: {0}")]
    UnselectedClass(String),
    #[error("event contract digest mismatch for {class}")]
    ContractDigest { class: String },
    #[error("event clock domain mismatch for {class}: expected {expected}, got {actual}")]
    ClockDomain {
        class: String,
        expected: String,
        actual: String,
    },
    #[error("event clock regressed in domain {domain}: previous {previous}, got {actual}")]
    ClockRegression {
        domain: String,
        previous: u64,
        actual: u64,
    },
    #[error("event frame {frame} is outside [{f_start},{f_end})")]
    FrameScope {
        frame: u64,
        f_start: u64,
        f_end: u64,
    },
    #[error("event frame/tick relation failed for {class}: frame {frame}, tick {tick}")]
    FrameRelation {
        class: String,
        frame: u64,
        tick: u64,
    },
    #[error("frame_boundary order mismatch: expected frame {expected}, got {actual}")]
    FrameBoundaryOrder { expected: u64, actual: u64 },
    #[error("frame_completed order mismatch: expected frame {expected}, got {actual}")]
    FrameCompletedOrder { expected: u64, actual: u64 },
    #[error("frame event order is invalid for frame {0}")]
    FrameEventOrder(u64),
    #[error("event stream continued after the requested stop event")]
    EventAfterStop,
    #[error("event stop condition is invalid: {0}")]
    StopCondition(String),
    #[error("event payload does not satisfy contract for {0}")]
    Payload(String),
    #[error("an observation event preceded the declared event-aligned start")]
    EventBeforeObservationStart,
    #[error("the declared event-aligned start does not match the event stream")]
    ObservationStartMismatch,
}

fn next_u64(value: u64) -> Result<u64, EventValidationError> {
    value
        .checked_add(1)
        .ok_or(EventValidationError::CounterOverflow)
}

fn payload_value<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(payload, |value, part| value.get(part))
}

fn payload_leaf_paths(value: &Value, prefix: &str, paths: &mut BTreeSet<String>) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    for (name, value) in object {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if value.is_object() {
            if !payload_leaf_paths(value, &path, paths) {
                return false;
            }
        } else if !paths.insert(path) {
            return false;
        }
    }
    true
}

fn validate_payload(contract: &EventContract, payload: &Value) -> bool {
    match contract.payload_kind {
        PayloadKind::EmptyObject => payload.as_object().is_some_and(|value| value.is_empty()),
        PayloadKind::Object => {
            let mut actual = BTreeSet::new();
            if !payload_leaf_paths(payload, "", &mut actual) {
                return false;
            }
            let expected: BTreeSet<_> = contract
                .payload_fields
                .iter()
                .map(|field| field.path.clone())
                .collect();
            if actual != expected {
                return false;
            }
            contract.payload_fields.iter().all(|field| {
                let Some(value) = payload_value(payload, &field.path) else {
                    return false;
                };
                match field.value_type {
                    PayloadValueType::U64 => value.as_u64().is_some_and(|number| {
                        number >= field.min.unwrap_or(0) && number <= field.max.unwrap_or(u64::MAX)
                    }),
                    PayloadValueType::Bool => value.is_boolean(),
                }
            })
        }
    }
}

fn update_total(total: &mut u64, amount: usize, limit: u64) -> Result<(), EventValidationError> {
    *total = total
        .checked_add(u64::try_from(amount).map_err(|_| EventValidationError::CounterOverflow)?)
        .ok_or(EventValidationError::CounterOverflow)?;
    if *total > limit {
        return Err(EventValidationError::ByteLimit(limit));
    }
    Ok(())
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    line_number: u64,
    max_line_bytes: u64,
    max_bytes: u64,
    physical_bytes: &mut u64,
    hasher: &mut Sha256,
) -> Result<Option<(Vec<u8>, bool)>, EventValidationError> {
    let mut line = Vec::with_capacity(256);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some((line, false)))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        let new_line_len = u64::try_from(line.len())
            .ok()
            .and_then(|length| length.checked_add(u64::try_from(take).ok()?))
            .ok_or(EventValidationError::CounterOverflow)?;
        if new_line_len > max_line_bytes {
            return Err(EventValidationError::LineLimit {
                line: line_number,
                limit: max_line_bytes,
            });
        }
        update_total(physical_bytes, take, max_bytes)?;
        hasher.update(&available[..take]);
        line.extend_from_slice(&available[..take]);
        let complete = available[take - 1] == b'\n';
        reader.consume(take);
        if complete {
            return Ok(Some((line, true)));
        }
    }
}

pub fn validate_event_stream(
    path: &Path,
    registry: &EventContractRegistry,
    spec: &EventValidationSpec,
) -> Result<EventStreamReport, EventValidationError> {
    validate_event_stream_at_start(path, registry, spec, None, &BTreeSet::new())
}

pub(crate) fn validate_event_stream_at_start(
    path: &Path,
    registry: &EventContractRegistry,
    spec: &EventValidationSpec,
    observation_anchor: Option<&ObservationStartFacts>,
    observation_classes: &BTreeSet<String>,
) -> Result<EventStreamReport, EventValidationError> {
    let selected: BTreeMap<_, _> = spec
        .event_classes
        .iter()
        .map(|identity| {
            registry
                .validate_identity(identity)
                .map(|contract| (identity.id.clone(), contract))
        })
        .collect::<Result<_, _>>()?;
    let selected_ids: BTreeSet<_> = selected.keys().cloned().collect();
    if selected_ids.is_empty() {
        return Err(EventValidationError::UnselectedClass("<none>".into()));
    }

    if let Some(stop_on) = &spec.stop_on {
        if stop_on.occurrence == 0 || !selected_ids.contains(&stop_on.event_class) {
            return Err(EventValidationError::StopCondition(
                "class must be selected and occurrence must be positive".into(),
            ));
        }
    }

    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(8192, file);
    let mut hasher = Sha256::new();
    let mut physical_bytes = 0_u64;
    let mut complete_bytes = 0_u64;
    let mut records = 0_u64;
    let mut expected_sequence = 0_u64;
    let mut first_sequence = None;
    let mut last_sequence = None;
    let mut clocks = BTreeMap::<String, u64>::new();
    let mut class_clocks = BTreeMap::<String, u64>::new();
    let mut class_counts = BTreeMap::<String, u64>::new();
    let mut observation_class_counts = BTreeMap::<String, u64>::new();
    let frame_boundary_interval = spec.class_intervals.get("frame_boundary");
    let frame_completed_interval = spec.class_intervals.get("frame_completed");
    let mut next_frame_boundary =
        frame_boundary_interval.map_or(spec.f_start, |scope| scope.f_start);
    let mut next_frame_completed =
        frame_completed_interval.map_or(spec.f_start, |scope| scope.f_start);
    let mut frame_boundary_records = 0_u64;
    let mut frame_completed_records = 0_u64;
    let mut stop_event = None;
    let mut observation_start_matched = observation_anchor.is_none();
    let mut completeness = StreamCompleteness::Prefix;
    let mut line_number = 1_u64;

    while let Some((line, newline)) = read_bounded_line(
        &mut reader,
        line_number,
        spec.limits.max_line_bytes,
        spec.limits.max_bytes,
        &mut physical_bytes,
        &mut hasher,
    )? {
        if stop_event.is_some() {
            return Err(EventValidationError::EventAfterStop);
        }
        if !newline {
            completeness = StreamCompleteness::PartialFinalLine;
            break;
        }
        let event: EventEnvelope =
            serde_json::from_slice(&line).map_err(|source| EventValidationError::Json {
                line: line_number,
                source,
            })?;
        if let Some(start) = observation_anchor {
            if event.sequence < start.sequence && observation_classes.contains(&event.class) {
                return Err(EventValidationError::EventBeforeObservationStart);
            }
            if event.sequence == start.sequence {
                if event.class != start.event_class
                    || event.contract_sha256 != start.contract_sha256
                    || event.frame != start.frame
                    || event.clock.domain != start.clock_domain
                    || event.clock.tick != start.clock_tick
                    || !observation_classes.contains(&event.class)
                {
                    return Err(EventValidationError::ObservationStartMismatch);
                }
                observation_start_matched = true;
            } else if event.sequence > start.sequence && !observation_start_matched {
                return Err(EventValidationError::ObservationStartMismatch);
            }
        }
        if event.sequence != expected_sequence {
            return Err(EventValidationError::Sequence {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }
        expected_sequence = next_u64(expected_sequence)?;
        records = next_u64(records)?;
        if records > spec.limits.max_events {
            return Err(EventValidationError::EventLimit(spec.limits.max_events));
        }
        complete_bytes = complete_bytes
            .checked_add(
                u64::try_from(line.len()).map_err(|_| EventValidationError::CounterOverflow)?,
            )
            .ok_or(EventValidationError::CounterOverflow)?;
        first_sequence.get_or_insert(event.sequence);
        last_sequence = Some(event.sequence);

        let contract = selected
            .get(&event.class)
            .ok_or_else(|| EventValidationError::UnselectedClass(event.class.clone()))?;
        if event.contract_sha256 != contract.contract_sha256 {
            return Err(EventValidationError::ContractDigest { class: event.class });
        }
        if event.clock.domain != contract.clock_domain {
            return Err(EventValidationError::ClockDomain {
                class: event.class,
                expected: contract.clock_domain.clone(),
                actual: event.clock.domain,
            });
        }
        let interval = spec.class_intervals.get(&event.class);
        let class_f_start = interval.map_or(spec.f_start, |scope| scope.f_start);
        let class_f_end = interval.map_or(spec.f_end, |scope| scope.f_end);
        if event.frame < class_f_start || event.frame >= class_f_end {
            return Err(EventValidationError::FrameScope {
                frame: event.frame,
                f_start: class_f_start,
                f_end: class_f_end,
            });
        }
        let relation_valid = match contract.frame_relation {
            FrameRelation::Tick => event.frame == event.clock.tick,
            FrameRelation::PreviousTick => event
                .frame
                .checked_add(1)
                .is_some_and(|tick| tick == event.clock.tick),
            FrameRelation::Independent => true,
        };
        if !relation_valid {
            return Err(EventValidationError::FrameRelation {
                class: event.class,
                frame: event.frame,
                tick: event.clock.tick,
            });
        }
        if let Some(previous) = clocks.insert(event.clock.domain.clone(), event.clock.tick) {
            if event.clock.tick < previous {
                return Err(EventValidationError::ClockRegression {
                    domain: event.clock.domain,
                    previous,
                    actual: event.clock.tick,
                });
            }
        }
        if let Some(previous) = class_clocks.insert(event.class.clone(), event.clock.tick) {
            let valid = match contract.clock_order {
                ClockOrder::Strict => event.clock.tick > previous,
                ClockOrder::Nondecreasing => event.clock.tick >= previous,
            };
            if !valid {
                return Err(EventValidationError::ClockRegression {
                    domain: format!("{}:{}", event.clock.domain, event.class),
                    previous,
                    actual: event.clock.tick,
                });
            }
        }
        if !validate_payload(contract, &event.payload) {
            return Err(EventValidationError::Payload(event.class));
        }
        if contract.id == "frame_boundary" {
            if event.frame != next_frame_boundary {
                return Err(EventValidationError::FrameBoundaryOrder {
                    expected: next_frame_boundary,
                    actual: event.frame,
                });
            }
            if selected_ids.contains("frame_completed")
                && event.frame > class_f_start
                && next_frame_completed < event.frame
            {
                return Err(EventValidationError::FrameEventOrder(event.frame));
            }
            next_frame_boundary = next_u64(next_frame_boundary)?;
            frame_boundary_records = next_u64(frame_boundary_records)?;
        } else if contract.id == "frame_completed" {
            if event.frame != next_frame_completed {
                return Err(EventValidationError::FrameCompletedOrder {
                    expected: next_frame_completed,
                    actual: event.frame,
                });
            }
            if next_frame_boundary <= event.frame {
                return Err(EventValidationError::FrameEventOrder(event.frame));
            }
            next_frame_completed = next_u64(next_frame_completed)?;
            frame_completed_records = next_u64(frame_completed_records)?;
        }
        let occurrence = class_counts.entry(event.class.clone()).or_insert(0);
        *occurrence = next_u64(*occurrence)?;
        let observation_started = observation_anchor
            .map_or(event.frame >= spec.observation_start, |start| {
                event.sequence >= start.sequence
            });
        let stop_occurrence = if observation_started {
            let count = observation_class_counts
                .entry(event.class.clone())
                .or_insert(0);
            *count = next_u64(*count)?;
            Some(*count)
        } else {
            None
        };
        if stop_occurrence.is_some_and(|occurrence| {
            spec.stop_on.as_ref().is_some_and(|condition| {
                condition.event_class == event.class && condition.occurrence == occurrence
            })
        }) {
            let occurrence = stop_occurrence.expect("matched observation occurrence");
            stop_event = Some(EventStopFacts {
                sequence: event.sequence,
                event_class: event.class,
                clock_domain: event.clock.domain,
                clock_tick: event.clock.tick,
                frame: event.frame,
                occurrence,
            });
        }
        line_number = next_u64(line_number)?;
    }

    if !observation_start_matched {
        return Err(EventValidationError::ObservationStartMismatch);
    }

    if completeness != StreamCompleteness::PartialFinalLine
        && selected_ids.contains("frame_boundary")
        && next_frame_boundary == frame_boundary_interval.map_or(spec.f_end, |scope| scope.f_end)
        && (!selected_ids.contains("frame_completed")
            || next_frame_completed
                == frame_completed_interval.map_or(spec.f_end, |scope| scope.f_end))
    {
        completeness = StreamCompleteness::Complete;
    }

    Ok(EventStreamReport {
        sha256: hex::encode(hasher.finalize()),
        physical_bytes,
        complete_bytes,
        records,
        first_sequence,
        last_sequence,
        frame_boundary_records,
        frame_completed_records,
        class_counts,
        stop_event,
        completeness,
    })
}
