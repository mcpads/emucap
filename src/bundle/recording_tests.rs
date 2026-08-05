use std::fs;

use serde_json::json;

use super::event::{ClockPoint, EventEnvelope};
use super::recording::*;
use super::recording_manifest::*;
use crate::event_contracts::EventContractRegistry;

fn request(frames: u64) -> RecordingRequest {
    RecordingRequest {
        frames,
        warmup_frames: 0,
        event_classes: EventContractRegistry::builtin()
            .unwrap()
            .identities(["frame_boundary"])
            .unwrap(),
        event_arming: vec![],
        limits: RecordingLimits {
            max_frames: 300,
            max_events: 1000,
            max_bytes: 1024 * 1024,
            max_line_bytes: 4096,
            max_host_ms: 30_000,
            progress_interval_ms: 100,
        },
        input_movie: None,
        stop_on: None,
        start_on: None,
        initial_snapshots: vec![],
        terminal_snapshots: vec![],
        terminal_state: None,
    }
}

fn stream(frames: u64, f_start: u64) -> Vec<u8> {
    let identity = request(frames).event_classes.remove(0);
    let mut output = Vec::new();
    for offset in 0..frames {
        serde_json::to_writer(
            &mut output,
            &EventEnvelope {
                sequence: offset,
                class: identity.id.clone(),
                contract_sha256: identity.contract_sha256.clone(),
                clock: ClockPoint {
                    domain: "frame".into(),
                    tick: f_start + offset,
                },
                frame: f_start + offset,
                payload: json!({}),
            },
        )
        .unwrap();
        output.push(b'\n');
    }
    output
}

fn event_stop_stream(frames: u64, f_start: u64) -> Vec<u8> {
    let registry = EventContractRegistry::builtin().unwrap();
    let boundary = registry.identities(["frame_boundary"]).unwrap().remove(0);
    let completed = registry.identities(["frame_completed"]).unwrap().remove(0);
    let mut output = Vec::new();
    let mut sequence = 0;
    for offset in 0..frames {
        for (identity, tick) in [
            (&boundary, f_start + offset),
            (&completed, f_start + offset + 1),
        ] {
            serde_json::to_writer(
                &mut output,
                &EventEnvelope {
                    sequence,
                    class: identity.id.clone(),
                    contract_sha256: identity.contract_sha256.clone(),
                    clock: ClockPoint {
                        domain: "frame".into(),
                        tick,
                    },
                    frame: f_start + offset,
                    payload: json!({}),
                },
            )
            .unwrap();
            output.push(b'\n');
            sequence += 1;
        }
    }
    output
}

fn cleanup() -> CleanupFacts {
    CleanupFacts {
        hooks: CleanupState::NotAcquired,
        transient_input: CleanupState::NotAcquired,
        sink: CleanupState::Released,
    }
}

fn terminal(frames: u64, bytes: u64, f_end: u64) -> ProducerTerminalReport {
    ProducerTerminalReport {
        operation_outcome: OperationOutcome::Completed,
        execution_outcome: ExecutionOutcome::TargetReached,
        claimed_integrity: Integrity::Complete,
        final_execution_state: FinalExecutionState::Frozen,
        final_frame: f_end,
        f_origin: None,
        counters: RecordingCounters {
            frames,
            events: frames,
            bytes,
            dropped: 0,
        },
        loss: LossFacts {
            dropped: 0,
            truncated: false,
            first_sequence_gap: None,
        },
        cleanup: cleanup(),
        stop_event: None,
        reason: None,
        event_classes: Vec::new(),
    }
}

#[test]
fn complete_integrity_is_derived_from_exact_stream_and_terminal_facts() {
    let raw = stream(3, 40);
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &raw).unwrap();
    let validated = validate_recording(
        file.path(),
        &EventContractRegistry::builtin().unwrap(),
        RecordingValidationInput {
            request: request(3),
            origin: RecordingOrigin::NextFrameBoundary,
            f_start: 40,
            f_end: 43,
            observation_start: None,
            terminal: terminal(3, raw.len() as u64, 43),
        },
    )
    .unwrap();
    assert_eq!(validated.integrity(), Integrity::Complete);
    assert_eq!(validated.stream().records, 3);
}

#[test]
fn event_aligned_start_is_bound_to_the_exact_first_observation_envelope() {
    let registry = EventContractRegistry::builtin().unwrap();
    let boundary = registry.identities(["frame_boundary"]).unwrap().remove(0);
    let instruction = registry
        .identities(["snes_cpu_instruction"])
        .unwrap()
        .remove(0);
    let events = [
        EventEnvelope {
            sequence: 0,
            class: boundary.id.clone(),
            contract_sha256: boundary.contract_sha256.clone(),
            clock: ClockPoint {
                domain: "frame".into(),
                tick: 40,
            },
            frame: 40,
            payload: json!({}),
        },
        EventEnvelope {
            sequence: 1,
            class: instruction.id.clone(),
            contract_sha256: instruction.contract_sha256.clone(),
            clock: ClockPoint {
                domain: "snes_master".into(),
                tick: 4567,
            },
            frame: 40,
            payload: json!({
                "pc": 0x808000_u64, "opcode": 0xea_u64, "a": 1_u64, "x": 2_u64,
                "y": 3_u64, "sp": 0x1ff_u64, "d": 0_u64, "dbr": 0x7e_u64,
                "k": 0x80_u64, "ps": 0x34_u64, "emulation": false,
                "cpu_cycle": 1234_u64,
            }),
        },
    ];
    let mut raw = Vec::new();
    for event in &events {
        serde_json::to_writer(&mut raw, event).unwrap();
        raw.push(b'\n');
    }
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &raw).unwrap();
    let interval = Some(FrameInterval {
        f_start: 40,
        f_end: 41,
    });
    let mut requested = request(1);
    requested.event_classes = vec![boundary, instruction.clone()];
    requested.event_arming = vec![
        EventClassArming {
            id: "frame_boundary".into(),
            scope: EventArmingScope::Transaction,
        },
        EventClassArming {
            id: instruction.id.clone(),
            scope: EventArmingScope::Observation,
        },
    ];
    requested.start_on = Some(EventStartCondition {
        event_class: instruction.id.clone(),
    });
    let start = ObservationStartFacts {
        sequence: 1,
        event_class: instruction.id,
        contract_sha256: instruction.contract_sha256,
        frame: 40,
        clock_domain: "snes_master".into(),
        clock_tick: 4567,
    };
    let mut producer = terminal(1, raw.len() as u64, 41);
    producer.f_origin = Some(40);
    producer.counters.events = 2;
    producer.cleanup.hooks = CleanupState::Released;
    producer.event_classes = vec![
        EventClassTerminalFacts {
            id: "frame_boundary".into(),
            armed: true,
            armed_interval: interval.clone(),
            observed: 1,
            dropped: 0,
        },
        EventClassTerminalFacts {
            id: "snes_cpu_instruction".into(),
            armed: true,
            armed_interval: interval,
            observed: 1,
            dropped: 0,
        },
    ];

    let validated = validate_recording(
        file.path(),
        &registry,
        RecordingValidationInput {
            request: requested.clone(),
            origin: RecordingOrigin::NextFrameBoundary,
            f_start: 40,
            f_end: 41,
            observation_start: Some(start.clone()),
            terminal: producer.clone(),
        },
    )
    .unwrap();
    assert_eq!(validated.integrity(), Integrity::Complete);

    let mut wrong = start;
    wrong.clock_tick += 1;
    assert!(matches!(
        validate_recording(
            file.path(),
            &registry,
            RecordingValidationInput {
                request: requested,
                origin: RecordingOrigin::NextFrameBoundary,
                f_start: 40,
                f_end: 41,
                observation_start: Some(wrong),
                terminal: producer,
            },
        ),
        Err(RecordingValidationError::Event(_))
    ));
}

#[test]
fn class_accounting_must_cover_the_exact_selected_set_and_stream_counts() {
    let raw = stream(1, 40);
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &raw).unwrap();
    let registry = EventContractRegistry::builtin().unwrap();
    let input = |facts: EventClassTerminalFacts| RecordingValidationInput {
        request: request(1),
        origin: RecordingOrigin::NextFrameBoundary,
        f_start: 40,
        f_end: 41,
        observation_start: None,
        terminal: ProducerTerminalReport {
            event_classes: vec![facts],
            ..terminal(1, raw.len() as u64, 41)
        },
    };

    let validated = validate_recording(
        file.path(),
        &registry,
        input(EventClassTerminalFacts {
            id: "frame_boundary".into(),
            armed: true,
            armed_interval: None,
            observed: 1,
            dropped: 0,
        }),
    )
    .unwrap();
    assert_eq!(validated.integrity(), Integrity::Complete);

    for facts in [
        EventClassTerminalFacts {
            id: "frame_boundary".into(),
            armed: true,
            armed_interval: None,
            observed: 0,
            dropped: 0,
        },
        EventClassTerminalFacts {
            id: "unselected".into(),
            armed: true,
            armed_interval: None,
            observed: 1,
            dropped: 0,
        },
        EventClassTerminalFacts {
            id: "frame_boundary".into(),
            armed: true,
            armed_interval: None,
            observed: 1,
            dropped: 1,
        },
    ] {
        assert!(matches!(
            validate_recording(file.path(), &registry, input(facts)),
            Err(RecordingValidationError::EventClassFacts)
        ));
    }

    let unarmed = validate_recording(
        file.path(),
        &registry,
        input(EventClassTerminalFacts {
            id: "frame_boundary".into(),
            armed: false,
            armed_interval: None,
            observed: 1,
            dropped: 0,
        }),
    );
    assert!(matches!(
        unarmed,
        Err(RecordingValidationError::InconsistentComplete(_))
    ));
}

#[test]
fn explicit_loss_with_a_valid_prefix_is_lossy() {
    let raw = stream(2, 40);
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &raw).unwrap();
    let mut terminal = terminal(3, raw.len() as u64, 42);
    terminal.operation_outcome = OperationOutcome::Failed;
    terminal.execution_outcome = ExecutionOutcome::LossDetected;
    terminal.claimed_integrity = Integrity::Lossy;
    terminal.counters.events = 2;
    terminal.counters.dropped = 1;
    terminal.loss.dropped = 1;
    let validated = validate_recording(
        file.path(),
        &EventContractRegistry::builtin().unwrap(),
        RecordingValidationInput {
            request: request(3),
            origin: RecordingOrigin::NextFrameBoundary,
            f_start: 40,
            f_end: 43,
            observation_start: None,
            terminal,
        },
    )
    .unwrap();
    assert_eq!(validated.integrity(), Integrity::Lossy);
}

#[test]
fn partial_line_and_counter_ambiguity_are_unverifiable() {
    let mut raw = stream(1, 40);
    raw.extend_from_slice(br#"{"sequence":1"#);
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &raw).unwrap();
    let mut terminal = terminal(2, raw.len() as u64, 41);
    terminal.operation_outcome = OperationOutcome::Failed;
    terminal.execution_outcome = ExecutionOutcome::AdapterError;
    terminal.claimed_integrity = Integrity::Unverifiable;
    terminal.counters.events = 1;
    terminal.loss.truncated = true;
    let validated = validate_recording(
        file.path(),
        &EventContractRegistry::builtin().unwrap(),
        RecordingValidationInput {
            request: request(2),
            origin: RecordingOrigin::NextFrameBoundary,
            f_start: 40,
            f_end: 42,
            observation_start: None,
            terminal,
        },
    )
    .unwrap();
    assert_eq!(validated.integrity(), Integrity::Unverifiable);
}

#[test]
fn producer_cannot_claim_complete_with_missing_records_or_cleanup() {
    let raw = stream(1, 40);
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &raw).unwrap();
    let mut claimed = terminal(2, raw.len() as u64, 42);
    claimed.counters.events = 1;
    claimed.cleanup.sink = CleanupState::Unverifiable;
    assert!(matches!(
        validate_recording(
            file.path(),
            &EventContractRegistry::builtin().unwrap(),
            RecordingValidationInput {
                request: request(2),
                origin: RecordingOrigin::NextFrameBoundary,
                f_start: 40,
                f_end: 42,
                observation_start: None,
                terminal: claimed,
            }
        ),
        Err(RecordingValidationError::InconsistentComplete(_))
    ));
}

#[test]
fn scope_arithmetic_fails_closed_on_overflow() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let error = validate_recording(
        file.path(),
        &EventContractRegistry::builtin().unwrap(),
        RecordingValidationInput {
            request: request(2),
            origin: RecordingOrigin::NextFrameBoundary,
            f_start: u64::MAX,
            f_end: 0,
            observation_start: None,
            terminal: terminal(2, 0, 0),
        },
    )
    .unwrap_err();
    assert!(matches!(error, RecordingValidationError::FrameOverflow));
}

#[test]
fn empty_failed_capture_can_be_preserved_as_lossy_prefix() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut terminal = terminal(1, 0, 40);
    terminal.operation_outcome = OperationOutcome::Aborted;
    terminal.execution_outcome = ExecutionOutcome::LossDetected;
    terminal.claimed_integrity = Integrity::Lossy;
    terminal.counters.events = 0;
    terminal.counters.dropped = 1;
    terminal.loss.dropped = 1;
    let validated = validate_recording(
        file.path(),
        &EventContractRegistry::builtin().unwrap(),
        RecordingValidationInput {
            request: request(1),
            origin: RecordingOrigin::NextFrameBoundary,
            f_start: 40,
            f_end: 41,
            observation_start: None,
            terminal,
        },
    )
    .unwrap();
    assert_eq!(validated.integrity(), Integrity::Lossy);
}

#[test]
fn event_stop_derives_complete_actual_scope_from_the_final_event() {
    let raw = event_stop_stream(2, 40);
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &raw).unwrap();
    let mut request = request(5);
    request.event_classes = EventContractRegistry::builtin()
        .unwrap()
        .identities(["frame_boundary", "frame_completed"])
        .unwrap();
    request.stop_on = Some(EventStopCondition {
        event_class: "frame_completed".into(),
        occurrence: 2,
    });
    let stop_event = EventStopFacts {
        sequence: 3,
        event_class: "frame_completed".into(),
        clock_domain: "frame".into(),
        clock_tick: 42,
        frame: 41,
        occurrence: 2,
    };
    let mut terminal = terminal(2, raw.len() as u64, 42);
    terminal.execution_outcome = ExecutionOutcome::EventStop;
    terminal.counters.events = 4;
    terminal.stop_event = Some(stop_event);
    let validated = validate_recording(
        file.path(),
        &EventContractRegistry::builtin().unwrap(),
        RecordingValidationInput {
            request,
            origin: RecordingOrigin::NextFrameBoundary,
            f_start: 40,
            f_end: 42,
            observation_start: None,
            terminal,
        },
    )
    .unwrap();
    assert_eq!(validated.integrity(), Integrity::Complete);
    assert_eq!(validated.stream().frame_completed_records, 2);
}

#[test]
fn rejects_unbound_producer_stop_facts() {
    let raw = stream(1, 40);
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &raw).unwrap();
    let mut terminal = terminal(1, raw.len() as u64, 41);
    terminal.stop_event = Some(EventStopFacts {
        sequence: 0,
        event_class: "frame_completed".into(),
        clock_domain: "frame".into(),
        clock_tick: 41,
        frame: 40,
        occurrence: 1,
    });
    assert!(matches!(
        validate_recording(
            file.path(),
            &EventContractRegistry::builtin().unwrap(),
            RecordingValidationInput {
                request: request(1),
                origin: RecordingOrigin::NextFrameBoundary,
                f_start: 40,
                f_end: 41,
                observation_start: None,
                terminal,
            },
        ),
        Err(RecordingValidationError::StopEventMismatch)
    ));
}
