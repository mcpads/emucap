use super::*;
use std::path::Path;

fn opts<'a>(content: &'a str) -> SpecOpts<'a> {
    SpecOpts {
        content,
        port: 47800,
        name: None,
        session_token: None,
        runtime: None,
        headless: true,
    }
}

#[test]
fn saturn_spec_has_module_and_content_and_headless() {
    let spec = mednafen_spec(
        Path::new("/run/mednafen"),
        Path::new("/tmp/m.log"),
        Some("ss"),
        false,
        &opts("game.cue"),
    );
    assert_eq!(
        spec.args,
        vec!["-sound", "0", "-force_module", "ss", "game.cue"]
    );
    assert!(spec
        .env
        .contains(&("SDL_VIDEODRIVER".to_string(), "dummy".to_string())));
    assert!(spec
        .env
        .contains(&("EMUCAP_PORT".to_string(), "47800".to_string())));
}

#[test]
fn md_spec_forces_six_button_pad() {
    let spec = mednafen_spec(
        Path::new("/run/mednafen"),
        Path::new("/tmp/m.log"),
        Some("md"),
        false,
        &opts("game.md"),
    );
    assert_eq!(
        spec.args,
        vec![
            "-sound",
            "0",
            "-md.input.auto",
            "0",
            "-md.input.port1",
            "gamepad6",
            "-force_module",
            "md",
            "game.md"
        ]
    );
}

#[test]
fn name_and_token_are_passed_when_present() {
    let mut o = opts("g.cue");
    o.name = Some("saturn_session");
    o.session_token = Some("tok123");
    let spec = mednafen_spec(Path::new("/b"), Path::new("/l"), Some("ss"), false, &o);
    assert!(spec
        .env
        .contains(&("EMUCAP_NAME".to_string(), "saturn_session".to_string())));
    assert!(spec
        .env
        .contains(&("EMUCAP_SESSION_TOKEN".to_string(), "tok123".to_string())));
}

#[test]
fn pce_spec_enables_sound_only_when_requested() {
    let spec = mednafen_spec(
        Path::new("/run/mednafen"),
        Path::new("/tmp/m.log"),
        Some("pce"),
        true,
        &opts("game.cue"),
    );
    assert_eq!(
        spec.args,
        vec!["-sound", "1", "-force_module", "pce", "game.cue"]
    );
}

#[test]
fn flycast_spec_leaves_os_specific_isolation_to_launcher() {
    let mut o = opts("game.gdi");
    o.name = Some("dc_session");
    o.session_token = Some("tok123");
    let spec = flycast_spec(Path::new("/run/Flycast"), Path::new("/tmp/f.log"), &o);
    assert_eq!(spec.args, vec!["game.gdi"]);
    assert!(spec
        .env
        .contains(&("EMUCAP_PORT".to_string(), "47800".to_string())));
    assert!(spec
        .env
        .contains(&("EMUCAP_NAME".to_string(), "dc_session".to_string())));
    assert!(spec
        .env
        .contains(&("EMUCAP_SESSION_TOKEN".to_string(), "tok123".to_string())));
    assert!(!spec.env.iter().any(|(k, _)| k == "HOME"));
}

#[test]
fn dolphin_headless_spec_uses_isolated_user_and_headless_platform() {
    let spec = dolphin_spec(
        Path::new("/run/dolphin-emu-nogui"),
        Path::new("/tmp/dolphin.log"),
        Path::new("/tmp/dolphin-user"),
        "gamecube",
        &opts("game.gcm"),
    );
    assert!(spec
        .args
        .windows(2)
        .any(|args| args == ["--user".to_string(), "/tmp/dolphin-user".to_string()]));
    assert!(spec
        .args
        .windows(2)
        .any(|args| args == ["--exec".to_string(), "game.gcm".to_string()]));
    assert!(spec.args.iter().any(|arg| arg == "--platform=headless"));
    assert!(!spec.args.iter().any(|arg| arg == "--batch"));
    assert!(spec
        .env
        .contains(&("EMUCAP_SYSTEM".to_string(), "gamecube".to_string())));
}

#[test]
fn dolphin_gui_spec_uses_batch_render_window() {
    let mut options = opts("game.wbfs");
    options.headless = false;
    let spec = dolphin_spec(
        Path::new("/run/DolphinQt"),
        Path::new("/tmp/dolphin.log"),
        Path::new("/tmp/dolphin-user"),
        "wii",
        &options,
    );

    assert!(spec.args.iter().any(|arg| arg == "--batch"));
    assert!(!spec.args.iter().any(|arg| arg == "--platform=headless"));
    assert!(spec
        .args
        .iter()
        .any(|arg| arg == "--config=Dolphin.DSP.Backend=No Audio Output"));
}

#[test]
fn mesen_spec_passes_rom_lua_and_cli_config_overrides() {
    let spec = mesen_spec(
        Path::new("/run/Mesen"),
        Path::new("/tmp/m.log"),
        Path::new("/a/emucap-snes.lua"),
        &opts("game.sfc"),
    );
    // ROM and adapter Lua are positional, first.
    assert_eq!(&spec.args[0], "game.sfc");
    assert_eq!(&spec.args[1], "/a/emucap-snes.lua");
    // Required settings are applied via CLI config override, not by editing the user's
    // settings.json (so the user's key mappings/controller are inherited untouched), and
    // --donotSaveSettings keeps the overrides out of the user's saved config.
    let has = |a: &str| spec.args.iter().any(|x| x == a);
    assert!(has("--debug.scriptWindow.allowIoOsAccess=true"));
    assert!(has("--debug.scriptWindow.allowNetworkAccess=true"));
    assert!(has("--preferences.singleInstance=false"));
    assert!(has("--snes.port1.type=SnesController"));
    assert!(has("--donotSaveSettings"));
    assert!(spec
        .env
        .contains(&("EMUCAP_PORT".to_string(), "47800".to_string())));
}

fn mame_opts<'a>(media: &'a str) -> MameOpts<'a> {
    MameOpts {
        machine: "pc9801rs",
        rompath: Path::new("/roms"),
        mame_home: Path::new("/iso/mame"),
        pluginspath: Path::new("/a/plugins"),
        media,
        headless: true,
        cbus0: None,
        flop2: None,
        name: None,
        session_token: None,
    }
}

#[test]
fn mame_spec_isolates_dirs_and_loads_floppy() {
    let spec = mame_spec(Path::new("/mame"), Path::new("/l"), &mame_opts("game.hdm"));
    // All state dirs are under the emucap-owned MAME_HOME.
    assert!(spec
        .args
        .windows(2)
        .any(|w| w == ["-cfg_directory".to_string(), "/iso/mame/cfg".to_string()]));
    assert!(spec.args.contains(&"emucap_gdbstub".to_string()));
    // .hdm is a floppy.
    assert!(spec
        .args
        .windows(2)
        .any(|w| w == ["-flop1".to_string(), "game.hdm".to_string()]));
    assert!(!spec.args.iter().any(|a| a == "-hard"));
}

#[test]
fn mame_spec_loads_hdi_as_hard_disk() {
    let spec = mame_spec(Path::new("/mame"), Path::new("/l"), &mame_opts("disk.hdi"));
    assert!(spec
        .args
        .windows(2)
        .any(|w| w == ["-hard".to_string(), "disk.hdi".to_string()]));
    assert!(!spec.args.iter().any(|a| a == "-flop1"));
}

#[test]
fn mame_spec_mounts_second_floppy() {
    let mut o = mame_opts("system.d88");
    o.flop2 = Some("sampling.d88");
    let spec = mame_spec(Path::new("/mame"), Path::new("/l"), &o);
    // 2-드라이브 게임(System+Sampling): -flop1·-flop2 둘 다 마운트돼야 인게임까지 부팅된다.
    assert!(spec
        .args
        .windows(2)
        .any(|w| w == ["-flop1".to_string(), "system.d88".to_string()]));
    assert!(spec
        .args
        .windows(2)
        .any(|w| w == ["-flop2".to_string(), "sampling.d88".to_string()]));
}

#[test]
fn mame_spec_visible_mode_authorizes_wrapper_without_headless_options() {
    let mut o = mame_opts("game.hdm");
    o.headless = false;
    let spec = mame_spec(Path::new("/mame"), Path::new("/l"), &o);

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
            "visible MAME spec contains headless option {forbidden}"
        );
    }
    assert!(spec.args.iter().any(|arg| arg == "-window"));
    assert!(spec.args.iter().any(|arg| arg == "-nomaximize"));
}
