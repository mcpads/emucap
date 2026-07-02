use super::manifest::*;
use super::summary::*;

fn sample() -> Manifest {
    Manifest {
        format_version: FORMAT_VERSION,
        platform: "snes".into(),
        rom: RomId {
            sha1: "abc123".into(),
            path_hint: None,
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
            at_unix_ms: 0,
            at_frame: 1264,
        },
        ring_policy: RingPolicy {
            interval_frames: 30,
            depth: 8,
        },
        slices: vec![
            Slice {
                frame: 1234,
                artifacts: vec![],
            },
            Slice {
                frame: 1264,
                artifacts: vec![],
            },
        ],
        input_movie: None,
    }
}

#[test]
fn summarize_extracts_fields() {
    let s = summarize(&sample());
    assert_eq!(s.slice_count, 2);
    assert_eq!(s.frames, vec![1234, 1264]);
    assert_eq!(s.trigger_kind, "retrospective");
    assert_eq!(s.rom_sha1, "abc123");
}

#[test]
fn render_json_is_machine_readable() {
    let s = summarize(&sample());
    let json = render_json(&s);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["slice_count"], 2);
    assert_eq!(parsed["frames"][0], 1234);
}

#[test]
fn render_table_mentions_key_facts() {
    let s = summarize(&sample());
    let table = render_table(&s);
    assert!(table.contains("snes"));
    assert!(table.contains("1264"));
    assert!(table.contains("abc123"));
}
