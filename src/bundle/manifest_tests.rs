use super::manifest::*;
use super::recording_manifest::EventOrder;

fn sample() -> Manifest {
    Manifest {
        format_version: FORMAT_VERSION,
        platform: "snes".into(),
        rom: RomId {
            sha1: "abc123".into(),
            path_hint: Some("roms/game.sfc".into()),
        },
        adapter: ComponentId {
            name: "mesen2".into(),
            version: "0.1".into(),
        },
        emulator: ComponentId {
            name: "Mesen2".into(),
            version: "2.0".into(),
        },
        trigger: Trigger {
            kind: TriggerKind::Retrospective,
            at_unix_ms: 100,
            at_frame: 1264,
        },
        ring_policy: RingPolicy {
            interval_frames: 30,
            depth: 8,
        },
        slices: vec![Slice {
            frame: 1234,
            artifacts: vec![
                Artifact::Savestate {
                    path: "slices/f01234/state.mss".into(),
                },
                Artifact::Screenshot {
                    path: "slices/f01234/screen.png".into(),
                },
            ],
        }],
        input_movie: Some("input.movie".into()),
    }
}

#[test]
fn manifest_roundtrips_through_json() {
    let m = sample();
    let json = serde_json::to_string(&m).unwrap();
    let back: Manifest = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
}

#[test]
fn artifact_uses_kind_tag() {
    let json = serde_json::to_string(&Artifact::Savestate { path: "x".into() }).unwrap();
    assert!(json.contains("\"kind\":\"savestate\""), "got: {json}");
}

#[test]
fn trigger_kind_is_snake_case() {
    let json = serde_json::to_string(&TriggerKind::RecordWindow).unwrap();
    assert_eq!(json, "\"record_window\"");
}

fn recording_sample() -> RecordingManifest {
    use super::recording_manifest::*;
    RecordingManifest {
        format_version: RECORDING_FORMAT_VERSION,
        capture_id: "capture-01TEST".into(),
        capture_kind: CaptureKind::RecordWindow,
        created_at_unix_ms: 100,
        request_digest_sha256: "11".repeat(32),
        runtime: RuntimeIdentity {
            system: "snes".into(),
            adapter_id: "mesen2".into(),
            server_build: "server".into(),
            adapter_build: "adapter".into(),
            emulator_id: "Mesen".into(),
            emulator_build: "emulator".into(),
            emulator_upstream_revision: "upstream".into(),
            emulator_patchset_sha256: "22".repeat(32),
            launch_id: "launch-01TEST".into(),
            capability_revision: "33".repeat(32),
            content: ContentIdentity {
                sha1: Some("44".repeat(20)),
                sha256: None,
                bytes: 524288,
                path_hint: None,
            },
        },
        request: RecordingRequest {
            frames: 1,
            warmup_frames: 0,
            event_classes: vec![EventClassIdentity {
                id: "frame_boundary".into(),
                contract_sha256: "55".repeat(32),
            }],
            event_arming: vec![],
            limits: RecordingLimits {
                max_frames: 300,
                max_events: 1000,
                max_bytes: 1024,
                max_line_bytes: 512,
                max_host_ms: 8000,
                progress_interval_ms: 100,
            },
            input_movie: None,
            stop_on: None,
            start_on: None,
            initial_snapshots: vec![],
            terminal_snapshots: vec![],
            terminal_state: None,
        },
        scope: EffectiveScope {
            origin: RecordingOrigin::NextFrameBoundary,
            f_origin: None,
            f_start: 10,
            f_end: 11,
            clock_domains: vec!["frame".into()],
            event_order: None,
            observation_start: None,
        },
        terminal: TerminalFacts {
            operation_outcome: OperationOutcome::Completed,
            execution_outcome: ExecutionOutcome::TargetReached,
            integrity: Integrity::Complete,
            publication: PublicationOutcome::Published,
            final_execution_state: FinalExecutionState::Frozen,
            final_frame: 11,
            stop_event: None,
            reason: None,
            event_classes: Vec::new(),
        },
        counters: RecordingCounters {
            frames: 1,
            events: 1,
            bytes: 100,
            dropped: 0,
        },
        loss: LossFacts {
            dropped: 0,
            truncated: false,
            first_sequence_gap: None,
        },
        cleanup: CleanupFacts {
            hooks: CleanupState::NotAcquired,
            transient_input: CleanupState::NotAcquired,
            sink: CleanupState::Released,
        },
        members: vec![MemberDescriptor {
            role: MemberRole::Events,
            path: "events/segment-000.ndjson".into(),
            sha256: "66".repeat(32),
            bytes: 100,
            records: Some(1),
        }],
    }
}

#[test]
fn dispatches_legacy_and_recording_versions_before_full_decode() {
    let legacy = serde_json::to_string(&sample()).unwrap();
    assert!(matches!(
        parse_manifest(&legacy).unwrap(),
        BundleManifest::Legacy(_)
    ));
    let recording = serde_json::to_string(&recording_sample()).unwrap();
    assert!(matches!(
        parse_manifest(&recording).unwrap(),
        BundleManifest::Recording(_)
    ));
}

#[test]
fn recording_scope_preserves_an_explicit_guest_emission_order_claim() {
    let mut manifest = recording_sample();
    manifest.scope.event_order = Some(EventOrder::GuestEmission);
    let json = serde_json::to_string(&manifest).unwrap();
    let BundleManifest::Recording(decoded) = parse_manifest(&json).unwrap() else {
        panic!("recording manifest was not dispatched as recording");
    };
    assert_eq!(decoded.scope.event_order, Some(EventOrder::GuestEmission));
}

#[test]
fn rejects_unknown_version_and_missing_recording_identity() {
    let unknown = r#"{"format_version":999}"#;
    assert!(matches!(
        parse_manifest(unknown),
        Err(ManifestDecodeError::UnsupportedFormatVersion(999))
    ));

    let mut value = serde_json::to_value(recording_sample()).unwrap();
    value.as_object_mut().unwrap().remove("runtime");
    assert!(matches!(
        parse_manifest(&value.to_string()),
        Err(ManifestDecodeError::Json(_))
    ));
}

#[test]
fn recording_fields_do_not_default_into_legacy_manifest() {
    let mut value = serde_json::to_value(recording_sample()).unwrap();
    value["format_version"] = serde_json::json!(LEGACY_FORMAT_VERSION);
    assert!(matches!(
        parse_manifest(&value.to_string()),
        Err(ManifestDecodeError::Json(_))
    ));
}
