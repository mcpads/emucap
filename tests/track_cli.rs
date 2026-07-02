use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

#[test]
fn reindex_then_ls_empty_is_ok() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("emucap")
        .unwrap()
        .env("EMUCAP_TRACK_ROOT", dir.path())
        .args(["track", "reindex"])
        .assert()
        .success();
    Command::cargo_bin("emucap")
        .unwrap()
        .env("EMUCAP_TRACK_ROOT", dir.path())
        .args(["track", "ls"])
        .assert()
        .success()
        .stdout(contains("runs: 0"));
}

#[test]
fn import_bundle_then_ls_shows_run() {
    let track = TempDir::new().unwrap();
    let bundle = TempDir::new().unwrap();
    // 최소 manifest.json
    let manifest = serde_json::json!({
        "format_version": 1, "platform": "snes",
        "rom": {"sha1": "romsha", "path_hint": "game.sfc"},
        "adapter": {"name": "mesen2", "version": "1"},
        "emulator": {"name": "mesen2", "version": "2"},
        "trigger": {"kind": "retrospective", "at_unix_ms": 0, "at_frame": 10},
        "ring_policy": {"interval_frames": 30, "depth": 8},
        "slices": [{"frame": 10, "artifacts": [{"kind": "screenshot", "path": "slices/f10/screen.png"}]}],
        "input_movie": null
    });
    std::fs::write(bundle.path().join("manifest.json"), manifest.to_string()).unwrap();

    Command::cargo_bin("emucap")
        .unwrap()
        .env("EMUCAP_TRACK_ROOT", track.path())
        .args(["track", "import", bundle.path().to_str().unwrap()])
        .assert()
        .success();

    Command::cargo_bin("emucap")
        .unwrap()
        .env("EMUCAP_TRACK_ROOT", track.path())
        .args(["track", "ls"])
        .assert()
        .success()
        .stdout(contains("romsha"))
        .stdout(contains("runs: 1"));
}
