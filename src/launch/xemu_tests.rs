use super::*;
use tempfile::TempDir;

fn write_file(path: &Path, size: usize, prefix: &[u8]) {
    let mut data = vec![0_u8; size];
    data[..prefix.len()].copy_from_slice(prefix);
    fs::write(path, data).unwrap();
}

fn firmware_fixture(root: &Path, with_eeprom: bool) -> FirmwareInventory {
    write_file(&root.join("mcpx_1.0.bin"), 512, b"mcpx");
    write_file(&root.join("Complex_4627.bin"), 1024 * 1024, b"flash");
    write_file(&root.join("xbox_hdd.qcow2"), 4096, b"QFI\xfb");
    if with_eeprom {
        write_file(&root.join("xemu_eeprom.bin"), 256, b"eeprom");
    }
    validate_firmware_root(root).unwrap()
}

fn launch_fixture<'a>(
    temporary: &'a TempDir,
    firmware: &'a FirmwareInventory,
    runtime: RuntimeEnv<'a>,
) -> Launch<'a> {
    let content = temporary.path().join("game.xiso");
    fs::write(&content, b"xiso").unwrap();
    let binary = temporary.path().join("xemu");
    let bridge = temporary.path().join("bridge");
    fs::write(&binary, b"binary").unwrap();
    fs::write(&bridge, b"bridge").unwrap();
    let host_build = BuildMetadata {
        upstream: "https://example.invalid/xemu".into(),
        tag: "test".into(),
        commit: "1".repeat(40),
        host_api: REQUIRED_HOST_API,
        patchset_sha256: "2".repeat(64),
        binary_sha256: "3".repeat(64),
    };
    Launch {
        binary: Box::leak(binary.into_boxed_path()),
        bridge: Box::leak(bridge.into_boxed_path()),
        content: Box::leak(content.into_boxed_path()),
        firmware,
        host_build: Box::leak(Box::new(host_build)),
        port: 47800,
        name: Some("xbox-test"),
        build: Some("test-build"),
        session_token: Some("token"),
        runtime: Some(runtime),
        display: false,
        sound: false,
        start_frozen: true,
    }
}

#[test]
fn firmware_validation_separates_machine_file_roles() {
    let temporary = TempDir::new().unwrap();
    let inventory = firmware_fixture(temporary.path(), true);
    assert_eq!(inventory.mcpx_identity.size, 512);
    assert_eq!(inventory.flash_identity.size, 1024 * 1024);
    assert_eq!(inventory.hdd_template_identity.size, 4096);
    assert_eq!(inventory.eeprom_template_identity.unwrap().size, 256);
}

#[test]
fn compatible_build_requires_the_pinned_sidecar_and_binary_digest() {
    let temporary = TempDir::new().unwrap();
    let dist = temporary.path().join("adapters/xemu/work/xemu/dist");
    fs::create_dir_all(&dist).unwrap();
    let binary = dist.join("xemu");
    fs::write(&binary, b"pinned xemu binary").unwrap();
    let binary_sha256 = identity_for_regular_file(&binary).unwrap().sha256;
    let metadata = serde_json::json!({
        "upstream": lock_value("XEMU_REPO").unwrap(),
        "tag": lock_value("XEMU_TAG").unwrap(),
        "commit": lock_value("XEMU_COMMIT").unwrap(),
        "host_api": REQUIRED_HOST_API,
        "patchset_sha256": lock_value("XEMU_PATCHSET_SHA256").unwrap(),
        "binary_sha256": binary_sha256,
    });
    fs::write(
        dist.join("emucap-xemu-build.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    assert_eq!(
        require_compatible_build(temporary.path(), &binary)
            .unwrap()
            .commit,
        lock_value("XEMU_COMMIT").unwrap()
    );

    fs::write(&binary, b"changed binary").unwrap();
    let error = require_compatible_build(temporary.path(), &binary).unwrap_err();
    assert!(error.to_string().contains("binary digest mismatch"));
}

#[test]
fn firmware_validation_rejects_non_qcow_hdd() {
    let temporary = TempDir::new().unwrap();
    write_file(&temporary.path().join("mcpx_1.0.bin"), 512, b"mcpx");
    write_file(
        &temporary.path().join("Complex_4627.bin"),
        256 * 1024,
        b"flash",
    );
    write_file(&temporary.path().join("xbox_hdd.qcow2"), 4096, b"notq");
    let error = validate_firmware_root(temporary.path()).unwrap_err();
    assert!(error.to_string().contains("not a QCOW image"));
}

#[test]
fn generation_preparation_copies_mutable_state_and_writes_isolated_config() {
    let temporary = TempDir::new().unwrap();
    let firmware_root = temporary.path().join("firmware");
    fs::create_dir(&firmware_root).unwrap();
    let firmware = firmware_fixture(&firmware_root, true);
    let emu_home = temporary.path().join("home");
    let failure = temporary.path().join("failure.json");
    let runtime = RuntimeEnv {
        launch_id: "launch-01TEST",
        adapter_failure_path: &failure,
    };
    let launch = launch_fixture(&temporary, &firmware, runtime);
    let prepared = prepare_generation_under(&launch, &emu_home).unwrap();
    assert!(prepared
        .runtime_home
        .ends_with("xemu/47800/generations/launch-01TEST"));
    assert_ne!(prepared.hdd, firmware.hdd_template);
    assert_eq!(
        fs::read(&prepared.hdd).unwrap(),
        fs::read(&firmware.hdd_template).unwrap()
    );
    assert_eq!(fs::read(&prepared.eeprom).unwrap().len(), 256);
    let config = fs::read_to_string(&prepared.settings).unwrap();
    assert!(config.contains("show_welcome = false"));
    assert!(config.contains("[audio]\nvolume_limit = 0.0"));
    assert!(config.contains("[input.bindings]\nport1 = \"keyboard\""));
    assert!(config.contains(&prepared.hdd.display().to_string()));
    assert!(!config.contains(&firmware.root.display().to_string()));
}

#[test]
fn generation_settings_enable_audio_only_when_requested() {
    let temporary = TempDir::new().unwrap();
    let firmware_root = temporary.path().join("firmware");
    fs::create_dir(&firmware_root).unwrap();
    let firmware = firmware_fixture(&firmware_root, true);
    let failure = temporary.path().join("failure.json");
    let runtime = RuntimeEnv {
        launch_id: "launch-01AUDIO",
        adapter_failure_path: &failure,
    };
    let mut launch = launch_fixture(&temporary, &firmware, runtime);
    launch.sound = true;
    let prepared = prepare_generation_under(&launch, &temporary.path().join("home")).unwrap();
    let config = fs::read_to_string(&prepared.settings).unwrap();
    assert!(config.contains("[audio]\nvolume_limit = 1.0"));
}

#[test]
fn emulator_spec_uses_hidden_rendering_and_controlled_start() {
    let temporary = TempDir::new().unwrap();
    let firmware_root = temporary.path().join("firmware");
    fs::create_dir(&firmware_root).unwrap();
    let firmware = firmware_fixture(&firmware_root, true);
    let failure = temporary.path().join("failure.json");
    let runtime = RuntimeEnv {
        launch_id: "launch-01SPEC",
        adapter_failure_path: &failure,
    };
    let launch = launch_fixture(&temporary, &firmware, runtime);
    let prepared = prepare_generation_under(&launch, &temporary.path().join("home")).unwrap();
    let spec = emulator_spec(&launch, &prepared, 48123, 48124);
    assert!(spec.args.iter().any(|argument| argument == "-S"));
    assert!(spec
        .args
        .iter()
        .any(|argument| argument == "-emucap-hidden"));
    assert!(spec
        .args
        .iter()
        .any(|argument| argument == "tcp:127.0.0.1:48123,server=on,wait=off"));
    assert!(spec
        .args
        .iter()
        .any(|argument| argument == "tcp:127.0.0.1:48124"));
    assert!(spec.env.iter().any(|(key, value)| {
        key == "EMUCAP_XEMU_SCREEN_ROOT" && Path::new(value) == prepared.screenshots.as_path()
    }));
}

#[test]
fn bridge_spec_binds_state_storage_and_exact_host_build() {
    let temporary = TempDir::new().unwrap();
    let firmware_root = temporary.path().join("firmware");
    fs::create_dir(&firmware_root).unwrap();
    let firmware = firmware_fixture(&firmware_root, true);
    let failure = temporary.path().join("failure.json");
    let runtime = RuntimeEnv {
        launch_id: "launch-01STATE",
        adapter_failure_path: &failure,
    };
    let launch = launch_fixture(&temporary, &firmware, runtime);
    let prepared = prepare_generation_under(&launch, &temporary.path().join("home")).unwrap();
    let eeprom_identity = identity_for_regular_file(&prepared.eeprom).unwrap();
    let spec = bridge_spec(&launch, &prepared, 48123, 48124, &eeprom_identity);
    let env = spec
        .env
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(Path::new(&env["EMUCAP_XEMU_HDD_PATH"]), prepared.hdd);
    assert_eq!(Path::new(&env["EMUCAP_XEMU_EEPROM_PATH"]), prepared.eeprom);
    assert_eq!(env["EMUCAP_XEMU_HOST_COMMIT"], launch.host_build.commit);
    assert_eq!(
        env["EMUCAP_XEMU_HOST_PATCHSET_SHA256"],
        launch.host_build.patchset_sha256
    );
    assert_eq!(
        env["EMUCAP_XEMU_HOST_BINARY_SHA256"],
        launch.host_build.binary_sha256
    );
}

#[test]
fn generation_ids_cannot_escape_the_managed_root() {
    let temporary = TempDir::new().unwrap();
    let firmware_root = temporary.path().join("firmware");
    fs::create_dir(&firmware_root).unwrap();
    let firmware = firmware_fixture(&firmware_root, true);
    let failure = temporary.path().join("failure.json");
    let runtime = RuntimeEnv {
        launch_id: "../escape",
        adapter_failure_path: &failure,
    };
    let launch = launch_fixture(&temporary, &firmware, runtime);
    let error = prepare_generation_under(&launch, &temporary.path().join("home")).unwrap_err();
    assert!(error.to_string().contains("unsafe characters"));
}
