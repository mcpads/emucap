use super::*;

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
fn sibling_bios_is_discovered_without_global_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("game.zip");
    let bios = dir.path().join("neogeo.zip");
    std::fs::write(&game, b"game").unwrap();
    std::fs::write(&bios, b"bios").unwrap();
    assert!(default_bios_candidates(&game)
        .into_iter()
        .any(|candidate| candidate == bios));
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
        .any(|(key, value)| { key == "EMUCAP_MAME_PROFILE" && value == "neogeo" }));
    assert!(spec.args.iter().any(|v| v.contains("mame-neogeo/47822")));
}
