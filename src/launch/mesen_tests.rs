use super::*;
use crate::test_env::{lock_env, EnvGuard};
use serde_json::{json, Value};

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn with_emu_home<T>(base: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    std::env::set_var("EMUCAP_EMU_HOME", base);
    f()
}

#[test]
fn copy_file_replace_replaces_runtime_copy_without_touching_source() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::write(&src, b"new").unwrap();
    std::fs::write(&dst, b"old").unwrap();

    crate::launch::copy_file_replace(&src, &dst).unwrap();

    assert_eq!(std::fs::read(&dst).unwrap(), b"new");
    assert_eq!(std::fs::read(&src).unwrap(), b"new");
}

#[cfg(target_os = "macos")]
#[test]
fn default_install_candidates_include_macos_app() {
    assert!(default_install_candidates().contains(&PathBuf::from(
        "/Applications/Mesen.app/Contents/MacOS/Mesen"
    )));
}

#[test]
fn resolve_binary_accepts_explicit_app_bundle_path() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["MESEN_BIN"]);
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Mesen.app");
    let binary = app.join("Contents/MacOS/Mesen");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"fake mesen").unwrap();
    #[cfg(unix)]
    make_executable(&binary);

    std::env::set_var("MESEN_BIN", &app);
    let resolved = resolve_binary(dir.path());

    assert_eq!(resolved, Some(binary));
}

#[cfg(windows)]
#[test]
fn default_install_candidates_include_windows_user_installs() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["LOCALAPPDATA"]);
    let base = PathBuf::from(r"C:\Users\alice\AppData\Local");
    std::env::set_var("LOCALAPPDATA", &base);

    let candidates = default_install_candidates();

    assert!(candidates.contains(&base.join("Programs/Mesen/Mesen.exe")));
}

#[test]
fn portable_plain_binary_writes_only_emucap_home() {
    let src = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let source_bin = src.path().join("Mesen");
    let source_settings = src.path().join("settings.json");
    std::fs::write(&source_bin, "fake mesen").unwrap();
    std::fs::write(
        &source_settings,
        serde_json::to_string(&json!({"Video": {"Scale": 3}})).unwrap(),
    )
    .unwrap();

    let portable = with_emu_home(emu_home.path(), || {
        prepare_portable_binary(&source_bin, 47911).unwrap()
    });

    assert_eq!(portable.home, emu_home.path().join("mesen2/47911"));
    assert_eq!(portable.binary, portable.home.join("portable/Mesen"));
    assert_eq!(
        portable.settings,
        portable.home.join("portable/settings.json")
    );
    assert!(portable.binary.is_file());
    assert_eq!(
        read(&source_settings),
        json!({"Video": {"Scale": 3}}),
        "source settings must remain untouched"
    );
    // plain 바이너리 copy는 binary만 옮기고 settings.json은 만들지 않는다(우리가 주입하지
    // 않음 — Mesen이 사용자 기본 settings를 로드하고 필수값은 CLI override로 들어간다).
    assert!(!portable.settings.exists());
}

fn test_build_metadata() -> BuildMetadata {
    BuildMetadata {
        upstream: "https://example.invalid/MesenCE.git".into(),
        tag: "2.2.1".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        host_api: REQUIRED_HOST_API,
        patchset_sha256: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
    }
}

#[test]
fn build_metadata_rejects_host_without_safe_halt_savestates() {
    let publish = tempfile::tempdir().unwrap();
    let binary = publish.path().join("Mesen");
    std::fs::write(&binary, "fake").unwrap();
    let mut metadata = test_build_metadata();
    metadata.host_api = 1;
    std::fs::write(
        publish.path().join("emucap-mesen-build.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();

    let error = read_build_metadata(&binary).unwrap_err();

    assert!(error.to_string().contains("host API 1 is incompatible"));
    assert!(error.to_string().contains("expected 2"));
}

#[test]
fn portable_patched_publish_copies_runtime_dependencies_and_sidecar() {
    let src = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let source_bin = src.path().join("Mesen");
    std::fs::write(&source_bin, "fake mesen").unwrap();
    std::fs::write(src.path().join("Mesen.dll"), "dependency").unwrap();
    std::fs::write(
        src.path().join("emucap-mesen-build.json"),
        serde_json::to_vec(&test_build_metadata()).unwrap(),
    )
    .unwrap();

    let portable = with_emu_home(emu_home.path(), || {
        prepare_portable_binary(&source_bin, 47915).unwrap()
    });

    assert_eq!(portable.binary, portable.home.join("portable/Mesen"));
    assert_eq!(
        std::fs::read_to_string(portable.home.join("portable/Mesen.dll")).unwrap(),
        "dependency"
    );
    assert!(portable
        .home
        .join("portable/emucap-mesen-build.json")
        .is_file());
}

#[test]
fn repository_lock_rejects_sidecar_from_another_revision() {
    let root = tempfile::tempdir().unwrap();
    let publish = tempfile::tempdir().unwrap();
    let binary = publish.path().join("Mesen");
    std::fs::write(&binary, "fake").unwrap();
    let metadata = test_build_metadata();
    std::fs::write(
        publish.path().join("emucap-mesen-build.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    let adapter = root.path().join("adapters/mesen2");
    std::fs::create_dir_all(&adapter).unwrap();
    std::fs::write(
            adapter.join("upstream.lock"),
            format!(
                "MESEN_REPO={}\nMESEN_TAG={}\nMESEN_COMMIT={}\nMESEN_HOST_API={}\nMESEN_PATCHSET_SHA256={}\n",
                metadata.upstream,
                metadata.tag,
                "ffffffffffffffffffffffffffffffffffffffff",
                metadata.host_api,
                metadata.patchset_sha256
            ),
        )
        .unwrap();

    let error = require_compatible_build(root.path(), &binary).unwrap_err();
    assert!(error.to_string().contains("mesen-patch-required"));
}

#[cfg(unix)]
#[test]
fn portable_plain_binary_refuses_symlink_inside_emucap_home() {
    let outside = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let target_bin = outside.path().join("Mesen");
    std::fs::write(&target_bin, "user mesen").unwrap();
    make_executable(&target_bin);
    let portable_dir = emu_home.path().join("mesen2/47913/portable");
    std::fs::create_dir_all(&portable_dir).unwrap();
    let portable_link = portable_dir.join("Mesen");
    std::os::unix::fs::symlink(&target_bin, &portable_link).unwrap();

    let err = with_emu_home(emu_home.path(), || {
        prepare_portable_binary(&portable_link, 47913)
    })
    .unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(std::fs::symlink_metadata(&portable_link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_to_string(&target_bin).unwrap(), "user mesen");
    assert!(!portable_dir.join("settings.json").exists());
}

#[cfg(unix)]
#[test]
fn portable_plain_binary_refuses_symlinked_parent_inside_emucap_home() {
    let outside = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let outside_portable = outside.path().join("portable-target");
    std::fs::create_dir_all(&outside_portable).unwrap();
    let target_bin = outside_portable.join("Mesen");
    std::fs::write(&target_bin, "user mesen").unwrap();
    make_executable(&target_bin);
    let port_home = emu_home.path().join("mesen2/47914");
    std::fs::create_dir_all(&port_home).unwrap();
    let portable_link = port_home.join("portable");
    std::os::unix::fs::symlink(&outside_portable, &portable_link).unwrap();
    let apparent_binary = portable_link.join("Mesen");

    let err = with_emu_home(emu_home.path(), || {
        prepare_portable_binary(&apparent_binary, 47914)
    })
    .unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read_to_string(&target_bin).unwrap(), "user mesen");
    assert!(!outside_portable.join("settings.json").exists());
}

#[test]
fn portable_app_bundle_copies_bundle_and_keeps_source_settings() {
    let src = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let app = src.path().join("Mesen.app");
    let source_bin = app.join("Contents/MacOS/Mesen");
    let source_info = app.join("Contents/Info.plist");
    let source_settings = app.join("Contents/MacOS/settings.json");
    let source_resource = app.join("Contents/Resources/icon.txt");
    std::fs::create_dir_all(source_bin.parent().unwrap()).unwrap();
    std::fs::create_dir_all(source_resource.parent().unwrap()).unwrap();
    std::fs::write(&source_bin, "fake app mesen").unwrap();
    std::fs::write(
        &source_info,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>ca.mesen</string>
</dict></plist>
"#,
    )
    .unwrap();
    std::fs::write(&source_resource, "resource").unwrap();
    std::fs::write(
        &source_settings,
        serde_json::to_string(&json!({"Video": {"Scale": 4}})).unwrap(),
    )
    .unwrap();

    let portable = with_emu_home(emu_home.path(), || {
        prepare_portable_binary(&source_bin, 47912).unwrap()
    });

    assert_eq!(
        portable.binary,
        emu_home
            .path()
            .join("mesen2/47912/Mesen.app/Contents/MacOS/Mesen")
    );
    assert_eq!(
        portable.settings,
        emu_home
            .path()
            .join("mesen2/47912/Mesen.app/Contents/MacOS/settings.json")
    );
    assert!(portable
        .home
        .join("Mesen.app/Contents/Resources/icon.txt")
        .is_file());
    assert_eq!(
        read(&source_settings),
        json!({"Video": {"Scale": 4}}),
        "source app settings must remain untouched"
    );
    #[cfg(target_os = "macos")]
    {
        let portable_info = portable.home.join("Mesen.app/Contents/Info.plist");
        let portable_plist = std::fs::read_to_string(portable_info).unwrap();
        let source_plist = std::fs::read_to_string(source_info).unwrap();
        assert!(portable_plist.contains("ca.mesen.emucap.p47912"));
        assert!(source_plist.contains("<string>ca.mesen</string>"));
        assert!(!source_plist.contains("ca.mesen.emucap"));
    }
    // app bundle copy는 source .app의 settings.json을 그대로 옮길 뿐, 우리가 키를 주입하지
    // 않는다(필수값은 CLI override). source에 있던 값은 유지되고 우리 키는 없다.
    let v = read(&portable.settings);
    assert_eq!(v["Video"]["Scale"], json!(4));
    assert!(
        v.get("Debug").is_none(),
        "settings.json에 우리 키를 주입하지 않는다"
    );
}

/// Run `f` with `EMUCAP_EMU_HOME` and `EMUCAP_GBA_BIOS` set as given (both restored after),
/// under the shared env lock so it does not race other env-touching tests.
fn with_gba_env<T>(emu_home: &Path, gba_bios: Option<&Path>, f: impl FnOnce() -> T) -> T {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME", "EMUCAP_GBA_BIOS"]);
    std::env::set_var("EMUCAP_EMU_HOME", emu_home);
    match gba_bios {
        Some(p) => std::env::set_var("EMUCAP_GBA_BIOS", p),
        None => std::env::remove_var("EMUCAP_GBA_BIOS"),
    }
    f()
}

/// Build the (`Launch`, `PreparedPortable`) inputs `provision_gba_bios` needs. `portable.binary`
/// lives at `<root>/portable/Mesen`, so its `Firmware` dir is `<root>/portable/Firmware`.
fn gba_provision_inputs<'a>(
    root: &Path,
    lua: &'a Path,
    log: &'a Path,
) -> (Launch<'a>, PreparedPortable) {
    let bindir = root.join("portable");
    let portable = PreparedPortable {
        binary: bindir.join("Mesen"),
        settings: bindir.join("settings.json"),
        home: root.to_path_buf(),
    };
    let l = Launch {
        binary: Path::new("/unused/source/Mesen"),
        content: "/unused/rom.gba",
        lua,
        log_path: log,
        port: 47800,
        name: None,
        session_token: None,
        runtime: None,
    };
    (l, portable)
}

#[test]
fn gba_materializes_minimal_portable_settings_for_firmware_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path().join("emucap-gba.lua");
    let log = dir.path().join("launch.log");
    let (_, portable) = gba_provision_inputs(dir.path(), &lua, &log);

    ensure_portable_settings(&portable).unwrap();

    let settings = read(&portable.settings);
    assert_eq!(settings["Debug"]["ScriptWindow"]["AllowIoOsAccess"], true);
    assert_eq!(
        settings["Debug"]["ScriptWindow"]["AllowNetworkAccess"],
        true
    );
    assert_eq!(settings["Debug"]["ScriptWindow"]["ScriptTimeout"], 60);
    assert_eq!(settings["Preferences"]["SingleInstance"], false);
    assert_eq!(
        portable.settings.parent(),
        portable.binary.parent(),
        "settings and Firmware must resolve from the same portable data directory"
    );
}

#[test]
fn portable_setup_preserves_existing_settings() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path().join("emucap-gba.lua");
    let log = dir.path().join("launch.log");
    let (_, portable) = gba_provision_inputs(dir.path(), &lua, &log);
    std::fs::create_dir_all(portable.settings.parent().unwrap()).unwrap();
    std::fs::write(&portable.settings, br#"{"Video":{"Scale":4}}"#).unwrap();

    ensure_portable_settings(&portable).unwrap();

    assert_eq!(read(&portable.settings), json!({"Video": {"Scale": 4}}));
}

#[test]
fn non_gba_also_gets_the_portable_settings_marker() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path().join("emucap-snes.lua");
    let log = dir.path().join("launch.log");
    let (_, portable) = gba_provision_inputs(dir.path(), &lua, &log);

    ensure_portable_settings(&portable).unwrap();

    assert!(portable.settings.is_file());
    assert_eq!(
        read(&portable.settings)["Preferences"]["SingleInstance"],
        false
    );
}

#[cfg(unix)]
#[test]
fn portable_setup_refuses_symlinked_settings() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let lua = dir.path().join("emucap-gba.lua");
    let log = dir.path().join("launch.log");
    let (_, portable) = gba_provision_inputs(dir.path(), &lua, &log);
    std::fs::create_dir_all(portable.settings.parent().unwrap()).unwrap();
    let target = outside.path().join("settings.json");
    std::fs::write(&target, b"user settings").unwrap();
    std::os::unix::fs::symlink(&target, &portable.settings).unwrap();

    let err = ensure_portable_settings(&portable).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(&target).unwrap(), b"user settings");
}

#[test]
fn default_gba_bios_source_is_shared_firmware_dir_not_per_port() {
    let dir = tempfile::tempdir().unwrap();
    with_gba_env(dir.path(), None, || {
        assert_eq!(
            default_gba_bios_source(),
            dir.path().join("firmware/gba_bios.bin"),
            "default BIOS source must be the shared <home>/firmware, not under mesen2/<port>"
        );
    });
}

#[test]
fn provision_stages_bios_from_default_shared_firmware_source() {
    let dir = tempfile::tempdir().unwrap();
    // BIOS at the documented shared location, EMUCAP_GBA_BIOS unset.
    let src = dir.path().join("firmware/gba_bios.bin");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    let bios = vec![0xA5; 0x4000];
    std::fs::write(&src, &bios).unwrap();
    let lua = dir.path().join("emucap-gba.lua");
    let log = dir.path().join("launch.log");
    let (l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
    std::fs::create_dir_all(portable.binary.parent().unwrap()).unwrap();

    with_gba_env(dir.path(), None, || provision_gba_bios(&l, &portable)).unwrap();

    let staged = portable
        .binary
        .parent()
        .unwrap()
        .join("Firmware/gba_bios.bin");
    assert_eq!(std::fs::read(&staged).unwrap(), bios);
}

#[test]
fn provision_accepts_already_staged_bios_when_source_gone_and_unset() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path().join("emucap-gba.lua");
    let log = dir.path().join("launch.log");
    let (l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
    // A BIOS was staged by a prior run; the shared source dir does NOT exist now.
    let staged = portable
        .binary
        .parent()
        .unwrap()
        .join("Firmware/gba_bios.bin");
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    let bios = vec![0x5A; 0x4000];
    std::fs::write(&staged, &bios).unwrap();

    with_gba_env(dir.path(), None, || provision_gba_bios(&l, &portable)).unwrap();

    // Accepted as-is: not overwritten, not failed.
    assert_eq!(std::fs::read(&staged).unwrap(), bios);
}

#[test]
fn provision_fails_fast_when_explicit_source_missing_even_if_staged() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path().join("emucap-gba.lua");
    let log = dir.path().join("launch.log");
    let (l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
    // Even with a staged BIOS, an explicitly-configured but missing source must fail fast.
    let staged = portable
        .binary
        .parent()
        .unwrap()
        .join("Firmware/gba_bios.bin");
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    std::fs::write(&staged, b"PRIORRUN").unwrap();
    let missing = dir.path().join("nowhere/gba_bios.bin");

    let err = with_gba_env(dir.path(), Some(&missing), || {
        provision_gba_bios(&l, &portable)
    })
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn provision_rejects_wrong_sized_bios_before_launch() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path().join("emucap-gba.lua");
    let log = dir.path().join("launch.log");
    let (l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
    let bios = dir.path().join("wrong-size.bin");
    std::fs::write(&bios, b"not a GBA BIOS").unwrap();

    let err = with_gba_env(dir.path(), Some(&bios), || {
        provision_gba_bios(&l, &portable)
    })
    .unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(!portable
        .binary
        .parent()
        .unwrap()
        .join("Firmware/gba_bios.bin")
        .exists());
}

#[test]
fn provision_skips_non_gba_lua_entry() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path().join("emucap-snes.lua");
    let log = dir.path().join("launch.log");
    let (mut l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
    l.content = "/unused/rom.sfc";
    // No BIOS anywhere, but a non-GBA entry must not attempt provisioning.
    with_gba_env(dir.path(), None, || provision_gba_bios(&l, &portable)).unwrap();
}
