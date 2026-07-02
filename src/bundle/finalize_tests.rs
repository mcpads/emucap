use super::finalize::{finalize, FinalizeError};
use std::fs;
use std::path::Path;

/// 유효한 번들 디렉토리를 tempdir에 구성한다. ROM도 디렉토리 안에 둔다.
fn make_valid_bundle(dir: &Path) {
    fs::create_dir_all(dir.join("slices/f01234")).unwrap();
    fs::write(dir.join("slices/f01234/state.mss"), b"state").unwrap();
    fs::write(dir.join("slices/f01234/screen.png"), b"png").unwrap();
    fs::write(dir.join("game.sfc"), b"abc").unwrap();
    fs::write(dir.join("input.movie"), b"movie").unwrap();
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
        "input_movie": "input.movie"
    });
    fs::write(
        dir.join("_raw.json"),
        serde_json::to_vec_pretty(&raw).unwrap(),
    )
    .unwrap();
}

#[test]
fn finalize_writes_manifest_and_note() {
    let tmp = tempfile::tempdir().unwrap();
    make_valid_bundle(tmp.path());

    let manifest = finalize(tmp.path(), None).unwrap();
    // "abc"의 SHA-1
    assert_eq!(
        manifest.rom.sha1,
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(manifest.slices.len(), 1);
    assert!(tmp.path().join("manifest.json").exists());
    assert!(tmp.path().join("note.md").exists());
}

#[test]
fn finalize_rom_override_takes_precedence() {
    let tmp = tempfile::tempdir().unwrap();
    make_valid_bundle(tmp.path());
    // _raw.json의 rom_path(game.sfc="abc")와 다른 내용의 오버라이드 ROM
    let other = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(other.path(), b"different rom bytes").unwrap();
    let manifest = finalize(tmp.path(), Some(other.path())).unwrap();
    // "abc"의 SHA-1이 아니라 오버라이드 ROM의 해시여야 한다
    assert_ne!(
        manifest.rom.sha1,
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        manifest.rom.path_hint.as_deref(),
        Some(other.path().display().to_string().as_str())
    );
}

#[test]
fn finalize_errors_when_raw_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let err = finalize(tmp.path(), None).unwrap_err();
    assert!(matches!(err, FinalizeError::RawNotFound(_)));
}

#[test]
fn finalize_errors_when_artifact_missing() {
    let tmp = tempfile::tempdir().unwrap();
    make_valid_bundle(tmp.path());
    fs::remove_file(tmp.path().join("slices/f01234/state.mss")).unwrap();
    let err = finalize(tmp.path(), None).unwrap_err();
    assert!(matches!(err, FinalizeError::ArtifactMissing(_)));
}

/// artifact 경로가 번들을 벗어나는(`..`) 번들. 자기완결성 위반이라 거부되어야 한다.
fn make_bundle_with_escaping_artifact(dir: &Path) {
    fs::create_dir_all(dir.join("slices/f01234")).unwrap();
    fs::write(dir.join("game.sfc"), b"abc").unwrap();
    let raw = r#"{
      "format_version": 1, "platform": "snes",
      "rom_path": "game.sfc",
      "adapter": { "name": "mesen2", "version": "0.1" },
      "emulator": { "name": "Mesen2", "version": "2.0" },
      "trigger": { "kind": "retrospective", "at_unix_ms": 100, "at_frame": 1264 },
      "ring_policy": { "interval_frames": 30, "depth": 8 },
      "slices": [ { "frame": 1234, "artifacts": [
          { "kind": "savestate", "path": "../evil.mss" }
      ]} ]
    }"#;
    fs::write(dir.join("_raw.json"), raw).unwrap();
}

#[test]
fn finalize_rejects_artifact_escaping_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    make_bundle_with_escaping_artifact(tmp.path());
    let err = finalize(tmp.path(), None).unwrap_err();
    assert!(
        matches!(err, FinalizeError::ArtifactOutsideBundle(_)),
        "번들 밖을 가리키는 artifact 경로는 거부해야: {err:?}"
    );
}

#[test]
fn finalize_does_not_overwrite_existing_note() {
    let tmp = tempfile::tempdir().unwrap();
    make_valid_bundle(tmp.path());
    fs::write(tmp.path().join("note.md"), "사람이 쓴 내용".as_bytes()).unwrap();
    finalize(tmp.path(), None).unwrap();
    let note = fs::read_to_string(tmp.path().join("note.md")).unwrap();
    assert_eq!(note, "사람이 쓴 내용", "기존 note.md를 덮어쓰면 안 된다");
}
