use serde_json::json;

use super::recording_capability::*;
use crate::bundle::recording_manifest::{EventArmingScope, RecordingLimits};
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
            filterable_fields: vec![],
        }],
        event_order: None,
        class_accounting: false,
        input_movie: None,
        state_load: None,
        initial_snapshots: None,
        terminal_snapshots: None,
        terminal_state: None,
        warmup: None,
        repeatability: None,
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
fn repeatability_is_bounded_to_declared_origins_and_covered_by_revision() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    capability
        .origins
        .push(RecordingCapabilityOrigin::ResetRelease);
    capability.repeatability = Some(RecordingRepeatabilityCapability {
        profile: "repeatable_test".into(),
        conditions_sha256: "ab".repeat(32),
        origins: vec![RecordingCapabilityOrigin::ResetRelease],
        requires_input_movie: true,
    });
    capability.input_movie = Some(RecordingInputMovieCapability {
        format: INPUT_MOVIE_FORMAT.into(),
        port: 0,
        max_frames: capability.limits.max_frames,
        max_bytes: CORE_MAX_INPUT_MOVIE_BYTES,
        max_buttons_per_frame: 32,
    });
    capability.revision = capability.computed_revision().unwrap();
    capability.validate(&registry).unwrap();

    let revision = capability.revision.clone();
    capability.repeatability.as_mut().unwrap().conditions_sha256 = "cd".repeat(32);
    assert_ne!(capability.computed_revision().unwrap(), revision);
    assert!(capability.validate(&registry).is_err());

    capability.revision = capability.computed_revision().unwrap();
    capability.repeatability.as_mut().unwrap().origins =
        vec![RecordingCapabilityOrigin::NextFrameBoundary];
    capability.revision = capability.computed_revision().unwrap();
    capability.validate(&registry).unwrap();
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
        filterable_fields: vec![],
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
        filterable_fields: vec![],
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
        selectable_event_scopes: vec![
            RecordingEventScopeCapability {
                id: "frame_boundary".into(),
                scopes: vec![EventArmingScope::Transaction, EventArmingScope::Observation],
            },
            RecordingEventScopeCapability {
                id: "frame_completed".into(),
                scopes: vec![EventArmingScope::Transaction, EventArmingScope::Observation],
            },
        ],
    });
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "20520b327e06f8ed30387f20f8609b861ceb4306ac1d955f6fb7a38b7489e885"
    );
    capability.terminal_snapshots = Some(RecordingTerminalSnapshotCapability {
        max_members: 8,
        max_member_bytes: 128 * 1024,
        max_total_bytes: 1024 * 1024,
    });
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "7d673b3f299c2f5f8ba91cf12475385581ec6f18ad38efa1b25a6a3ef7cde08d"
    );
    capability
        .origins
        .push(RecordingCapabilityOrigin::StateLoad);
    capability.state_load = Some(RecordingStateLoadCapability {
        format: "mesen-savestate".into(),
        max_bytes: 64 * 1024 * 1024,
        alignment: RecordingStateLoadAlignment::RestoredFrameBoundary,
        requires_input_movie: true,
    });
    let state_groups = vec!["ppu".into()];
    let mut state_only = capability.clone();
    state_only.terminal_state = Some(RecordingTerminalStateCapability {
        max_bytes: 128 * 1024,
        profiles: vec![RecordingTerminalStateProfile {
            id: "snes_ppu".into(),
            contract_sha256: terminal_state_contract_sha256(&state_groups).unwrap(),
            groups: state_groups,
        }],
    });
    let mut state_only_without_snapshots = state_only.clone();
    state_only_without_snapshots.terminal_snapshots = None;
    state_only_without_snapshots.revision =
        state_only_without_snapshots.computed_revision().unwrap();
    assert_eq!(
        state_only_without_snapshots.revision,
        "89000c4b91750e4ad8317eb234ccc3e75a6c304978feb74daaedda8f2aa3ba7e"
    );
    state_only.revision = state_only.computed_revision().unwrap();
    assert_eq!(
        state_only.revision,
        "76f79723be824e502f5be0f0188c1494b249694c340bc8112a04872845ded730"
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
            filterable_fields: vec![],
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
        "c7bc749b13517456b049a73a868bc662c54cc98e580cc306b873328fd842dc22"
    );
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "151544f37bf2e72601429981e93fb8d45bd404a0e1501d26432b9cf29995e658"
    );
    for id in [
        "snes_cpu_instruction",
        "snes_content_read",
        "snes_transfer_enable",
        "snes_transfer_access",
        "snes_device_port_write",
        "snes_interrupt_delivery",
        "snes_ppu_obj_consumption_read",
        "snes_ppu_cgram_lookup",
        "snes_ppu_bg_chr_fetch",
    ] {
        let identity = registry.identities([id]).unwrap().remove(0);
        let startable = identity.id == "snes_cpu_instruction";
        let stoppable = matches!(
            identity.id.as_str(),
            "snes_ppu_obj_consumption_read" | "snes_ppu_cgram_lookup" | "snes_ppu_bg_chr_fetch"
        );
        let filterable_fields = match identity.id.as_str() {
            "snes_ppu_obj_consumption_read" => vec![
                RecordingEventFilterField {
                    path: "memory_kind".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 1,
                },
                RecordingEventFilterField {
                    path: "address".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 0xffff,
                },
            ],
            "snes_ppu_cgram_lookup" => vec![
                RecordingEventFilterField {
                    path: "address".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 0xff,
                },
                RecordingEventFilterField {
                    path: "layer".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 5,
                },
                RecordingEventFilterField {
                    path: "target".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 2,
                },
                RecordingEventFilterField {
                    path: "pixel_x".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 0xff,
                },
                RecordingEventFilterField {
                    path: "scanline".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 0xffff,
                },
            ],
            "snes_ppu_bg_chr_fetch" => vec![
                RecordingEventFilterField {
                    path: "address".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 0x7fff,
                },
                RecordingEventFilterField {
                    path: "layer".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 3,
                },
                RecordingEventFilterField {
                    path: "scanline".into(),
                    kind: RecordingEventFilterKind::U64Range,
                    min: 0,
                    max: 0xffff,
                },
            ],
            _ => vec![],
        };
        capability.event_classes.push(RecordingEventCapability {
            id: identity.id,
            contract_sha256: identity.contract_sha256,
            clock_domains: vec!["snes_master".into()],
            exact: true,
            stoppable,
            startable,
            filterable_fields,
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
        "9cb6540758c6f4a690371afc92c52803597183466ab0fc3486cbabc9e4287840"
    );
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "79af7faa13666068539eaa7749e021f2035cefca4e5f9c12548a70468abc92ee"
    );
    capability.repeatability = Some(RecordingRepeatabilityCapability {
        profile: "mesen_snes_repeatable".into(),
        conditions_sha256: "b9f4760915a13576fe4fa5c55a75dffd0e79987ac6259cea1bff5a1701826d6b"
            .into(),
        origins: vec![RecordingCapabilityOrigin::ResetRelease],
        requires_input_movie: true,
    });
    capability.revision = capability.computed_revision().unwrap();
    assert_eq!(
        capability.revision,
        "4436231189dd28b27f252d8c1241ccdfc04ead72aca5b1f675c53ad5a6377511"
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
        filterable_fields: vec![],
    });
    capability.event_order = Some(RecordingEventOrder::GuestEmission);
    capability.class_accounting = true;
    capability.warmup = Some(RecordingWarmupCapability {
        max_frames: capability.limits.max_frames,
        transaction_event_classes: vec!["frame_boundary".into()],
        selectable_event_scopes: vec![],
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

#[test]
fn selectable_warmup_event_scopes_are_identity_and_revision_bound() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    capability.class_accounting = true;
    capability.warmup = Some(RecordingWarmupCapability {
        max_frames: capability.limits.max_frames,
        transaction_event_classes: vec!["frame_boundary".into()],
        selectable_event_scopes: vec![RecordingEventScopeCapability {
            id: "frame_boundary".into(),
            scopes: vec![EventArmingScope::Transaction, EventArmingScope::Observation],
        }],
    });
    let previous_revision = capability.revision.clone();
    capability.revision = capability.computed_revision().unwrap();
    assert_ne!(capability.revision, previous_revision);
    capability.validate(&registry).unwrap();

    capability.warmup.as_mut().unwrap().selectable_event_scopes[0].scopes =
        vec![EventArmingScope::Transaction, EventArmingScope::Transaction];
    capability.revision = capability.computed_revision().unwrap();
    assert!(capability.validate(&registry).is_err());
}

#[test]
fn filterable_fields_are_contract_bound_and_revision_covered() {
    let registry = EventContractRegistry::builtin().unwrap();
    let identity = registry
        .identities(["snes_ppu_obj_consumption_read"])
        .unwrap()
        .remove(0);
    let mut capability = capability();
    capability.event_classes.push(RecordingEventCapability {
        id: identity.id,
        contract_sha256: identity.contract_sha256,
        clock_domains: vec!["snes_master".into()],
        exact: true,
        stoppable: false,
        startable: false,
        filterable_fields: vec![RecordingEventFilterField {
            path: "address".into(),
            kind: RecordingEventFilterKind::U64Range,
            min: 0,
            max: 0xffff,
        }],
    });
    let revision_without_filter = {
        let mut unfiltered = capability.clone();
        unfiltered.event_classes[1].filterable_fields.clear();
        unfiltered.computed_revision().unwrap()
    };
    capability.revision = capability.computed_revision().unwrap();
    assert_ne!(capability.revision, revision_without_filter);
    capability.validate(&registry).unwrap();

    capability.event_classes[1].filterable_fields[0].path = "unknown".into();
    capability.revision = capability.computed_revision().unwrap();
    assert!(capability.validate(&registry).is_err());
}

#[test]
fn state_load_origin_requires_one_bounded_capability_and_explicit_input_support() {
    let registry = EventContractRegistry::builtin().unwrap();
    let mut capability = capability();
    capability
        .origins
        .push(RecordingCapabilityOrigin::StateLoad);
    capability.input_movie = Some(RecordingInputMovieCapability {
        format: INPUT_MOVIE_FORMAT.into(),
        port: 0,
        max_frames: capability.limits.max_frames,
        max_bytes: CORE_MAX_INPUT_MOVIE_BYTES,
        max_buttons_per_frame: 32,
    });
    capability.state_load = Some(RecordingStateLoadCapability {
        format: "mesen-savestate".into(),
        max_bytes: CORE_MAX_RECORDING_STATE_BYTES,
        alignment: RecordingStateLoadAlignment::RestoredFrameBoundary,
        requires_input_movie: true,
    });
    capability.revision = capability.computed_revision().unwrap();
    capability.validate(&registry).unwrap();

    capability.input_movie = None;
    capability.revision = capability.computed_revision().unwrap();
    assert!(capability.validate(&registry).is_err());

    capability.state_load = None;
    capability.revision = capability.computed_revision().unwrap();
    assert!(capability.validate(&registry).is_err());
}
