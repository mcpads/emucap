use super::*;
use crate::test_env::{lock_env, EnvGuard};

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn copy_run_binary_replaces_existing_copy() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::write(&src, b"new").unwrap();
    std::fs::write(&dst, b"old").unwrap();

    copy_run_binary(&src, &dst).unwrap();

    assert_eq!(std::fs::read(&dst).unwrap(), b"new");
    assert_eq!(std::fs::read(&src).unwrap(), b"new");
}

#[test]
fn repo_local_candidate_is_platform_native() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("adapters/mednafen/work/mednafen/src");
    std::fs::create_dir_all(&src).unwrap();
    let expected = src.join(default_binary_name());
    std::fs::write(&expected, b"fake mednafen").unwrap();
    #[cfg(unix)]
    make_executable(&expected);

    assert_eq!(repo_local_binary(dir.path()).unwrap(), expected);
}

#[cfg(windows)]
#[test]
fn default_install_candidates_include_windows_user_installs() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["LOCALAPPDATA"]);
    let base = PathBuf::from(r"C:\Users\alice\AppData\Local");
    std::env::set_var("LOCALAPPDATA", &base);

    let candidates = default_install_candidates();

    assert!(candidates.contains(&base.join("Programs/Mednafen/mednafen.exe")));
}

#[test]
fn run_copy_preserves_source_binary_name() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join(if cfg!(windows) {
        "mednafen.exe"
    } else {
        "mednafen"
    });
    let run_dir = dir.path().join("run");

    assert_eq!(
        run_binary_path(&src, &run_dir),
        run_dir.join(default_binary_name())
    );
}

#[test]
fn build_sidecar_binds_the_exact_mednafen_binary() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join(default_binary_name());
    std::fs::write(&binary, b"maintained mednafen").unwrap();
    let binary_sha256 = hex::encode(Sha256::digest(b"maintained mednafen"));
    let metadata = BuildMetadata {
        upstream: "https://example.invalid/mednafen.tar.xz".into(),
        upstream_revision: "1.2.3@sha256:archive".into(),
        patchset_sha256: "a".repeat(64),
        binary_sha256,
    };
    std::fs::write(
        metadata_path(&binary),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();

    assert_eq!(read_build_metadata(&binary).unwrap(), Some(metadata));
}

#[test]
fn build_sidecar_rejects_a_replaced_binary() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join(default_binary_name());
    std::fs::write(&binary, b"first").unwrap();
    let metadata = BuildMetadata {
        upstream: "https://example.invalid/mednafen.tar.xz".into(),
        upstream_revision: "1.2.3@sha256:archive".into(),
        patchset_sha256: "a".repeat(64),
        binary_sha256: hex::encode(Sha256::digest(b"first")),
    };
    std::fs::write(
        metadata_path(&binary),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    std::fs::write(&binary, b"replacement").unwrap();

    let error = read_build_metadata(&binary).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(error.to_string().contains("binary_sha256 does not match"));
}

#[test]
fn pcfx_bios_resolution_rejects_wrong_hash_after_size_check() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_PCFX_BIOS", "EMUCAP_EMU_HOME"]);
    let dir = tempfile::tempdir().unwrap();
    let bios = dir.path().join("pcfx.rom");
    // The hash check is tested independently below; this fixture first proves the size gate.
    std::fs::write(&bios, vec![0u8; PCFX_BIOS_SIZE as usize]).unwrap();
    std::env::set_var("EMUCAP_PCFX_BIOS", &bios);

    let error = resolve_pcfx_bios().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(error.to_string().contains("sha256="));
}

#[test]
fn pcfx_bios_resolution_rejects_relative_override_before_io() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_PCFX_BIOS"]);
    std::env::set_var("EMUCAP_PCFX_BIOS", "pcfx.rom");

    let error = resolve_pcfx_bios().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn runtime_home_stages_known_firmware_without_reading_user_profile() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME", "EMUCAP_MEDNAFEN_FIRMWARE", "HOME"]);
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("operator-firmware");
    let fake_user = dir.path().join("user");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(fake_user.join(".mednafen/firmware")).unwrap();
    std::fs::write(shared.join("sega_101.bin"), b"managed Saturn BIOS").unwrap();
    std::fs::write(
        fake_user.join(".mednafen/firmware/sega_101.bin"),
        b"user Saturn BIOS",
    )
    .unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", dir.path().join("emucap"));
    std::env::set_var("EMUCAP_MEDNAFEN_FIRMWARE", &shared);
    std::env::set_var("HOME", &fake_user);

    let home = prepare_runtime_home(47800).unwrap();

    assert_eq!(home, dir.path().join("emucap/mednafen/47800"));
    assert_eq!(
        std::fs::read(home.join("firmware/sega_101.bin")).unwrap(),
        b"managed Saturn BIOS"
    );
    assert_eq!(
        std::fs::read(fake_user.join(".mednafen/firmware/sega_101.bin")).unwrap(),
        b"user Saturn BIOS"
    );
}

#[test]
fn runtime_home_allows_an_absent_default_firmware_inventory() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME", "EMUCAP_MEDNAFEN_FIRMWARE"]);
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", dir.path().join("emucap"));
    std::env::remove_var("EMUCAP_MEDNAFEN_FIRMWARE");

    let home = prepare_runtime_home(47801).unwrap();

    assert!(home.join("firmware").is_dir());
}

#[test]
fn explicit_firmware_inventory_must_be_absolute_and_exist() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME", "EMUCAP_MEDNAFEN_FIRMWARE"]);
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", dir.path().join("emucap"));
    std::env::set_var("EMUCAP_MEDNAFEN_FIRMWARE", "relative-firmware");
    assert_eq!(
        prepare_runtime_home(47802).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );

    std::env::set_var(
        "EMUCAP_MEDNAFEN_FIRMWARE",
        dir.path().join("missing-firmware"),
    );
    assert_eq!(
        prepare_runtime_home(47802).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[cfg(unix)]
#[test]
fn runtime_home_refuses_a_symlinked_port_directory() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME", "EMUCAP_MEDNAFEN_FIRMWARE"]);
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let base = dir.path().join("emucap");
    std::fs::create_dir_all(base.join("mednafen")).unwrap();
    std::os::unix::fs::symlink(outside.path(), base.join("mednafen/47803")).unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", &base);
    std::env::remove_var("EMUCAP_MEDNAFEN_FIRMWARE");

    let error = prepare_runtime_home(47803).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}
