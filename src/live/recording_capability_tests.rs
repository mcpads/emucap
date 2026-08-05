use serde_json::json;

use super::recording_capability::*;
use crate::bundle::recording_manifest::RecordingLimits;
use crate::event_contracts::EventContractRegistry;

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
            progress_interval_ms: 250,
        },
    };
    capability.revision = capability.computed_revision().unwrap();
    capability
}

#[test]
fn validates_the_initial_generic_capability_and_defaults() {
    let registry = EventContractRegistry::builtin().unwrap();
    let capability = capability();
    assert_eq!(capability.revision, INITIAL_RECORDING_CAPABILITY_REVISION);
    capability.validate(&registry).unwrap();
    let identities = capability.identities(&[]).unwrap();
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].id, "frame_boundary");
}

#[test]
fn rejects_unknown_or_mismatched_contracts_and_expansive_limits() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut value = serde_json::to_value(capability()).unwrap();
    value["event_classes"][0]["contract_sha256"] = json!("00".repeat(32));
    assert!(RecordingCapability::from_hello(Some(&value), &registry).is_err());

    let mut expansive = capability();
    expansive.limits.max_frames = CORE_MAX_RECORDING_FRAMES + 1;
    expansive.revision = expansive.computed_revision().unwrap();
    assert!(expansive.validate(&registry).is_err());

    let mut value = serde_json::to_value(capability()).unwrap();
    value["event_classes"][0]["id"] = json!("platform_private");
    assert!(RecordingCapability::from_hello(Some(&value), &registry).is_err());
}

#[test]
fn accepts_the_shared_long_window_ceiling_without_changing_initial_profiles() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    capability.limits.max_frames = CORE_MAX_RECORDING_FRAMES;
    capability.limits.max_host_ms = CORE_MAX_RECORDING_HOST_MS;
    capability.input_movie = Some(RecordingInputMovieCapability {
        format: INPUT_MOVIE_FORMAT.into(),
        port: 0,
        max_frames: CORE_MAX_RECORDING_FRAMES,
        max_bytes: CORE_MAX_INPUT_MOVIE_BYTES,
        max_buttons_per_frame: 32,
    });
    capability.revision = capability.computed_revision().unwrap();
    capability.validate(&registry).unwrap();

    assert_eq!(capability.limits.max_frames, 5_000);
    assert_eq!(capability.limits.max_host_ms, 250_000);
    assert_eq!(capability.input_movie.unwrap().max_frames, 5_000);
}

#[test]
fn terminal_snapshot_bounds_are_optional_revision_material() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    let previous = capability.revision.clone();
    capability.terminal_snapshots = Some(RecordingTerminalSnapshotCapability {
        max_members: CORE_MAX_TERMINAL_SNAPSHOT_MEMBERS,
        max_member_bytes: CORE_MAX_TERMINAL_SNAPSHOT_MEMBER_BYTES,
        max_total_bytes: CORE_MAX_TERMINAL_SNAPSHOT_TOTAL_BYTES,
    });
    capability.revision = capability.computed_revision().unwrap();
    assert_ne!(capability.revision, previous);
    capability.validate(&registry).unwrap();

    capability.terminal_snapshots.as_mut().unwrap().max_members =
        CORE_MAX_TERMINAL_SNAPSHOT_MEMBERS + 1;
    capability.revision = capability.computed_revision().unwrap();
    assert!(capability.validate(&registry).is_err());
}

#[test]
fn terminal_state_profiles_are_bounded_and_contract_bound() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    let groups = vec!["ppu".into()];
    capability.terminal_state = Some(RecordingTerminalStateCapability {
        max_bytes: CORE_MAX_TERMINAL_STATE_BYTES,
        profiles: vec![RecordingTerminalStateProfile {
            id: "snes_ppu".into(),
            contract_sha256: terminal_state_contract_sha256(&groups).unwrap(),
            groups,
        }],
    });
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "7a63f4233406541101fdd078a4bd6ffbd1a9785efc24664355a6b491bd8f0efd"
    );
    capability.validate(&registry).unwrap();

    capability.terminal_snapshots = Some(RecordingTerminalSnapshotCapability {
        max_members: 8,
        max_member_bytes: 128 * 1024,
        max_total_bytes: 1024 * 1024,
    });
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "ea526265eb6a5d6b229d568d2bfe7df503adc54032959a9f73eb361b5e6ade3f"
    );
    capability.validate(&registry).unwrap();

    capability.terminal_state.as_mut().unwrap().profiles[0].contract_sha256 = "00".repeat(32);
    capability.revision = capability.computed_revision().unwrap();
    assert!(capability.validate(&registry).is_err());
}

#[test]
fn guest_emission_order_is_explicit_revision_material() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    let unordered_revision = capability.revision.clone();
    capability.event_order = Some(RecordingEventOrder::GuestEmission);
    capability.revision = capability.computed_revision().unwrap();
    assert_ne!(capability.revision, unordered_revision);
    capability.validate(&registry).unwrap();

    let mut value = serde_json::to_value(capability).unwrap();
    value["event_order"] = json!("record_arrival");
    assert!(RecordingCapability::from_hello(Some(&value), &registry).is_err());
}

#[test]
fn absence_is_distinct_from_an_invalid_advertisement() {
    let registry = EventContractRegistry::builtin().unwrap();
    assert!(RecordingCapability::from_hello(None, &registry)
        .unwrap()
        .is_none());
    assert!(RecordingCapability::from_hello(Some(&json!({})), &registry).is_err());
}

#[test]
fn validates_independently_advertised_reset_movie_and_event_stop() {
    let registry = EventContractRegistry::builtin().unwrap();
    let completed = registry.identities(["frame_completed"]).unwrap().remove(0);
    let mut capability = capability();
    capability
        .origins
        .push(RecordingCapabilityOrigin::ResetRelease);
    capability.event_classes.push(RecordingEventCapability {
        id: completed.id,
        contract_sha256: completed.contract_sha256,
        clock_domains: vec!["frame".into()],
        exact: true,
        stoppable: true,
        startable: false,
    });
    capability.input_movie = Some(RecordingInputMovieCapability {
        format: INPUT_MOVIE_FORMAT.into(),
        port: 0,
        max_frames: 300,
        max_bytes: CORE_MAX_INPUT_MOVIE_BYTES,
        max_buttons_per_frame: 32,
    });
    capability.revision = capability.computed_revision().unwrap();
    capability.validate(&registry).unwrap();
    assert_eq!(
        capability.revision,
        "7daa3baefbe5f7b3455d52827349ff943841e6f5e3b572aacd1ef94245b85272"
    );

    capability.origins = vec![RecordingCapabilityOrigin::NextFrameBoundary];
    capability.revision = capability.computed_revision().unwrap();
    capability.validate(&registry).unwrap();
    assert_eq!(
        capability.revision,
        "fb6acdcc8e5d8a9ff21f49f22c2d08425d414d5d39af52ee04553a578a0b1224"
    );
}

#[test]
fn mesen_terminal_snapshot_capability_revisions_cover_base_and_semantic_classes() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    capability.origins = vec![
        RecordingCapabilityOrigin::NextFrameBoundary,
        RecordingCapabilityOrigin::ResetRelease,
    ];
    capability.class_accounting = true;
    capability.event_order = Some(RecordingEventOrder::GuestEmission);
    capability.event_classes.push(RecordingEventCapability {
        id: "frame_completed".into(),
        contract_sha256: registry
            .identities(["frame_completed"])
            .unwrap()
            .remove(0)
            .contract_sha256,
        clock_domains: vec!["frame".into()],
        exact: true,
        stoppable: true,
        startable: false,
    });
    capability.input_movie = Some(RecordingInputMovieCapability {
        format: INPUT_MOVIE_FORMAT.into(),
        port: 0,
        max_frames: 5_000,
        max_bytes: 1024 * 1024,
        max_buttons_per_frame: 32,
    });
    capability.limits = RecordingLimits {
        max_frames: 5_000,
        max_events: 100_000,
        max_bytes: 64 * 1024 * 1024,
        max_line_bytes: 64 * 1024,
        max_host_ms: 250_000,
        progress_interval_ms: 250,
    };
    capability.warmup = Some(RecordingWarmupCapability {
        max_frames: 5_000,
        transaction_event_classes: vec!["frame_boundary".into(), "frame_completed".into()],
    });
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "dea5c89d917c0e645296117dc9b14dcf089a49794dbe72b0319a104016a449bf"
    );
    capability.terminal_snapshots = Some(RecordingTerminalSnapshotCapability {
        max_members: 8,
        max_member_bytes: 128 * 1024,
        max_total_bytes: 1024 * 1024,
    });
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "3314d6344f03df096660917a6087b19a62aece996402ecdd1ee992c87131d0aa"
    );
    for id in ["snes_ppu_obj_evaluation_start", "snes_ppu_obj_handoff"] {
        let identity = registry.identities([id]).unwrap().remove(0);
        capability.event_classes.push(RecordingEventCapability {
            id: identity.id,
            contract_sha256: identity.contract_sha256,
            clock_domains: vec!["snes_master".into()],
            exact: true,
            stoppable: false,
            startable: false,
        });
    }
    let state_groups = vec!["ppu".into()];
    capability.terminal_state = Some(RecordingTerminalStateCapability {
        max_bytes: 128 * 1024,
        profiles: vec![RecordingTerminalStateProfile {
            id: "snes_ppu".into(),
            contract_sha256: terminal_state_contract_sha256(&state_groups).unwrap(),
            groups: state_groups,
        }],
    });
    let mut semantic_without_snapshots = capability.clone();
    semantic_without_snapshots.terminal_snapshots = None;
    semantic_without_snapshots.revision = semantic_without_snapshots.computed_revision().unwrap();
    assert_eq!(
        semantic_without_snapshots.revision,
        "f303cc902eb1006eaab2dbd9c05a739a7184b4a4e2be7890e318f9b8c4b218a2"
    );
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "3360ead44ccebf59a35aefcba6e5846d645188781682293b508a502343212782"
    );
    for id in [
        "snes_cpu_instruction",
        "snes_content_read",
        "snes_transfer_enable",
        "snes_transfer_access",
        "snes_device_port_write",
        "snes_interrupt_delivery",
        "snes_ppu_obj_consumption_read",
    ] {
        let identity = registry.identities([id]).unwrap().remove(0);
        let startable = identity.id == "snes_cpu_instruction";
        capability.event_classes.push(RecordingEventCapability {
            id: identity.id,
            contract_sha256: identity.contract_sha256,
            clock_domains: vec!["snes_master".into()],
            exact: true,
            stoppable: false,
            startable,
        });
    }
    capability.initial_snapshots = Some(RecordingInitialSnapshotCapability {
        memory_types: vec!["snesWorkRam".into()],
        start_positions: vec![RecordingInitialSnapshotPosition::EventAnchor],
        max_members: 1,
        max_member_bytes: 128 * 1024,
        max_total_bytes: 128 * 1024,
        max_callback_ms: 100,
    });
    let mut deep_without_snapshots = capability.clone();
    deep_without_snapshots.terminal_snapshots = None;
    deep_without_snapshots.revision = deep_without_snapshots.computed_revision().unwrap();
    assert_eq!(
        deep_without_snapshots.revision,
        "6f601a701d9a979cde0c118c9fcd4fd4a2d572f529728ca77db5f4ddd99b26b4"
    );
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "cf4f250d1319e642294bc69db241defe50d8a5ff1546f81aac99720b3209890b"
    );
}

#[test]
fn event_aligned_initial_snapshots_require_exact_guest_order_and_bounds() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    let identity = registry
        .identities(["snes_cpu_instruction"])
        .unwrap()
        .remove(0);
    capability.event_classes.push(RecordingEventCapability {
        id: identity.id,
        contract_sha256: identity.contract_sha256,
        clock_domains: vec!["snes_master".into()],
        exact: true,
        stoppable: false,
        startable: true,
    });
    capability.event_order = Some(RecordingEventOrder::GuestEmission);
    capability.class_accounting = true;
    capability.warmup = Some(RecordingWarmupCapability {
        max_frames: capability.limits.max_frames,
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
    capability.validate(&registry).unwrap();

    capability.event_order = None;
    capability.revision = capability.computed_revision().unwrap();
    assert!(capability.validate(&registry).is_err());

    capability.event_order = Some(RecordingEventOrder::GuestEmission);
    capability
        .initial_snapshots
        .as_mut()
        .unwrap()
        .max_callback_ms = CORE_MAX_INITIAL_SNAPSHOT_CALLBACK_MS + 1;
    capability.revision = capability.computed_revision().unwrap();
    assert!(capability.validate(&registry).is_err());
}
