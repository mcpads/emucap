use super::*;
use crate::test_env::{lock_env, EnvGuard};

#[cfg(unix)]
fn make_runnable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, b"fixture").unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(windows)]
fn make_runnable(path: &Path) {
    std::fs::write(path, b"fixture").unwrap();
}

fn write_pinned_build(repo: &Path) -> PathBuf {
    let adapter = repo.join("adapters/openmsx");
    let binary_dir = adapter.join("work/openmsx-21.0/derived/test/openMSX.app/Contents/MacOS");
    std::fs::create_dir_all(&binary_dir).unwrap();
    std::fs::write(
        adapter.join("upstream.lock"),
        "OPENMSX_VERSION=21.0\n\
         OPENMSX_URL=https://example.invalid/openmsx.tar.gz\n\
         OPENMSX_SHA256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
         OPENMSX_SDL2_COMPAT_PATCH_URL=https://example.invalid/compat.patch\n\
         OPENMSX_SDL2_COMPAT_PATCH_SHA256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
         OPENMSX_HOST_API=1\n",
    )
    .unwrap();
    let binary = binary_dir.join(if cfg!(windows) {
        "openmsx.exe"
    } else {
        "openmsx"
    });
    make_runnable(&binary);
    std::fs::write(
        binary_dir.join("emucap-openmsx-build.json"),
        serde_json::to_vec(&BuildMetadata {
            upstream: "https://example.invalid/openmsx.tar.gz".into(),
            version: "21.0".into(),
            host_api: REQUIRED_HOST_API,
            archive_sha256: "a".repeat(64),
            sdl2_compat_patch_sha256: "b".repeat(64),
            native_patch: false,
        })
        .unwrap(),
    )
    .unwrap();
    binary
}

#[test]
fn compatible_build_requires_exact_pinned_stock_sidecar() {
    let repo = tempfile::tempdir().unwrap();
    let binary = write_pinned_build(repo.path());
    assert!(require_compatible_build(repo.path(), &binary).is_ok());

    let sidecar = binary.parent().unwrap().join("emucap-openmsx-build.json");
    let mut metadata: BuildMetadata =
        serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    metadata.native_patch = true;
    std::fs::write(sidecar, serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(require_compatible_build(repo.path(), &binary).is_err());
}

#[test]
fn resolver_finds_only_repo_build_or_explicit_override() {
    let _lock = lock_env();
    let _guard = EnvGuard::new(&["EMUCAP_OPENMSX_BIN"]);
    std::env::remove_var("EMUCAP_OPENMSX_BIN");
    let repo = tempfile::tempdir().unwrap();
    let binary = write_pinned_build(repo.path());
    assert_eq!(resolve_binary(repo.path()), Some(binary.clone()));

    let override_binary = repo.path().join(if cfg!(windows) {
        "custom.exe"
    } else {
        "custom"
    });
    make_runnable(&override_binary);
    std::env::set_var("EMUCAP_OPENMSX_BIN", &override_binary);
    assert_eq!(resolve_binary(repo.path()), Some(override_binary));
}

#[test]
fn launch_spec_isolates_and_passes_exact_process_identity_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("openmsx");
    let bridge = temp.path().join("emucap-openmsx-bridge");
    let rom = temp.path().join("game.rom");
    let log = temp.path().join("openmsx.log");
    let runtime_home = temp.path().join("runtime");
    let pid_file = runtime_home.join("emulator.pid");
    let launch = Launch {
        binary: &binary,
        bridge: &bridge,
        repo_root: temp.path(),
        content: &rom,
        log_path: &log,
        port: 47901,
        name: Some("msx-test"),
        session_token: Some("token"),
        build: Some("abc123"),
        runtime: None,
        display: false,
    };
    let spec = launch_spec(&launch, &runtime_home, &pid_file);
    assert_eq!(
        spec.args,
        vec![
            "47901",
            binary.to_str().unwrap(),
            rom.to_str().unwrap(),
            runtime_home.to_str().unwrap(),
            "0",
            pid_file.to_str().unwrap(),
        ]
    );
    assert!(spec
        .env
        .contains(&("EMUCAP_SESSION_TOKEN".into(), "token".into())));
    assert!(spec
        .env
        .contains(&("EMUCAP_BUILD_HASH".into(), "abc123".into())));
}

#[test]
fn content_validation_accepts_only_first_profile_cartridges() {
    let temp = tempfile::tempdir().unwrap();
    for extension in ["rom", "mx1", "mx2", "ri", "sg"] {
        let path = temp.path().join(format!("test.{extension}"));
        std::fs::write(&path, b"rom").unwrap();
        assert!(validate_content(&path).is_ok(), ".{extension}");
    }
    for extension in ["dsk", "cas", "zip"] {
        let path = temp.path().join(format!("test.{extension}"));
        std::fs::write(&path, b"media").unwrap();
        assert!(validate_content(&path).is_err(), ".{extension}");
    }
}
