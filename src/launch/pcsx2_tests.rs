use super::*;
use crate::test_env::{lock_env, EnvGuard};

fn launch_for<'a>(binary: &'a Path, bridge: &'a Path, bios: &'a Path, log: &'a Path) -> Launch<'a> {
    Launch {
        binary,
        bridge,
        bios,
        content: "/games/test.iso",
        log_path: log,
        port: 47870,
        name: Some("ps2-test"),
        session_token: Some("token"),
        runtime: None,
        display: false,
    }
}

#[test]
fn session_config_enables_pine_and_uses_bios_in_place() {
    let _lock = lock_env();
    let _guard = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let temporary = tempfile::tempdir().unwrap();
    let emulator_home = temporary.path().join("emulators");
    let bios_directory = temporary.path().join("operator-bios");
    std::fs::create_dir_all(&bios_directory).unwrap();
    let bios = bios_directory.join("SCPH-10000.bin");
    std::fs::write(&bios, vec![0u8; 4 * 1024 * 1024]).unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", &emulator_home);

    let prepared = prepare_session(47870, &bios, 39001).unwrap();
    let ini = std::fs::read_to_string(prepared.data_root.join("inis/PCSX2.ini")).unwrap();
    assert!(ini.contains("SetupWizardIncomplete = false"));
    assert!(ini.contains("Language = en-US"));
    assert!(ini.contains("EnablePINE = true"));
    assert!(ini.contains("PINESlot = 39001"));
    assert!(ini.contains(&format!("Bios = {}", bios_directory.display())));
    assert!(ini.contains("BIOS = SCPH-10000.bin"));
    assert!(!prepared.data_root.join("bios/SCPH-10000.bin").exists());
}

#[test]
fn emulator_spec_isolates_data_and_places_flags_before_content() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("pcsx2-qt");
    let bridge = dir.path().join("bridge");
    let bios = dir.path().join("bios.bin");
    let log = dir.path().join("pcsx2.log");
    let launch = launch_for(&binary, &bridge, &bios, &log);
    let prepared = PreparedSession {
        home: dir.path().join("home"),
        data_root: dir.path().join("data"),
        pine_runtime: dir.path().join("pine"),
        pine_socket: pine_socket_path(&dir.path().join("pine"), 39001),
    };
    let spec = emulator_spec(&launch, &prepared, 39001);
    assert_eq!(
        spec.args,
        vec![
            "-batch",
            "-fastboot",
            "-nofullscreen",
            "-nogui",
            "--",
            "/games/test.iso"
        ]
    );
    assert_eq!(
        spec.env
            .iter()
            .find(|(key, _)| key == "EMUCAP_PCSX2_DATAROOT")
            .map(|(_, value)| PathBuf::from(value)),
        Some(prepared.data_root.clone())
    );
    let bridge_spec = bridge_spec(&launch, &prepared, 39001);
    assert_eq!(
        bridge_spec
            .env
            .iter()
            .find(|(key, _)| key == "EMUCAP_PCSX2_CAPTURE_DIR")
            .map(|(_, value)| PathBuf::from(value)),
        Some(prepared.data_root.join("captures"))
    );
}

#[test]
fn build_sidecar_must_match_the_pinned_lock() {
    let root = tempfile::tempdir().unwrap();
    let adapter = root.path().join("adapters/pcsx2");
    let bin = root.path().join("build/pcsx2-qt");
    std::fs::create_dir_all(&adapter).unwrap();
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(
        adapter.join("upstream.lock"),
        "PCSX2_REPO=https://example.invalid/pcsx2.git\n\
         PCSX2_COMMIT=1111111111111111111111111111111111111111\n\
         PCSX2_PATCHES_REPO=https://example.invalid/pcsx2-patches.git\n\
         PCSX2_PATCHES_COMMIT=2222222222222222222222222222222222222222\n\
         PCSX2_PATCHES_TREE=3333333333333333333333333333333333333333\n\
         PCSX2_PATCHES_ARCHIVE_SHA256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
         PCSX2_HOST_API=3\n\
         PCSX2_PATCHSET_SHA256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    )
    .unwrap();
    std::fs::write(
        build_metadata_path(&bin),
        serde_json::to_vec(&BuildMetadata {
            upstream: "https://example.invalid/pcsx2.git".into(),
            commit: "1111111111111111111111111111111111111111".into(),
            patches_upstream: "https://example.invalid/pcsx2-patches.git".into(),
            patches_commit: "2222222222222222222222222222222222222222".into(),
            patches_tree: "3333333333333333333333333333333333333333".into(),
            patches_archive_sha256:
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            host_api: REQUIRED_HOST_API,
            patchset_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(require_compatible_build(root.path(), &bin).is_ok());

    let mut mismatch: BuildMetadata =
        serde_json::from_slice(&std::fs::read(build_metadata_path(&bin)).unwrap()).unwrap();
    mismatch.host_api = REQUIRED_HOST_API + 1;
    std::fs::write(
        build_metadata_path(&bin),
        serde_json::to_vec(&mismatch).unwrap(),
    )
    .unwrap();
    assert!(require_compatible_build(root.path(), &bin)
        .unwrap_err()
        .to_string()
        .contains("host API 4 is incompatible"));
}
