use std::fs;

use serde_json::json;

use super::event::*;
use super::recording_manifest::{
    EventClassFilter, EventClassIdentity, EventFilterTerm, EventStopCondition, RecordingLimits,
};
use crate::event_contracts::EventContractRegistry;

fn identity() -> EventClassIdentity {
    EventContractRegistry::builtin()
        .unwrap()
        .identities(["frame_boundary"])
        .unwrap()
        .remove(0)
}

fn limits() -> RecordingLimits {
    RecordingLimits {
        max_frames: 300,
        max_events: 1000,
        max_bytes: 1024 * 1024,
        max_line_bytes: 4096,
        max_host_ms: 30_000,
        progress_interval_ms: 100,
    }
}

fn event(sequence: u64, frame: u64) -> EventEnvelope {
    EventEnvelope {
        sequence,
        class: "frame_boundary".into(),
        contract_sha256: identity().contract_sha256,
        clock: ClockPoint {
            domain: "frame".into(),
            tick: frame,
        },
        frame,
        payload: json!({}),
    }
}

fn completed(sequence: u64, frame: u64) -> EventEnvelope {
    let identity = EventContractRegistry::builtin()
        .unwrap()
        .identities(["frame_completed"])
        .unwrap()
        .remove(0);
    EventEnvelope {
        sequence,
        class: identity.id,
        contract_sha256: identity.contract_sha256,
        clock: ClockPoint {
            domain: "frame".into(),
            tick: frame + 1,
        },
        frame,
        payload: json!({}),
    }
}

fn obj_handoff(sequence: u64, frame: u64, tick: u64) -> EventEnvelope {
    let identity = EventContractRegistry::builtin()
        .unwrap()
        .identities(["snes_ppu_obj_handoff"])
        .unwrap()
        .remove(0);
    EventEnvelope {
        sequence,
        class: identity.id,
        contract_sha256: identity.contract_sha256,
        clock: ClockPoint {
            domain: "snes_master".into(),
            tick,
        },
        frame,
        payload: json!({
            "cpu": {"pc": 0x808000, "a": 1, "x": 2, "y": 3, "sp": 0x1ff,
                    "d": 0, "dbr": 0x7e, "k": 0x80, "ps": 0x34},
            "ppu": {"scanline": 17, "dot": 340, "hclock": 1360},
            "forced_blank": false
        }),
    }
}

fn obj_consumption(sequence: u64, frame: u64, tick: u64, address: u64) -> EventEnvelope {
    let identity = EventContractRegistry::builtin()
        .unwrap()
        .identities(["snes_ppu_obj_consumption_read"])
        .unwrap()
        .remove(0);
    EventEnvelope {
        sequence,
        class: identity.id,
        contract_sha256: identity.contract_sha256,
        clock: ClockPoint {
            domain: "snes_master".into(),
            tick,
        },
        frame,
        payload: json!({
            "memory_kind": 1,
            "address": address,
            "value": 0x34,
            "scanline": 17,
            "dot": 128,
            "hclock": 512
        }),
    }
}

fn cgram_lookup(sequence: u64, frame: u64, tick: u64, address: u64, target: u64) -> EventEnvelope {
    let identity = EventContractRegistry::builtin()
        .unwrap()
        .identities(["snes_ppu_cgram_lookup"])
        .unwrap()
        .remove(0);
    EventEnvelope {
        sequence,
        class: identity.id,
        contract_sha256: identity.contract_sha256,
        clock: ClockPoint {
            domain: "snes_master".into(),
            tick,
        },
        frame,
        payload: json!({
            "address": address,
            "value": 0x1234,
            "layer": 4,
            "target": target,
            "pixel_x": 72,
            "scanline": 40,
            "dot": 94,
            "hclock": 376
        }),
    }
}

fn bytes(events: &[EventEnvelope]) -> Vec<u8> {
    let mut output = Vec::new();
    for event in events {
        serde_json::to_writer(&mut output, event).unwrap();
        output.push(b'\n');
    }
    output
}

fn validate(
    raw: &[u8],
    f_start: u64,
    frames: u64,
) -> Result<EventStreamReport, EventValidationError> {
    let temp = tempfile::NamedTempFile::new().unwrap();
    fs::write(temp.path(), raw).unwrap();
    validate_event_stream(
        temp.path(),
        &EventContractRegistry::builtin().unwrap(),
        &EventValidationSpec {
            f_start,
            f_end: f_start + frames,
            observation_start: f_start,
            class_intervals: Default::default(),
            event_classes: vec![identity()],
            event_filters: vec![],
            limits: limits(),
            stop_on: None,
        },
    )
}

#[test]
fn validates_exact_one_and_120_frame_streams() {
    for frames in [1_u64, 120] {
        let events: Vec<_> = (0..frames)
            .map(|offset| event(offset, 400 + offset))
            .collect();
        let report = validate(&bytes(&events), 400, frames).unwrap();
        assert_eq!(report.records, frames);
        assert_eq!(report.frame_boundary_records, frames);
        assert_eq!(report.first_sequence, Some(0));
        assert_eq!(report.last_sequence, Some(frames - 1));
        assert_eq!(report.completeness, StreamCompleteness::Complete);
        assert_eq!(report.complete_bytes, report.physical_bytes);
        assert_eq!(report.sha256.len(), 64);
    }
}

#[test]
fn rejects_sequence_duplicate_and_gap() {
    for actual in [0_u64, 2] {
        let error = validate(&bytes(&[event(0, 1), event(actual, 2)]), 1, 2).unwrap_err();
        assert!(matches!(
            error,
            EventValidationError::Sequence {
                expected: 1,
                actual: value
            } if value == actual
        ));
    }
}

#[test]
fn reports_a_partial_final_line_without_parsing_it_as_complete() {
    let mut raw = bytes(&[event(0, 10)]);
    raw.extend_from_slice(br#"{"sequence":1"#);
    let report = validate(&raw, 10, 2).unwrap();
    assert_eq!(report.records, 1);
    assert_eq!(report.completeness, StreamCompleteness::PartialFinalLine);
    assert!(report.physical_bytes > report.complete_bytes);
}

#[test]
fn enforces_line_and_total_byte_bounds_before_unbounded_growth() {
    let raw = bytes(&[event(0, 10)]);
    let temp = tempfile::NamedTempFile::new().unwrap();
    fs::write(temp.path(), &raw).unwrap();
    let mut bounded = limits();
    bounded.max_line_bytes = 32;
    assert!(matches!(
        validate_event_stream(
            temp.path(),
            &EventContractRegistry::builtin().unwrap(),
            &EventValidationSpec {
                f_start: 10,
                f_end: 11,
                observation_start: 10,
                class_intervals: Default::default(),
                event_classes: vec![identity()],
                event_filters: vec![],
                limits: bounded,
                stop_on: None,
            }
        ),
        Err(EventValidationError::LineLimit { .. })
    ));

    let mut bounded = limits();
    bounded.max_bytes = (raw.len() - 1) as u64;
    assert!(matches!(
        validate_event_stream(
            temp.path(),
            &EventContractRegistry::builtin().unwrap(),
            &EventValidationSpec {
                f_start: 10,
                f_end: 11,
                observation_start: 10,
                class_intervals: Default::default(),
                event_classes: vec![identity()],
                event_filters: vec![],
                limits: bounded,
                stop_on: None,
            }
        ),
        Err(EventValidationError::ByteLimit(_))
    ));
}

#[test]
fn rejects_unknown_class_and_contract_digest_mismatch() {
    let mut unknown = event(0, 10);
    unknown.class = "unknown".into();
    assert!(matches!(
        validate(&bytes(&[unknown]), 10, 1),
        Err(EventValidationError::UnselectedClass(_))
    ));

    let mut mismatched = event(0, 10);
    mismatched.contract_sha256 = "11".repeat(32);
    assert!(matches!(
        validate(&bytes(&[mismatched]), 10, 1),
        Err(EventValidationError::ContractDigest { .. })
    ));
}

#[test]
fn rejects_clock_regression_scope_mismatch_and_bad_payload() {
    let mut regression = event(1, 10);
    regression.clock.tick = 10;
    assert!(matches!(
        validate(&bytes(&[event(0, 10), regression]), 10, 2),
        Err(EventValidationError::ClockRegression { .. })
    ));

    assert!(matches!(
        validate(&bytes(&[event(0, 11)]), 10, 1),
        Err(EventValidationError::FrameScope { .. })
    ));

    let mut payload = event(0, 10);
    payload.payload = json!({"consumer": true});
    assert!(matches!(
        validate(&bytes(&[payload]), 10, 1),
        Err(EventValidationError::Payload(_))
    ));
}

#[test]
fn empty_stream_is_a_bounded_prefix_not_complete_evidence() {
    let report = validate(&[], 10, 1).unwrap();
    assert_eq!(report.records, 0);
    assert_eq!(report.physical_bytes, 0);
    assert_eq!(report.completeness, StreamCompleteness::Prefix);
}

#[test]
fn validates_interleaved_boundaries_and_completions_at_shared_ticks() {
    let raw = bytes(&[
        event(0, 10),
        completed(1, 10),
        event(2, 11),
        completed(3, 11),
    ]);
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), raw).unwrap();
    let report = validate_event_stream(
        file.path(),
        &EventContractRegistry::builtin().unwrap(),
        &EventValidationSpec {
            f_start: 10,
            f_end: 12,
            observation_start: 10,
            class_intervals: Default::default(),
            event_classes: EventContractRegistry::builtin()
                .unwrap()
                .identities(["frame_boundary", "frame_completed"])
                .unwrap(),
            event_filters: vec![],
            limits: limits(),
            stop_on: None,
        },
    )
    .unwrap();
    assert_eq!(report.frame_boundary_records, 2);
    assert_eq!(report.frame_completed_records, 2);
    assert_eq!(report.completeness, StreamCompleteness::Complete);
}

#[test]
fn validates_typed_semantic_payload_and_rejects_schema_drift() {
    let registry = EventContractRegistry::builtin().unwrap();
    let classes = registry
        .identities(["frame_boundary", "snes_ppu_obj_handoff"])
        .unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        file.path(),
        bytes(&[event(0, 10), obj_handoff(1, 10, 12_345)]),
    )
    .unwrap();
    let report = validate_event_stream(
        file.path(),
        &registry,
        &EventValidationSpec {
            f_start: 10,
            f_end: 11,
            observation_start: 10,
            class_intervals: Default::default(),
            event_classes: classes.clone(),
            event_filters: vec![],
            limits: limits(),
            stop_on: None,
        },
    )
    .unwrap();
    assert_eq!(report.records, 2);

    let mut missing = obj_handoff(1, 10, 12_345);
    missing.payload["cpu"].as_object_mut().unwrap().remove("pc");
    fs::write(file.path(), bytes(&[event(0, 10), missing])).unwrap();
    assert!(matches!(
        validate_event_stream(
            file.path(),
            &registry,
            &EventValidationSpec {
                f_start: 10,
                f_end: 11,
                observation_start: 10,
                class_intervals: Default::default(),
                event_classes: classes.clone(),
                event_filters: vec![],
                limits: limits(),
                stop_on: None,
            },
        ),
        Err(EventValidationError::Payload(_))
    ));

    let mut extra = obj_handoff(1, 10, 12_345);
    extra.payload["consumer"] = json!(true);
    fs::write(file.path(), bytes(&[event(0, 10), extra])).unwrap();
    assert!(matches!(
        validate_event_stream(
            file.path(),
            &registry,
            &EventValidationSpec {
                f_start: 10,
                f_end: 11,
                observation_start: 10,
                class_intervals: Default::default(),
                event_classes: classes,
                event_filters: vec![],
                limits: limits(),
                stop_on: None,
            },
        ),
        Err(EventValidationError::Payload(_))
    ));
}

#[test]
fn accepts_only_events_inside_the_declared_payload_filter() {
    let registry = EventContractRegistry::builtin().unwrap();
    let classes = registry
        .identities(["frame_boundary", "snes_ppu_obj_consumption_read"])
        .unwrap();
    let filters = vec![EventClassFilter {
        event_class: "snes_ppu_obj_consumption_read".into(),
        terms: vec![
            EventFilterTerm::U64Range {
                path: "memory_kind".into(),
                start: 1,
                length: 1,
            },
            EventFilterTerm::U64Range {
                path: "address".into(),
                start: 0x2000,
                length: 0x100,
            },
        ],
    }];
    let file = tempfile::NamedTempFile::new().unwrap();
    let validate_address = |address| {
        fs::write(
            file.path(),
            bytes(&[event(0, 10), obj_consumption(1, 10, 12_345, address)]),
        )
        .unwrap();
        validate_event_stream(
            file.path(),
            &registry,
            &EventValidationSpec {
                f_start: 10,
                f_end: 11,
                observation_start: 10,
                class_intervals: Default::default(),
                event_classes: classes.clone(),
                event_filters: filters.clone(),
                limits: limits(),
                stop_on: None,
            },
        )
    };

    assert_eq!(validate_address(0x20ff).unwrap().records, 2);
    assert!(matches!(
        validate_address(0x2100),
        Err(EventValidationError::EventOutsideFilter(class))
            if class == "snes_ppu_obj_consumption_read"
    ));
}

#[test]
fn distinguishes_main_and_sub_cgram_lookup_targets() {
    let registry = EventContractRegistry::builtin().unwrap();
    let classes = registry
        .identities(["frame_boundary", "snes_ppu_cgram_lookup"])
        .unwrap();
    let filters = vec![EventClassFilter {
        event_class: "snes_ppu_cgram_lookup".into(),
        terms: vec![
            EventFilterTerm::U64Range {
                path: "address".into(),
                start: 0x80,
                length: 1,
            },
            EventFilterTerm::U64Range {
                path: "target".into(),
                start: 1,
                length: 1,
            },
        ],
    }];
    let file = tempfile::NamedTempFile::new().unwrap();
    let validate_lookup = |address, target| {
        fs::write(
            file.path(),
            bytes(&[event(0, 10), cgram_lookup(1, 10, 12_345, address, target)]),
        )
        .unwrap();
        validate_event_stream(
            file.path(),
            &registry,
            &EventValidationSpec {
                f_start: 10,
                f_end: 11,
                observation_start: 10,
                class_intervals: Default::default(),
                event_classes: classes.clone(),
                event_filters: filters.clone(),
                limits: limits(),
                stop_on: None,
            },
        )
    };

    assert_eq!(validate_lookup(0x80, 1).unwrap().records, 2);
    assert!(matches!(
        validate_lookup(0x81, 1),
        Err(EventValidationError::EventOutsideFilter(class))
            if class == "snes_ppu_cgram_lookup"
    ));
    assert!(matches!(
        validate_lookup(0x80, 2),
        Err(EventValidationError::EventOutsideFilter(class))
            if class == "snes_ppu_cgram_lookup"
    ));

    let mut invalid = cgram_lookup(1, 10, 12_345, 0x80, 1);
    invalid.payload["value"] = json!(0x8000);
    fs::write(file.path(), bytes(&[event(0, 10), invalid])).unwrap();
    assert!(matches!(
        validate_event_stream(
            file.path(),
            &registry,
            &EventValidationSpec {
                f_start: 10,
                f_end: 11,
                observation_start: 10,
                class_intervals: Default::default(),
                event_classes: classes,
                event_filters: filters,
                limits: limits(),
                stop_on: None,
            },
        ),
        Err(EventValidationError::Payload(_))
    ));
}

#[test]
fn binds_a_stop_occurrence_and_rejects_any_later_record() {
    let registry = EventContractRegistry::builtin().unwrap();
    let classes = registry
        .identities(["frame_boundary", "frame_completed"])
        .unwrap();
    let stop_on = EventStopCondition {
        event_class: "frame_completed".into(),
        occurrence: 1,
    };
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), bytes(&[event(0, 10), completed(1, 10)])).unwrap();
    let report = validate_event_stream(
        file.path(),
        &registry,
        &EventValidationSpec {
            f_start: 10,
            f_end: 11,
            observation_start: 10,
            class_intervals: Default::default(),
            event_classes: classes.clone(),
            event_filters: vec![],
            limits: limits(),
            stop_on: Some(stop_on.clone()),
        },
    )
    .unwrap();
    let stop = report.stop_event.unwrap();
    assert_eq!(stop.sequence, 1);
    assert_eq!(stop.frame, 10);
    assert_eq!(stop.clock_tick, 11);

    fs::write(
        file.path(),
        bytes(&[event(0, 10), completed(1, 10), event(2, 11)]),
    )
    .unwrap();
    assert!(matches!(
        validate_event_stream(
            file.path(),
            &registry,
            &EventValidationSpec {
                f_start: 10,
                f_end: 12,
                observation_start: 10,
                class_intervals: Default::default(),
                event_classes: classes,
                event_filters: vec![],
                limits: limits(),
                stop_on: Some(stop_on),
            },
        ),
        Err(EventValidationError::EventAfterStop)
    ));
}
