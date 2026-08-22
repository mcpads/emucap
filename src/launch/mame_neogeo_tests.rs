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

#[test]
fn mvs_driver_is_the_zip_stem_and_rejects_other_media() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("mslug.zip");
    std::fs::write(&zip, b"set").unwrap();
    assert_eq!(mvs_driver(&zip).unwrap(), "mslug");
    let rom = dir.path().join("mslug.rom");
    std::fs::write(&rom, b"set").unwrap();
    assert!(mvs_driver(&rom).is_err());
}

#[test]
fn aes_software_name_is_the_zip_stem_and_rejects_other_media() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("mslug2.zip");
    std::fs::write(&zip, b"set").unwrap();
    assert_eq!(aes_software_name(&zip).unwrap(), "mslug2");
    let rom = dir.path().join("mslug2.bin");
    std::fs::write(&rom, b"set").unwrap();
    assert!(aes_software_name(&rom).is_err());
}

#[test]
fn sibling_bios_is_discovered_without_global_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("game.zip");
    let bios = dir.path().join("neogeo.zip");
    std::fs::write(&game, b"game").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    assert!(default_bios_candidates(&game, "neogeo_mvs")
        .into_iter()
        .any(|candidate| candidate == bios));
}

#[test]
fn sibling_aes_bios_is_discovered_for_aes_only() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("game.zip");
    let bios = dir.path().join("aes.zip");
    std::fs::write(&game, b"game").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    assert!(default_bios_candidates(&game, "neogeo_aes")
        .into_iter()
        .any(|candidate| candidate == bios));
    assert!(!default_bios_candidates(&game, "neogeo_mvs")
        .into_iter()
        .any(|candidate| candidate == bios));
}

#[test]
fn sibling_cd_bios_is_discovered_for_cd_only() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    let bios = dir.path().join("neocdz.zip");
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    assert!(default_bios_candidates(&cue, "neogeo_cd")
        .into_iter()
        .any(|candidate| candidate == bios));
    assert!(!default_bios_candidates(&cue, "neogeo_mvs")
        .into_iter()
        .any(|candidate| candidate == bios));
}

#[test]
fn resolver_uses_the_neogeo_build_without_reusing_the_pc98_subset() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_NEOGEO_MAME_BIN", "MAME_BIN"]);
    std::env::remove_var("EMUCAP_NEOGEO_MAME_BIN");
    std::env::remove_var("MAME_BIN");
    let root = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) { "mame.exe" } else { "mame" };
    let pc98 = root.path().join("adapters/mame-pc98/work").join(name);
    let neogeo = root.path().join("adapters/mame-neogeo/work").join(name);
    std::fs::create_dir_all(pc98.parent().unwrap()).unwrap();
    std::fs::create_dir_all(neogeo.parent().unwrap()).unwrap();
    make_runnable(&pc98);
    make_runnable(&neogeo);

    assert_eq!(resolve_binary(root.path()), Some(neogeo));
}

#[test]
fn headless_spec_uses_isolated_home_and_neogeo_profile() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game.zip");
    let bios = root.path().join("neogeo.zip");
    std::fs::write(&game, b"game").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    let log = root.path().join("mame.log");
    let launch = Launch {
        binary: Path::new("/mame"),
        bridge: Path::new("/bridge"),
        repo_root: root.path(),
        content: &game,
        bios: &bios,
        system: "neogeo_mvs",
        log_path: &log,
        port: 47822,
        name: None,
        session_token: None,
        runtime: None,
        display: false,
    };
    let spec = mame_spec(&launch, "game", 48822).unwrap();
    assert!(spec.args.windows(2).any(|v| v == ["-video", "none"]));
    assert!(spec.args.iter().any(|v| v == "-noreadconfig"));
    assert!(spec
        .env
        .iter()
        .any(|(key, value)| { key == "EMUCAP_MAME_PROFILE" && value == "neogeo_mvs" }));
    assert!(spec.args.iter().any(|v| v.contains("mame-neogeo/47822")));
    let bridge = bridge_spec(&launch, gdb_port(launch.port).unwrap()).unwrap();
    assert!(bridge.env.iter().any(|(key, value)| {
        key == "EMUCAP_ADAPTER_HOME" && value.contains("mame-neogeo/47822")
    }));
}

#[test]
fn bridge_uses_the_runtime_generation_for_temporary_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game.zip");
    let bios = root.path().join("neogeo.zip");
    std::fs::write(&game, b"game").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    let log = root.path().join("mame.log");
    let generation = root.path().join("sessions/47822/generations/launch-test");
    let failure = generation.join("adapter-failure.json");
    let runtime = RuntimeEnv {
        launch_id: "launch-test",
        adapter_failure_path: &failure,
    };
    let launch = Launch {
        binary: Path::new("/mame"),
        bridge: Path::new("/bridge"),
        repo_root: root.path(),
        content: &game,
        bios: &bios,
        system: "neogeo_mvs",
        log_path: &log,
        port: 47822,
        name: None,
        session_token: None,
        runtime: Some(runtime),
        display: false,
    };

    let bridge = bridge_spec(&launch, gdb_port(launch.port).unwrap()).unwrap();
    assert!(bridge
        .env
        .iter()
        .any(|(key, value)| { key == "EMUCAP_ADAPTER_HOME" && Path::new(value) == generation }));
}

#[test]
fn visible_spec_explicitly_authorizes_the_safe_wrapper() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game.zip");
    let bios = root.path().join("neogeo.zip");
    std::fs::write(&game, b"game").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    let log = root.path().join("mame.log");
    let launch = Launch {
        binary: Path::new("/mame"),
        bridge: Path::new("/bridge"),
        repo_root: root.path(),
        content: &game,
        bios: &bios,
        system: "neogeo_mvs",
        log_path: &log,
        port: 47823,
        name: None,
        session_token: None,
        runtime: None,
        display: true,
    };
    let spec = mame_spec(&launch, "game", 48823).unwrap();
    assert!(spec
        .env
        .contains(&("MAME_ALLOW_VISIBLE".to_string(), "1".to_string())));
    assert!(!spec
        .env
        .iter()
        .any(|(key, value)| key == "SDL_VIDEODRIVER" && value == "dummy"));
    for forbidden in [
        "-video",
        "-videodriver",
        "-keyboardprovider",
        "-mouseprovider",
        "-output",
    ] {
        assert!(
            !spec.args.iter().any(|arg| arg == forbidden),
            "visible Neo Geo MAME spec contains headless option {forbidden}"
        );
    }
}

#[test]
fn aes_spec_uses_software_list_cartridge_and_a_separate_home() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("mslug2.zip");
    let bios = root.path().join("aes.zip");
    let hash_path = root.path().join("adapters/mame-neogeo/work/hash");
    std::fs::create_dir_all(&hash_path).unwrap();
    std::fs::write(
        hash_path.join("neogeo.xml"),
        br#"<softwarelist><software name="mslug2">
<sharedfeat name="compatibility" value="MVS,AES" />
</software></softwarelist>"#,
    )
    .unwrap();
    std::fs::write(&game, b"game").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    let log = root.path().join("mame.log");
    let launch = Launch {
        binary: Path::new("/mame"),
        bridge: Path::new("/bridge"),
        repo_root: root.path(),
        content: &game,
        bios: &bios,
        system: "neogeo_aes",
        log_path: &log,
        port: 47824,
        name: None,
        session_token: None,
        runtime: None,
        display: false,
    };
    let spec = mame_spec(&launch, "aes", 48824).unwrap();
    assert_eq!(spec.args[0], "aes");
    assert!(spec.args.windows(2).any(|pair| pair == ["-bios", "japan"]));
    assert!(spec.args.windows(2).any(|pair| pair == ["-cart", "mslug2"]));
    assert!(spec
        .args
        .windows(2)
        .any(|pair| pair[0] == "-hashpath" && pair[1] == hash_path.to_string_lossy()));
    assert!(spec
        .env
        .contains(&("EMUCAP_MAME_PROFILE".into(), "neogeo_aes".into())));
    assert!(spec
        .args
        .iter()
        .any(|value| value.contains("mame-neogeo-aes/47824")));
}

#[test]
fn aes_spec_rejects_missing_or_mvs_only_software_before_launch() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("mvs_only.zip");
    let bios = root.path().join("aes.zip");
    let hash_path = root.path().join("adapters/mame-neogeo/work/hash");
    std::fs::create_dir_all(&hash_path).unwrap();
    std::fs::write(
        hash_path.join("neogeo.xml"),
        br#"<softwarelist><software name="mvs_only">
<sharedfeat name="compatibility" value="MVS" />
</software></softwarelist>"#,
    )
    .unwrap();
    std::fs::write(&game, b"game").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    let log = root.path().join("mame.log");
    let launch = Launch {
        binary: Path::new("/mame"),
        bridge: Path::new("/bridge"),
        repo_root: root.path(),
        content: &game,
        bios: &bios,
        system: "neogeo_aes",
        log_path: &log,
        port: 47824,
        name: None,
        session_token: None,
        runtime: None,
        display: false,
    };
    let error = mame_spec(&launch, "aes", 48824).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("not marked AES-compatible"));

    std::fs::write(hash_path.join("neogeo.xml"), b"<softwarelist />").unwrap();
    let error = mame_spec(&launch, "aes", 48824).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("is absent"));
}

#[test]
fn cd_spec_uses_cdz_driver_cdrom_media_and_a_separate_home() {
    let root = tempfile::tempdir().unwrap();
    let cue = root.path().join("disc.cue");
    let track = root.path().join("track01.bin");
    let bios = root.path().join("neocdz.zip");
    std::fs::write(&cue, "FILE \"track01.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    std::fs::write(&track, b"track").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    let log = root.path().join("mame.log");
    let launch = Launch {
        binary: Path::new("/mame"),
        bridge: Path::new("/bridge"),
        repo_root: root.path(),
        content: &cue,
        bios: &bios,
        system: "neogeo_cd",
        log_path: &log,
        port: 47825,
        name: None,
        session_token: None,
        runtime: None,
        display: false,
    };
    let spec = mame_spec(&launch, "neocdz", 48825).unwrap();
    assert_eq!(spec.args[0], "neocdz");
    assert!(spec
        .args
        .windows(2)
        .any(|pair| pair == ["-bios", "official"]));
    assert!(spec
        .args
        .windows(2)
        .any(|pair| pair[0] == "-cdrom" && pair[1] == cue.to_string_lossy()));
    assert!(spec
        .env
        .contains(&("EMUCAP_MAME_PROFILE".into(), "neogeo_cd".into())));
    assert!(spec
        .args
        .iter()
        .any(|value| value.contains("mame-neogeo-cd/47825")));
}
