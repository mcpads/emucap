use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::Digest;

use super::capture_capsule::{
    CaptureCapsule, CaptureCapsuleError, CaptureCapsuleRepository, CaptureLeaseIdentity,
    CaptureState,
};
use super::continuity::{
    ContinuitySnapshot, EvidenceContinuity, EvidenceState, ExecutionContinuity, ExecutionState,
    LinkRecord, RuntimeBinding, RuntimeBindingState, TransportContinuity, TransportState,
};
use super::link::*;
use super::recording::*;
use super::recording_capability::*;
use super::recording_request::effective_request;
use super::runtime::*;
use crate::bundle::event::{ClockPoint, EventEnvelope};
use crate::bundle::manifest::{parse_manifest, BundleManifest};
use crate::bundle::recording::{ProducerTerminalReport, RecordingValidationInput};
use crate::bundle::recording_manifest::*;

#[derive(Debug, Clone, Copy)]
enum Mode {
    Normal,
    Loss,
    CleanupFailed,
    Disconnect,
    Reject,
    SnapshotReadFailure,
    TerminalStateReadFailure,
    SnapshotGenerationChange,
}

struct SyntheticLink {
    caps: Capabilities,
    port: u16,
    mode: Mode,
    delay: Duration,
    terminal_frame: u64,
    status_calls: u64,
}

fn capability() -> RecordingCapability {
    let mut capability = RecordingCapability {
        revision: String::new(),
        origins: vec![RecordingCapabilityOrigin::NextFrameBoundary],
        units: vec![RecordingCapabilityUnit::Frames],
        default_event_classes: vec!["frame_boundary".into()],
        event_classes: vec![RecordingEventCapability {
            id: "frame_boundary".into(),
            contract_sha256: "498fcd52f2fa2327e0af9e9730b4314f0854a6047f57dcde16961b8a4ecb80cd"
                .into(),
            clock_domains: vec!["frame".into()],
            exact: true,
            stoppable: false,
            startable: false,
        }],
        event_order: None,
        class_accounting: false,
        input_movie: None,
        initial_snapshots: None,
        terminal_snapshots: None,
        terminal_state: None,
        warmup: None,
        limits: RecordingLimits {
            max_frames: 300,
            max_events: 100_000,
            max_bytes: 64 * 1024 * 1024,
            max_line_bytes: 64 * 1024,
            max_host_ms: 30_000,
            progress_interval_ms: 100,
        },
    };
    capability.revision = capability.computed_revision().unwrap();
    capability
}

impl SyntheticLink {
    fn new(port: u16, launch_id: &str, content: &str, mode: Mode, delay: Duration) -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec!["record_window".into(), "abort_recording".into()],
                memory_types: vec![],
                memory_regions: vec![],
                breakpoint_kinds: vec![],
                contracts: crate::contracts::ContractAdvertisement::Unreported,
                recording: Some(capability()),
                identity: EmulatorIdentity {
                    system: Some("snes".into()),
                    adapter: Some("synthetic-recording".into()),
                    build: Some("adapter-build".into()),
                    content: Some(content.into()),
                    launch_id: Some(launch_id.into()),
                    host_build: Some(json!({
                        "upstream": "https://example.invalid/emulator",
                        "commit": "upstream-commit",
                        "patchset_sha256": "22".repeat(32),
                        "binary_sha256": "33".repeat(32),
                    })),
                    ..EmulatorIdentity::default()
                },
            },
            port,
            mode,
            delay,
            terminal_frame: 0,
            status_calls: 0,
        }
    }

    fn enable_terminal_snapshots(&mut self) {
        self.caps
            .methods
            .extend(["status".into(), "read_memory".into()]);
        self.caps.memory_types = vec!["workram".into()];
        self.caps.memory_regions = vec![MemoryRegion {
            memory_type: "workram".into(),
            size: 256,
        }];
        let capability = self.caps.recording.as_mut().unwrap();
        capability.terminal_snapshots = Some(RecordingTerminalSnapshotCapability {
            max_members: 2,
            max_member_bytes: 16,
            max_total_bytes: 32,
        });
        capability.revision = capability.computed_revision().unwrap();
    }

    fn enable_warmup(&mut self) {
        let capability = self.caps.recording.as_mut().unwrap();
        capability.class_accounting = true;
        capability.warmup = Some(RecordingWarmupCapability {
            max_frames: capability.limits.max_frames,
            transaction_event_classes: vec!["frame_boundary".into()],
        });
        capability.revision = capability.computed_revision().unwrap();
    }

    fn enable_terminal_state(&mut self) {
        self.caps
            .methods
            .extend(["status".into(), "get_state".into()]);
        let groups = vec!["ppu".into()];
        let capability = self.caps.recording.as_mut().unwrap();
        capability.terminal_state = Some(RecordingTerminalStateCapability {
            max_bytes: 4096,
            profiles: vec![RecordingTerminalStateProfile {
                id: "snes_ppu".into(),
                contract_sha256: terminal_state_contract_sha256(&groups).unwrap(),
                groups,
            }],
        });
        capability.revision = capability.computed_revision().unwrap();
    }
}

impl EmulatorLink for SyntheticLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        match method {
            "status" => {
                self.status_calls += 1;
                if matches!(self.mode, Mode::SnapshotGenerationChange) && self.status_calls == 2 {
                    self.caps.identity.launch_id = Some("launch-01replacement".into());
                }
                Ok(json!({"connected": true, "state": "frozen", "frame": self.terminal_frame}))
            }
            "read_memory" if matches!(self.mode, Mode::SnapshotReadFailure) => {
                Err(LinkError::Emulator {
                    kind: "injected_read_failure".into(),
                    message: "synthetic terminal snapshot read failed".into(),
                })
            }
            "read_memory" => {
                let address = params["address"].as_u64().unwrap();
                let length = params["length"].as_u64().unwrap();
                let bytes = (0..length)
                    .map(|offset| ((address + offset) & 0xff) as u8)
                    .collect::<Vec<_>>();
                Ok(json!({"hex": hex::encode(bytes)}))
            }
            "get_state" if matches!(self.mode, Mode::TerminalStateReadFailure) => {
                Err(LinkError::Emulator {
                    kind: "injected_state_failure".into(),
                    message: "synthetic terminal state read failed".into(),
                })
            }
            "get_state" => Ok(json!({
                "state": {
                    "ppu.layers[2].tilemapAddress": 4096,
                    "ppu.bgMode": 1,
                    "ppu.forcedBlank": false
                }
            })),
            _ => Err(LinkError::Protocol(
                "synthetic recording requires call_with_progress".into(),
            )),
        }
    }

    fn call_with_progress(
        &mut self,
        method: &str,
        params: Value,
        observer: &mut ProgressObserver<'_>,
        control: &ProgressCallControl,
    ) -> Result<Value, LinkError> {
        assert_eq!(method, "record_window");
        let capture_id = params["capture_id"].as_str().unwrap().to_string();
        let launch_id = params["launch_id"].as_str().unwrap().to_string();
        assert_eq!(launch_id, self.caps.identity.launch_id.as_deref().unwrap());
        assert_eq!(
            control.abort.as_ref().unwrap().params["capture_id"],
            capture_id
        );
        assert_eq!(
            control.abort.as_ref().unwrap().params["launch_id"],
            launch_id
        );
        if matches!(self.mode, Mode::Reject) {
            return Err(LinkError::Emulator {
                kind: "unsafe_halt".into(),
                message: "the frozen position is not a frame boundary".into(),
            });
        }
        let frames = params["frames"].as_u64().unwrap();
        let warmup_frames = params["warmup_frames"].as_u64().unwrap();
        let total_frames = frames + warmup_frames;
        let event = &params["event_classes"][0];
        let contract = event["contract_sha256"].as_str().unwrap().to_string();
        let endpoint = params["sink"]["endpoint"].as_str().unwrap();
        let token = params["sink"]["token"].as_str().unwrap();
        let mut sink = TcpStream::connect(endpoint).unwrap();
        writeln!(
            sink,
            "{}",
            json!({"token": token, "capture_id": capture_id})
        )
        .unwrap();
        observer(&WorkingProgress {
            status: "working".into(),
            capture_id: capture_id.clone(),
            sequence: 0,
            frame: 100,
            frames: Some(0),
            events: 0,
            bytes: 0,
            phase: None,
        })?;

        let write_frames = match self.mode {
            Mode::Normal
            | Mode::CleanupFailed
            | Mode::SnapshotReadFailure
            | Mode::TerminalStateReadFailure
            | Mode::SnapshotGenerationChange => total_frames,
            Mode::Loss | Mode::Disconnect => total_frames.min(2),
            Mode::Reject => unreachable!("rejection returns before opening the sink"),
        };
        let mut bytes = 0_u64;
        for offset in 0..write_frames {
            std::thread::sleep(self.delay);
            let mut line = serde_json::to_vec(&EventEnvelope {
                sequence: offset,
                class: "frame_boundary".into(),
                contract_sha256: contract.clone(),
                clock: ClockPoint {
                    domain: "frame".into(),
                    tick: 100 + offset,
                },
                frame: 100 + offset,
                payload: json!({}),
            })
            .unwrap();
            line.push(b'\n');
            sink.write_all(&line).unwrap();
            bytes += line.len() as u64;
            observer(&WorkingProgress {
                status: "working".into(),
                capture_id: capture_id.clone(),
                sequence: offset + 1,
                frame: 101 + offset,
                frames: Some(offset + 1),
                events: offset + 1,
                bytes,
                phase: None,
            })?;
            if control.cancellation.is_cancelled() {
                drop(sink);
                return Err(LinkError::Cancelled);
            }
        }
        drop(sink);
        if matches!(self.mode, Mode::Disconnect) {
            return Err(LinkError::NotConnected);
        }
        self.terminal_frame = 100 + total_frames;
        let completed = matches!(
            self.mode,
            Mode::Normal
                | Mode::SnapshotReadFailure
                | Mode::SnapshotGenerationChange
                | Mode::TerminalStateReadFailure
        );
        let cleanup_failed = matches!(self.mode, Mode::CleanupFailed);
        let class_accounting = self
            .caps
            .recording
            .as_ref()
            .is_some_and(|capability| capability.class_accounting);
        let event_classes = if class_accounting {
            json!([{
                "id": "frame_boundary",
                "armed": true,
                "armed_interval": if warmup_frames > 0 {
                    json!({"f_start": 100, "f_end": 100 + total_frames})
                } else {
                    Value::Null
                },
                "observed": write_frames,
                "dropped": total_frames - write_frames,
            }])
        } else {
            json!([])
        };
        Ok(json!({
            "status": if completed {"completed"} else {"failed"},
            "capture_id": capture_id,
            "operation_outcome": if completed {"completed"} else {"failed"},
            "execution_outcome": if completed {"target_reached"} else if cleanup_failed {"adapter_error"} else {"loss_detected"},
            "integrity": if completed {"complete"} else if cleanup_failed {"unverifiable"} else {"lossy"},
            "reason": if completed {Value::Null} else if cleanup_failed {json!("cleanup_failed")} else {json!("injected_loss")},
            "f_origin": if warmup_frames > 0 {json!(100)} else {Value::Null},
            "f_start": 100 + warmup_frames,
            "f_end": 100 + total_frames,
            "final_frame": if completed {100 + total_frames} else {100 + write_frames},
            "frames": frames,
            "events": write_frames,
            "bytes": bytes,
            "physical_bytes": bytes,
            "dropped": total_frames - write_frames,
            "truncated": false,
            "first_sequence_gap": Value::Null,
            "wall_ms": 1,
            "final_execution_state": "frozen",
            "cleanup": {
                "hooks": if cleanup_failed {"unverifiable"} else {"released"},
                "transient_input": "not_acquired",
                "sink": "released",
            },
            "event_classes": event_classes,
        }))
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(self.port)
    }

    fn acquire_control_lease(&mut self, expected_launch_id: &str) -> Result<LeaseView, LinkError> {
        assert_eq!(
            Some(expected_launch_id),
            self.caps.identity.launch_id.as_deref()
        );
        Ok(LeaseView {
            state: LeaseState::Held,
            holder_pid: Some(std::process::id()),
        })
    }

    fn continuity(&self) -> ContinuitySnapshot {
        let launch_id = self.caps.identity.launch_id.clone();
        ContinuitySnapshot {
            runtime_binding: RuntimeBinding {
                state: RuntimeBindingState::Bound,
                current_launch_id: launch_id.clone(),
                live_launch_id: launch_id,
                reason: "synthetic link is bound to the exact test generation".into(),
            },
            transport: TransportContinuity {
                state: TransportState::Connected,
                last_response_unix_ms: Some(super::runtime::now_unix_ms()),
                consecutive_timeouts: 0,
            },
            execution: ExecutionContinuity {
                state: ExecutionState::Frozen,
                source: "synthetic-adapter".into(),
            },
            evidence: EvidenceContinuity {
                state: EvidenceState::Live,
                failure_context_available: true,
            },
            lease: LeaseView {
                state: LeaseState::Held,
                holder_pid: Some(std::process::id()),
            },
            lease_record_present: true,
            ..ContinuitySnapshot::default()
        }
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    store: RuntimeStore,
    port: u16,
    launch_id: String,
    content: String,
    output: tempfile::TempDir,
}

fn harness() -> Harness {
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("sessions"));
    let content = temp.path().join("fixture.sfc");
    std::fs::write(&content, b"synthetic recording content").unwrap();
    let port = 47800;
    let prepared = store.prepare(port).unwrap();
    let launch_id = prepared.launch_id().to_string();
    prepared
        .commit(&prepared.manifest(ManifestSpec {
            adapter: "synthetic-recording".into(),
            system: "snes".into(),
            content: content.display().to_string(),
            emulator_pid: std::process::id(),
            bridge_pid: None,
            backend_endpoint: None,
            build: Some("adapter-build".into()),
        }))
        .unwrap();
    let lease = CaptureLeaseIdentity::current();
    let lease_record = lease.clone();
    let record_launch = launch_id.clone();
    store
        .update_link_json(port, &launch_id, move |_: Option<LinkRecord>| {
            let mut record = LinkRecord::new(record_launch);
            record.lease = Some(LeaseRecord {
                control_session_key: lease_record.control_session_key,
                holder: lease_record.holder,
                acquired_at_unix_ms: 1,
                refreshed_at_unix_ms: 1,
            });
            Ok(record)
        })
        .unwrap();
    Harness {
        _temp: temp,
        store,
        port,
        launch_id,
        content: content.display().to_string(),
        output: tempfile::tempdir().unwrap(),
    }
}

fn request(output: &std::path::Path, frames: u64) -> RecordWindowRequest {
    RecordWindowRequest {
        output_root: output.to_path_buf(),
        frames,
        warmup_frames: 0,
        event_classes: vec![],
        origin: None,
        input_path: None,
        stop_on: None,
        start_on: None,
        initial_snapshots: vec![],
        terminal_snapshots: vec![],
        terminal_state_profile: None,
        limits: Some(RequestedRecordingLimits {
            max_events: Some(1000),
            max_bytes: Some(1024 * 1024),
            max_host_ms: Some(5000),
        }),
    }
}

#[test]
fn omitted_host_deadline_scales_only_long_explicit_windows() {
    assert_eq!(default_recording_host_ms(250_000, 120), 30_000);
    assert_eq!(default_recording_host_ms(250_000, 1_400), 70_000);
    assert_eq!(default_recording_host_ms(250_000, 5_000), 250_000);
    assert_eq!(default_recording_host_ms(30_000, 5_000), 30_000);
}

#[test]
fn event_aligned_start_and_initial_snapshot_are_admitted_as_one_bounded_contract() {
    let registry = crate::event_contracts::EventContractRegistry::builtin().unwrap();
    let instruction = registry
        .identities(["snes_cpu_instruction"])
        .unwrap()
        .remove(0);
    let mut capability = capability();
    capability.event_classes.push(RecordingEventCapability {
        id: instruction.id.clone(),
        contract_sha256: instruction.contract_sha256,
        clock_domains: vec!["snes_master".into()],
        exact: true,
        stoppable: false,
        startable: true,
    });
    capability.event_order = Some(RecordingEventOrder::GuestEmission);
    capability.class_accounting = true;
    capability.warmup = Some(RecordingWarmupCapability {
        max_frames: 300,
        transaction_event_classes: vec!["frame_boundary".into()],
    });
    capability.initial_snapshots = Some(RecordingInitialSnapshotCapability {
        memory_types: vec!["snesWorkRam".into()],
        start_positions: vec![RecordingInitialSnapshotPosition::EventAnchor],
        max_members: 1,
        max_member_bytes: 128 * 1024,
        max_total_bytes: 128 * 1024,
        max_callback_ms: 100,
    });
    capability.revision = capability.computed_revision().unwrap();
    let output = tempfile::tempdir().unwrap();
    let mut request = request(output.path(), 1);
    request.event_classes = vec!["frame_boundary".into(), instruction.id.clone()];
    request.start_on = Some(EventStartCondition {
        event_class: instruction.id,
    });
    request.initial_snapshots = vec![InitialSnapshotRequest {
        label: "wram".into(),
        memory_type: "snesWorkRam".into(),
        address: 0,
        length: 128 * 1024,
    }];
    let regions = vec![MemoryRegion {
        memory_type: "snesWorkRam".into(),
        size: 128 * 1024,
    }];

    let effective =
        effective_request(&capability, &["record_window".into()], &regions, &request).unwrap();
    assert_eq!(effective.request.event_arming.len(), 2);
    assert_eq!(
        effective.request.event_arming[0].scope,
        EventArmingScope::Transaction
    );
    assert_eq!(
        effective.request.event_arming[1].scope,
        EventArmingScope::Observation
    );

    request.start_on = None;
    assert!(effective_request(&capability, &["record_window".into()], &regions, &request).is_err());
    request.start_on = Some(EventStartCondition {
        event_class: "snes_cpu_instruction".into(),
    });
    request.initial_snapshots[0].address = 1;
    assert!(effective_request(&capability, &["record_window".into()], &regions, &request).is_err());
}

fn run(harness: &Harness, mode: Mode, delay: Duration, frames: u64) -> RecordWindowResult {
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        mode,
        delay,
    );
    record_window(
        &mut link,
        harness.store.clone(),
        request(harness.output.path(), frames),
        RequestCancellation::default(),
        &mut |_| {},
    )
    .unwrap()
}

#[test]
fn synthetic_normal_transaction_publishes_exact_validated_bundle() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let result = run(&harness, Mode::Normal, Duration::ZERO, 3);
    assert_eq!(result.operation_outcome, OperationOutcome::Completed);
    assert_eq!(result.integrity, Integrity::Complete);
    assert_eq!(result.frames, 3);
    assert_eq!(result.events, 3);
    let manifest =
        std::fs::read_to_string(std::path::Path::new(&result.bundle_path).join("manifest.json"))
            .unwrap();
    let BundleManifest::Recording(manifest) = parse_manifest(&manifest).unwrap() else {
        panic!("recording manifest expected")
    };
    assert_eq!(manifest.scope.f_start, 100);
    assert_eq!(manifest.scope.f_end, 103);
    assert_eq!(manifest.members[0].records, Some(3));
    assert!(!std::path::Path::new(&result.bundle_path)
        .join("snapshots")
        .exists());
    assert_eq!(
        manifest.runtime.content.path_hint.as_deref(),
        Some("fixture.sfc")
    );
}

#[test]
fn warmup_is_one_transaction_with_exact_per_class_arming_scope() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );
    link.enable_warmup();
    let mut capture_request = request(harness.output.path(), 2);
    capture_request.warmup_frames = 3;
    let result = record_window(
        &mut link,
        harness.store.clone(),
        capture_request,
        RequestCancellation::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.frames, 2);
    assert_eq!(result.events, 5);
    let manifest =
        std::fs::read_to_string(std::path::Path::new(&result.bundle_path).join("manifest.json"))
            .unwrap();
    let BundleManifest::Recording(manifest) = parse_manifest(&manifest).unwrap() else {
        panic!("recording manifest expected")
    };
    assert_eq!(manifest.scope.f_origin, Some(100));
    assert_eq!(manifest.scope.f_start, 103);
    assert_eq!(manifest.scope.f_end, 105);
    assert_eq!(manifest.request.warmup_frames, 3);
    assert_eq!(
        manifest.request.event_arming,
        vec![EventClassArming {
            id: "frame_boundary".into(),
            scope: EventArmingScope::Transaction,
        }]
    );
    assert_eq!(
        manifest.terminal.event_classes[0].armed_interval,
        Some(FrameInterval {
            f_start: 100,
            f_end: 105,
        })
    );
}

#[test]
fn terminal_snapshot_is_read_only_at_the_frozen_terminal_frame_and_reverified() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );
    link.enable_terminal_snapshots();
    let mut capture_request = request(harness.output.path(), 3);
    capture_request.terminal_snapshots = vec![TerminalSnapshotRequest {
        label: "terminal-wram".into(),
        memory_type: "workram".into(),
        address: 2,
        length: 4,
    }];
    let result = record_window(
        &mut link,
        harness.store.clone(),
        capture_request,
        RequestCancellation::default(),
        &mut |_| {},
    )
    .unwrap();
    let bundle = std::path::Path::new(&result.bundle_path);
    assert_eq!(
        std::fs::read(bundle.join("snapshots/terminal-wram.bin")).unwrap(),
        vec![2, 3, 4, 5]
    );
    let manifest = std::fs::read_to_string(bundle.join("manifest.json")).unwrap();
    let BundleManifest::Recording(manifest) = parse_manifest(&manifest).unwrap() else {
        panic!("recording manifest expected")
    };
    assert_eq!(manifest.scope.f_end, 103);
    assert_eq!(manifest.request.terminal_snapshots.len(), 1);
    assert!(manifest
        .members
        .iter()
        .any(|member| member.role == MemberRole::TerminalSnapshot
            && member.path == "snapshots/terminal-wram.bin"
            && member.bytes == 4));
}

#[test]
fn terminal_state_profile_is_captured_as_one_canonical_hashed_member() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );
    link.enable_terminal_state();
    let mut capture_request = request(harness.output.path(), 2);
    capture_request.terminal_state_profile = Some("snes_ppu".into());
    let result = record_window(
        &mut link,
        harness.store.clone(),
        capture_request,
        RequestCancellation::default(),
        &mut |_| {},
    )
    .unwrap();
    let bundle = std::path::Path::new(&result.bundle_path);
    let state = std::fs::read(bundle.join("terminal-state.json")).unwrap();
    assert_eq!(
        state,
        br#"{"ppu.bgMode":1,"ppu.forcedBlank":false,"ppu.layers[2].tilemapAddress":4096}"#
    );
    let BundleManifest::Recording(manifest) =
        parse_manifest(&std::fs::read_to_string(bundle.join("manifest.json")).unwrap()).unwrap()
    else {
        panic!("recording manifest expected")
    };
    assert_eq!(
        manifest.request.terminal_state.as_ref().unwrap().profile,
        "snes_ppu"
    );
    assert!(manifest.members.iter().any(|member| {
        member.role == MemberRole::TerminalState
            && member.path == "terminal-state.json"
            && member.sha256 == hex::encode(sha2::Sha256::digest(&state))
    }));
    crate::bundle::publish::verify_published_recording(
        bundle,
        &crate::event_contracts::EventContractRegistry::builtin().unwrap(),
    )
    .unwrap();
}

#[test]
fn terminal_state_read_failure_cannot_publish_a_complete_bundle() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::TerminalStateReadFailure,
        Duration::ZERO,
    );
    link.enable_terminal_state();
    let mut capture_request = request(harness.output.path(), 1);
    capture_request.terminal_state_profile = Some("snes_ppu".into());
    assert!(record_window(
        &mut link,
        harness.store.clone(),
        capture_request,
        RequestCancellation::default(),
        &mut |_| {},
    )
    .is_err());
    assert!(std::fs::read_dir(harness.output.path())
        .unwrap()
        .filter_map(Result::ok)
        .all(|entry| entry.file_name().to_string_lossy().contains(".invalid-")));
}

#[test]
fn terminal_snapshot_failure_quarantines_without_a_complete_bundle() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::SnapshotReadFailure,
        Duration::ZERO,
    );
    link.enable_terminal_snapshots();
    let mut capture_request = request(harness.output.path(), 1);
    capture_request.terminal_snapshots = vec![TerminalSnapshotRequest {
        label: "terminal-wram".into(),
        memory_type: "workram".into(),
        address: 0,
        length: 4,
    }];
    assert!(record_window(
        &mut link,
        harness.store.clone(),
        capture_request,
        RequestCancellation::default(),
        &mut |_| {},
    )
    .is_err());
    assert!(!std::fs::read_dir(harness.output.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("capture-")));
    let capsule: CaptureCapsule = harness
        .store
        .read_capture_json(harness.port, &harness.launch_id)
        .unwrap()
        .unwrap();
    assert_eq!(capsule.state, CaptureState::PublicationFailed);
    assert_eq!(capsule.terminal.unwrap().integrity, Integrity::Unverifiable);

    link.mode = Mode::Normal;
    let retry = record_window(
        &mut link,
        harness.store.clone(),
        request(harness.output.path(), 1),
        RequestCancellation::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(retry.integrity, Integrity::Complete);
}

#[test]
fn terminal_snapshot_generation_change_never_publishes_old_generation_bytes() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::SnapshotGenerationChange,
        Duration::ZERO,
    );
    link.enable_terminal_snapshots();
    let mut capture_request = request(harness.output.path(), 1);
    capture_request.terminal_snapshots = vec![TerminalSnapshotRequest {
        label: "terminal-wram".into(),
        memory_type: "workram".into(),
        address: 0,
        length: 4,
    }];
    assert!(record_window(
        &mut link,
        harness.store.clone(),
        capture_request,
        RequestCancellation::default(),
        &mut |_| {},
    )
    .is_err());
    assert!(!std::fs::read_dir(harness.output.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("capture-")));
    let capsule: CaptureCapsule = harness
        .store
        .read_capture_json(harness.port, &harness.launch_id)
        .unwrap()
        .unwrap();
    assert_eq!(capsule.state, CaptureState::PublicationFailed);
    assert_eq!(capsule.terminal.unwrap().integrity, Integrity::Unverifiable);
}

#[cfg(unix)]
#[test]
fn terminal_snapshot_write_failure_quarantines_without_a_complete_bundle() {
    use std::os::unix::fs::PermissionsExt;

    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );
    link.enable_terminal_snapshots();
    let mut capture_request = request(harness.output.path(), 1);
    capture_request.terminal_snapshots = vec![TerminalSnapshotRequest {
        label: "terminal-wram".into(),
        memory_type: "workram".into(),
        address: 0,
        length: 4,
    }];
    let mut made_staging_read_only = false;
    let result = record_window(
        &mut link,
        harness.store.clone(),
        capture_request,
        RequestCancellation::default(),
        &mut |progress| {
            if progress.events == 1 && !made_staging_read_only {
                let staging = std::fs::read_dir(harness.output.path())
                    .unwrap()
                    .filter_map(Result::ok)
                    .find(|entry| entry.file_name().to_string_lossy().contains(".staging-"))
                    .unwrap();
                std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o500))
                    .unwrap();
                made_staging_read_only = true;
            }
        },
    );
    assert!(made_staging_read_only);
    assert!(result.is_err());
    assert!(!std::fs::read_dir(harness.output.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("capture-")));
    let capsule: CaptureCapsule = harness
        .store
        .read_capture_json(harness.port, &harness.launch_id)
        .unwrap()
        .unwrap();
    assert_eq!(capsule.state, CaptureState::PublicationFailed);
    assert_eq!(capsule.terminal.unwrap().integrity, Integrity::Unverifiable);
}

#[test]
fn invalid_terminal_snapshot_is_rejected_before_staging_or_guest_progress() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );
    link.enable_terminal_snapshots();
    let mut capture_request = request(harness.output.path(), 1);
    capture_request.terminal_snapshots = vec![TerminalSnapshotRequest {
        label: "../escape".into(),
        memory_type: "workram".into(),
        address: 0,
        length: 4,
    }];
    assert!(matches!(
        record_window(
            &mut link,
            harness.store.clone(),
            capture_request,
            RequestCancellation::default(),
            &mut |_| {},
        ),
        Err(RecordingError::Invalid(_))
    ));
    assert_eq!(std::fs::read_dir(harness.output.path()).unwrap().count(), 0);
}

#[test]
fn host_delay_does_not_change_guest_frame_projection() {
    let _env = crate::test_env::lock_env();
    let fast = harness();
    let slow = harness();
    let fast = run(&fast, Mode::Normal, Duration::ZERO, 3);
    let slow = run(&slow, Mode::Normal, Duration::from_millis(5), 3);
    let fast_events =
        std::fs::read(std::path::Path::new(&fast.bundle_path).join("events/segment-000.ndjson"))
            .unwrap();
    let slow_events =
        std::fs::read(std::path::Path::new(&slow.bundle_path).join("events/segment-000.ndjson"))
            .unwrap();
    assert_eq!(fast_events, slow_events);
}

#[test]
fn explicit_loss_and_cleanup_failure_never_publish_complete_integrity() {
    let _env = crate::test_env::lock_env();
    for mode in [Mode::Loss, Mode::CleanupFailed] {
        let harness = harness();
        let result = run(&harness, mode, Duration::ZERO, 4);
        assert_ne!(result.integrity, Integrity::Complete);
        assert_ne!(result.operation_outcome, OperationOutcome::Completed);
    }
}

#[test]
fn disconnect_preserves_only_a_valid_unverifiable_prefix() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let result = run(&harness, Mode::Disconnect, Duration::ZERO, 4);
    assert_eq!(result.integrity, Integrity::Unverifiable);
    assert_eq!(result.events, 2);
    assert_eq!(result.final_execution_state, FinalExecutionState::Unknown);

    let mut second = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );
    assert!(matches!(
        record_window(
            &mut second,
            harness.store.clone(),
            request(harness.output.path(), 1),
            RequestCancellation::default(),
            &mut |_| {},
        ),
        Err(RecordingError::Capsule(
            CaptureCapsuleError::ActiveCapture { .. }
        ))
    ));
}

#[test]
fn explicit_prearm_rejection_is_fast_safe_and_does_not_quarantine_the_generation() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Reject,
        Duration::ZERO,
    );
    let started = std::time::Instant::now();
    let error = record_window(
        &mut link,
        harness.store.clone(),
        request(harness.output.path(), 4),
        RequestCancellation::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RecordingError::Link(LinkError::Emulator { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(1));

    let capsule: CaptureCapsule = harness
        .store
        .read_capture_json(harness.port, &harness.launch_id)
        .unwrap()
        .unwrap();
    assert!(capsule.generation_mutation_blocker().is_none());

    let result = run(&harness, Mode::Normal, Duration::ZERO, 1);
    assert_eq!(result.integrity, Integrity::Complete);
}

#[test]
fn cancellation_is_bound_to_the_exact_capture_and_yields_no_complete_claim() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let cancellation = RequestCancellation::default();
    let trigger = cancellation.clone();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancelled_observer = Arc::clone(&cancelled);
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::from_millis(2),
    );
    let result = record_window(
        &mut link,
        harness.store.clone(),
        request(harness.output.path(), 20),
        cancellation,
        &mut move |progress| {
            if progress.events == 2
                && !cancelled_observer.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                trigger.cancel();
            }
        },
    )
    .unwrap();
    assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(result.operation_outcome, OperationOutcome::Aborted);
    assert_ne!(result.integrity, Integrity::Complete);
}

#[test]
fn cancellation_before_dispatch_has_no_guest_or_staging_effect() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let cancellation = RequestCancellation::default();
    cancellation.cancel();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );

    assert!(matches!(
        record_window(
            &mut link,
            harness.store.clone(),
            request(harness.output.path(), 20),
            cancellation,
            &mut |_| {},
        ),
        Err(RecordingError::Link(LinkError::Cancelled))
    ));
    assert!(harness
        .store
        .read_capture_json::<CaptureCapsule>(harness.port, &harness.launch_id)
        .unwrap()
        .is_none());
    assert_eq!(std::fs::read_dir(harness.output.path()).unwrap().count(), 0);
}

#[test]
fn generation_change_never_publishes_or_relabels_the_old_capture() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );
    let store = harness.store.clone();
    let current_path = store.current_path(harness.port);
    let mut changed = false;
    let error = record_window(
        &mut link,
        harness.store.clone(),
        request(harness.output.path(), 4),
        RequestCancellation::default(),
        &mut |progress| {
            if progress.events == 1 && !changed {
                let mut replacement = store.read_current(harness.port).unwrap().unwrap();
                replacement.launch_id = "launch-01replacement".into();
                std::fs::write(&current_path, serde_json::to_vec(&replacement).unwrap()).unwrap();
                changed = true;
            }
        },
    )
    .unwrap_err();

    assert!(matches!(error, RecordingError::Capsule(_)));
    assert!(changed);
    assert!(!std::fs::read_dir(harness.output.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("capture-")));
    let capsule: CaptureCapsule = harness
        .store
        .read_capture_json(harness.port, &harness.launch_id)
        .unwrap()
        .unwrap();
    assert!(!capsule.state.is_terminal());
}

#[test]
fn invalid_requests_have_no_staging_or_guest_effect() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let mut link = SyntheticLink::new(
        harness.port,
        &harness.launch_id,
        &harness.content,
        Mode::Normal,
        Duration::ZERO,
    );
    let error = record_window(
        &mut link,
        harness.store.clone(),
        request(harness.output.path(), 301),
        RequestCancellation::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(error, RecordingError::Invalid(_)));
    assert_eq!(std::fs::read_dir(harness.output.path()).unwrap().count(), 0);

    let mut too_short = request(harness.output.path(), 1);
    too_short.limits.as_mut().unwrap().max_host_ms = Some(100);
    let error = record_window(
        &mut link,
        harness.store,
        too_short,
        RequestCancellation::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(error, RecordingError::Invalid(_)));
    assert_eq!(std::fs::read_dir(harness.output.path()).unwrap().count(), 0);
}

#[test]
fn producer_status_cannot_contradict_a_completed_operation() {
    let request = RecordingRequest {
        frames: 1,
        warmup_frames: 0,
        event_classes: capability().identities(&[]).unwrap(),
        event_arming: vec![],
        limits: RecordingLimits {
            max_frames: 1,
            ..capability().limits
        },
        input_movie: None,
        stop_on: None,
        start_on: None,
        initial_snapshots: vec![],
        terminal_snapshots: vec![],
        terminal_state: None,
    };
    let value = json!({
        "status": "failed",
        "capture_id": "capture-test",
        "operation_outcome": "completed",
        "execution_outcome": "target_reached",
        "integrity": "complete",
        "f_start": 100,
        "f_end": 101,
        "final_frame": 101,
        "frames": 1,
        "events": 1,
        "bytes": 1,
        "physical_bytes": 1,
        "dropped": 0,
        "wall_ms": 1,
        "final_execution_state": "frozen",
        "cleanup": {
            "hooks": "released",
            "transient_input": "not_acquired",
            "sink": "released"
        }
    });

    assert!(matches!(
        terminal_validation(
            "capture-test",
            RecordingOrigin::NextFrameBoundary,
            &request,
            false,
            value
        ),
        Err(RecordingError::Terminal(_))
    ));
}

#[test]
fn abandoned_capture_recovery_assigns_publication_only_inside_core() {
    let status = json!({
        "recording": {
            "last": {
                "capture_id": "capture-test",
                "operation_outcome": "failed",
                "execution_outcome": "adapter_error",
                "integrity": "unverifiable",
                "final_execution_state": "frozen",
                "final_frame": 101,
                "counters": {"frames": 1, "events": 1, "bytes": 1, "dropped": 0},
                "cleanup": {
                    "hooks": "not_acquired",
                    "transient_input": "not_acquired",
                    "sink": "released"
                },
                "reason": "connection_closed"
            }
        }
    });

    let terminal = adapter_terminal_from_status(&status, "capture-test")
        .unwrap()
        .unwrap();
    assert_eq!(terminal.publication, PublicationOutcome::Failed);
    assert!(status["recording"]["last"].get("publication").is_none());
}

#[test]
fn adapter_status_preserves_event_stop_facts_for_recovery() {
    let status = json!({
        "recording": {
            "last": {
                "capture_id": "capture-stop",
                "operation_outcome": "completed",
                "execution_outcome": "event_stop",
                "integrity": "complete",
                "final_execution_state": "frozen",
                "final_frame": 102,
                "counters": {"frames": 2, "events": 4, "bytes": 512, "dropped": 0},
                "cleanup": {
                    "hooks": "not_acquired",
                    "transient_input": "released",
                    "sink": "released"
                },
                "stop_event": {
                    "sequence": 3,
                    "event_class": "frame_completed",
                    "clock_domain": "frame",
                    "clock_tick": 102,
                    "frame": 101,
                    "occurrence": 2
                },
                "reason": null
            }
        }
    });

    let terminal = adapter_terminal_from_status(&status, "capture-stop")
        .unwrap()
        .unwrap();
    assert_eq!(terminal.execution_outcome, ExecutionOutcome::EventStop);
    assert_eq!(terminal.stop_event.unwrap().occurrence, 2);
    assert_eq!(terminal.publication, PublicationOutcome::Failed);
}

#[test]
fn host_sink_failure_or_partial_record_downgrades_an_otherwise_complete_terminal() {
    let mut validation = RecordingValidationInput {
        request: RecordingRequest {
            frames: 1,
            warmup_frames: 0,
            event_classes: capability().identities(&[]).unwrap(),
            event_arming: vec![],
            limits: RecordingLimits {
                max_frames: 1,
                ..capability().limits
            },
            input_movie: None,
            stop_on: None,
            start_on: None,
            initial_snapshots: vec![],
            terminal_snapshots: vec![],
            terminal_state: None,
        },
        origin: RecordingOrigin::NextFrameBoundary,
        f_start: 100,
        f_end: 101,
        observation_start: None,
        terminal: ProducerTerminalReport {
            operation_outcome: OperationOutcome::Completed,
            execution_outcome: ExecutionOutcome::TargetReached,
            claimed_integrity: Integrity::Complete,
            final_execution_state: FinalExecutionState::Frozen,
            final_frame: 101,
            f_origin: None,
            counters: RecordingCounters {
                frames: 1,
                events: 1,
                bytes: 1,
                dropped: 0,
            },
            loss: LossFacts {
                dropped: 0,
                truncated: false,
                first_sequence_gap: None,
            },
            cleanup: CleanupFacts {
                hooks: CleanupState::Released,
                transient_input: CleanupState::NotAcquired,
                sink: CleanupState::Released,
            },
            stop_event: None,
            reason: None,
            event_classes: Vec::new(),
        },
    };

    apply_sink_outcome(
        &mut validation,
        &SinkOutcome {
            events: 1,
            bytes: 1,
            first_frame: Some(100),
            last_frame: Some(100),
            truncated: true,
            error: Some("injected sync failure".into()),
        },
    );

    assert_eq!(
        validation.terminal.operation_outcome,
        OperationOutcome::Failed
    );
    assert_eq!(
        validation.terminal.claimed_integrity,
        Integrity::Unverifiable
    );
    assert_eq!(validation.terminal.cleanup.sink, CleanupState::Unverifiable);
    assert!(validation.terminal.loss.truncated);
    assert!(validation
        .terminal
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("partial final record")
            && reason.contains("injected sync failure")));
}

#[test]
fn publication_failure_preserves_the_already_observed_execution_terminal() {
    let report = ProducerTerminalReport {
        operation_outcome: OperationOutcome::Completed,
        execution_outcome: ExecutionOutcome::TargetReached,
        claimed_integrity: Integrity::Complete,
        final_execution_state: FinalExecutionState::Frozen,
        final_frame: 101,
        f_origin: None,
        counters: RecordingCounters {
            frames: 1,
            events: 1,
            bytes: 128,
            dropped: 0,
        },
        loss: LossFacts {
            dropped: 0,
            truncated: false,
            first_sequence_gap: None,
        },
        cleanup: CleanupFacts {
            hooks: CleanupState::Released,
            transient_input: CleanupState::NotAcquired,
            sink: CleanupState::Released,
        },
        stop_event: None,
        reason: None,
        event_classes: Vec::new(),
    };

    let terminal = failed_publication_summary(&report, "manifest sync failed".into());
    assert_eq!(terminal.operation_outcome, OperationOutcome::Completed);
    assert_eq!(terminal.execution_outcome, ExecutionOutcome::TargetReached);
    assert_eq!(terminal.final_execution_state, FinalExecutionState::Frozen);
    assert_eq!(terminal.final_frame, 101);
    assert_eq!(terminal.cleanup, report.cleanup);
    assert_eq!(terminal.integrity, Integrity::Unverifiable);
    assert_eq!(terminal.publication, PublicationOutcome::Failed);
}

#[test]
fn local_setup_failure_closes_the_capsule_and_quarantines_staging() {
    let _env = crate::test_env::lock_env();
    let harness = harness();
    let capture_id = "capture-setup-failure";
    let staging =
        crate::bundle::publish::RecordingStaging::prepare(harness.output.path(), capture_id)
            .unwrap();
    let staging_path = staging.staging_path().to_path_buf();
    let repository =
        CaptureCapsuleRepository::new(harness.store.clone(), harness.port, &harness.launch_id);
    repository
        .create(super::capture_capsule::CapturePreparation {
            capture_id: capture_id.into(),
            request_digest_sha256: "11".repeat(32),
            capability_revision: capability().revision,
            output_root: harness.output.path().to_path_buf(),
            destination_path: staging.destination_path().to_path_buf(),
            staging_path: staging_path.clone(),
            lease: CaptureLeaseIdentity::current(),
        })
        .unwrap();
    repository
        .transition(
            capture_id,
            CaptureState::Prepared,
            CaptureState::Arming,
            None,
        )
        .unwrap();
    let mut staging = Some(staging);

    terminalize_setup_failure(
        &repository,
        capture_id,
        CaptureState::Arming,
        &mut staging,
        "injected setup failure".into(),
    )
    .unwrap();

    assert!(staging.is_none());
    assert!(!staging_path.exists());
    assert_eq!(
        repository.read().unwrap().unwrap().state,
        CaptureState::PublicationFailed
    );
    assert!(std::fs::read_dir(harness.output.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".capture-setup-failure.invalid-")));
}
