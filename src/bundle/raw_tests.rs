use super::raw::*;

const SAMPLE: &str = r#"{
  "format_version": 1,
  "platform": "snes",
  "rom_path": "roms/game.sfc",
  "adapter": { "name": "mesen2", "version": "0.1" },
  "emulator": { "name": "Mesen2", "version": "2.0" },
  "trigger": { "kind": "retrospective", "at_unix_ms": 100, "at_frame": 1264 },
  "ring_policy": { "interval_frames": 30, "depth": 8 },
  "slices": [
    { "frame": 1234, "artifacts": [
        { "kind": "savestate", "path": "slices/f01234/state.mss" },
        { "kind": "screenshot", "path": "slices/f01234/screen.png" }
    ]}
  ],
  "input_movie": "input.movie"
}"#;

#[test]
fn parses_valid_raw() {
    let raw = parse_raw(SAMPLE).unwrap();
    assert_eq!(raw.rom_path, "roms/game.sfc");
    assert_eq!(raw.slices.len(), 1);
    assert_eq!(raw.slices[0].frame, 1234);
}

#[test]
fn rejects_missing_required_field() {
    // rom_path 누락
    let bad = SAMPLE.replace("\"rom_path\": \"roms/game.sfc\",", "");
    assert!(parse_raw(&bad).is_err(), "rom_path 누락이 에러여야 한다");
}
