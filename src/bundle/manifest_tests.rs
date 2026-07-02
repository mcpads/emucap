use super::manifest::*;

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
