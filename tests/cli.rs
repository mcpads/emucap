use assert_cmd::Command;
use std::fs;
use std::path::Path;

fn make_valid_bundle(dir: &Path) {
    fs::create_dir_all(dir.join("slices/f01234")).unwrap();
    fs::write(dir.join("slices/f01234/state.mss"), b"state").unwrap();
    fs::write(dir.join("slices/f01234/screen.png"), b"png").unwrap();
    fs::write(dir.join("game.sfc"), b"abc").unwrap();
    let raw = serde_json::json!({
        "format_version": 1,
        "platform": "snes",
        "rom_path": dir.join("game.sfc").display().to_string(),
        "adapter": { "name": "mesen2", "version": "0.1" },
        "emulator": { "name": "Mesen2", "version": "2.0" },
        "trigger": { "kind": "retrospective", "at_unix_ms": 100, "at_frame": 1264 },
        "ring_policy": { "interval_frames": 30, "depth": 8 },
        "slices": [{
            "frame": 1234,
            "artifacts": [
                { "kind": "savestate", "path": "slices/f01234/state.mss" },
                { "kind": "screenshot", "path": "slices/f01234/screen.png" }
            ]
        }],
        "input_movie": null
    });
    fs::write(
        dir.join("_raw.json"),
        serde_json::to_vec_pretty(&raw).unwrap(),
    )
    .unwrap();
}

#[test]
fn finalize_then_inspect_json() {
    let tmp = tempfile::tempdir().unwrap();
    make_valid_bundle(tmp.path());

    Command::cargo_bin("emucap")
        .unwrap()
        .arg("finalize")
        .arg(tmp.path())
        .assert()
        .success();

    assert!(tmp.path().join("manifest.json").exists());

    Command::cargo_bin("emucap")
        .unwrap()
        .arg("inspect")
        .arg(tmp.path())
        .arg("--json")
        .assert()
        .success()
        .stdout(predicates::str::contains("\"slice_count\": 1"));
}

#[test]
fn finalize_fails_on_missing_raw() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("emucap")
        .unwrap()
        .arg("finalize")
        .arg(tmp.path())
        .assert()
        .failure();
}
