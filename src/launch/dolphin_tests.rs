use super::*;
use crate::test_env::{lock_env, EnvGuard};

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn metadata() -> BuildMetadata {
    BuildMetadata {
        upstream: "https://github.com/dolphin-emu/dolphin.git".into(),
        commit: "1".repeat(40),
        host_api: REQUIRED_HOST_API,
        patchset_sha256: "2".repeat(64),
    }
}

#[test]
fn build_metadata_rejects_missing_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("Dolphin");
    std::fs::write(&binary, b"fake").unwrap();

    let error = read_build_metadata(&binary).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("dolphin-patch-required"));
}

#[test]
fn build_metadata_accepts_valid_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("Dolphin");
    std::fs::write(&binary, b"fake").unwrap();
    std::fs::write(
        build_metadata_path(&binary),
        serde_json::to_vec(&metadata()).unwrap(),
    )
    .unwrap();

    assert_eq!(read_build_metadata(&binary).unwrap(), metadata());
}

#[test]
fn resolve_binary_accepts_explicit_app_bundle() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&[
        "EMUCAP_DOLPHIN_GUI_BIN",
        "EMUCAP_DOLPHIN_HEADLESS_BIN",
        "EMUCAP_DOLPHIN_BIN",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("DolphinQt.app");
    let binary = app.join("Contents/MacOS/DolphinQt");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"fake").unwrap();
    #[cfg(unix)]
    make_executable(&binary);

    std::env::set_var("EMUCAP_DOLPHIN_GUI_BIN", &app);

    assert_eq!(resolve_binary(dir.path(), true), Some(binary));
}

#[test]
fn runtime_plain_binary_is_copied_under_per_port_home() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let source = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let binary = source.path().join("dolphin-emu-nogui");
    std::fs::write(&binary, b"fake dolphin").unwrap();
    std::fs::write(
        build_metadata_path(&binary),
        serde_json::to_vec(&metadata()).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    make_executable(&binary);
    std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());

    let prepared = prepare_runtime_binary(&binary, 47920).unwrap();

    assert_eq!(
        prepared.binary,
        emu_home
            .path()
            .join("dolphin/47920/runtime/dolphin-emu-nogui")
    );
    assert_eq!(
        prepared.user_dir,
        emu_home.path().join("dolphin/47920/user")
    );
    assert!(prepared
        .home
        .join("runtime/emucap-dolphin-build.json")
        .is_file());
    assert_eq!(std::fs::read(&binary).unwrap(), b"fake dolphin");
}

#[test]
fn runtime_app_bundle_copy_preserves_resources() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let source = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let app = source.path().join("DolphinQt.app");
    let binary = app.join("Contents/MacOS/DolphinQt");
    let resource = app.join("Contents/Resources/qt.conf");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::create_dir_all(resource.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"fake dolphin").unwrap();
    std::fs::write(&resource, b"resource").unwrap();
    #[cfg(unix)]
    make_executable(&binary);
    std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());

    let prepared = prepare_runtime_binary(&binary, 47921).unwrap();

    assert_eq!(
        prepared.binary,
        emu_home
            .path()
            .join("dolphin/47921/runtime/DolphinQt.app/Contents/MacOS/DolphinQt")
    );
    assert_eq!(
        std::fs::read(
            emu_home
                .path()
                .join("dolphin/47921/runtime/DolphinQt.app/Contents/Resources/qt.conf")
        )
        .unwrap(),
        b"resource"
    );
}

#[cfg(unix)]
#[test]
fn runtime_refuses_symlinked_binary_inside_owned_home() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let outside = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let target = outside.path().join("Dolphin");
    std::fs::write(&target, b"user dolphin").unwrap();
    make_executable(&target);
    let runtime = emu_home.path().join("dolphin/47922/runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let link = runtime.join("Dolphin");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());

    let error = prepare_runtime_binary(&link, 47922).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(&target).unwrap(), b"user dolphin");
}
