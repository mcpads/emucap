//! Per-adapter `LaunchSpec` builders — turn a resolved (binary, content, port, options)
//! into the exact args + env for one emulator. Pure and testable; the filesystem side
//! effects (binary copy, config seeding) and the spawn live elsewhere.

use std::path::Path;

use super::{LaunchSpec, RuntimeEnv};

/// Options shared by adapter spec builders.
pub struct SpecOpts<'a> {
    pub content: &'a str,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    /// Run without a visible window where the emulator supports it.
    pub headless: bool,
}

/// Mednafen (Saturn / PSX / PCE / MD). One binary handles all four; `module` selects it.
/// Mirrors adapters/mednafen/launch.sh: `-sound 0`, a 6-button pad for MD so the raw input
/// mask has a stable 2-byte buffer, `-force_module`, then the content path.
pub fn mednafen_spec(
    binary: &Path,
    log_path: &Path,
    module: Option<&str>,
    opts: &SpecOpts,
) -> LaunchSpec {
    let mut spec = LaunchSpec::new(binary, log_path).args(["-sound", "0"]);
    if module == Some("md") {
        spec = spec.args(["-md.input.auto", "0", "-md.input.port1", "gamepad6"]);
    }
    if let Some(m) = module {
        spec = spec.args(["-force_module", m]);
    }
    spec = spec
        .arg(opts.content)
        .env("EMUCAP_PORT", opts.port.to_string())
        .env("EMUCAP_CONTENT", opts.content)
        .env("MEDNAFEN_ALLOWMULTI", "1");
    if let Some(name) = opts.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = opts.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    if opts.headless {
        spec = spec.env("SDL_VIDEODRIVER", "dummy");
    }
    spec.runtime_env(opts.runtime)
}

/// Flycast (Dreamcast). The interpreter/mute/GDB settings are seeded into the isolated config
/// directory by the launcher; this spec only carries process args and adapter env. args = [disc].
pub fn flycast_spec(binary: &Path, log_path: &Path, opts: &SpecOpts) -> LaunchSpec {
    let mut spec = LaunchSpec::new(binary, log_path)
        .arg(opts.content)
        .env("EMUCAP_PORT", opts.port.to_string())
        .env("EMUCAP_CONTENT", opts.content);
    if let Some(name) = opts.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = opts.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec.runtime_env(opts.runtime)
}

/// Mesen2 (SNES). The ROM and the adapter Lua script are positional args; the port, name,
/// and content are passed via the environment. args = [rom, lua].
pub fn mesen_spec(binary: &Path, log_path: &Path, lua: &Path, opts: &SpecOpts) -> LaunchSpec {
    let mut spec = LaunchSpec::new(binary, log_path)
        .arg(opts.content)
        .arg(lua.to_string_lossy().into_owned())
        // Apply the emucap-required settings via CLI config override instead of editing the
        // user's settings.json. This inherits the user's own key mappings / controller (so a
        // human can drive the GUI) while enabling the Lua socket, and --donotSaveSettings keeps
        // these overrides out of the saved config. Cross-platform: CommandLineHelper parses these
        // identically on macOS/Linux/Windows. ScriptWindow lives under Debug; SingleInstance under
        // Preferences. snes.port1.type is forced so emucap set_input always reaches a controller,
        // even on a fresh profile where the user hasn't attached one yet.
        .args([
            "--debug.scriptWindow.allowIoOsAccess=true",
            "--debug.scriptWindow.allowNetworkAccess=true",
            "--debug.scriptWindow.scriptTimeout=60",
            "--preferences.singleInstance=false",
            "--snes.port1.type=SnesController",
            "--donotSaveSettings",
        ])
        .env("EMUCAP_PORT", opts.port.to_string())
        .env("EMUCAP_CONTENT", opts.content)
        // The entry Lua (emucap-snes.lua / emucap-sms.lua) dofile's emucap-core.lua from this dir.
        .env(
            "EMUCAP_ADAPTER_DIR",
            lua.parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    if let Some(name) = opts.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = opts.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec.runtime_env(opts.runtime)
}

/// Resolved inputs for the MAME (PC-98) process spec.
pub struct MameOpts<'a> {
    pub machine: &'a str,
    pub rompath: &'a Path,
    pub mame_home: &'a Path,
    pub pluginspath: &'a Path,
    pub media: &'a str,
    pub headless: bool,
    pub cbus0: Option<&'a str>,
    pub flop2: Option<&'a str>,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
}

/// MAME (PC-98). MAME exposes a GDB stub via the emucap_gdbstub plugin; a separate bridge
/// process relays it to emucap. This builds the MAME process spec — every MAME directory
/// points at the emucap-owned MAME_HOME so the user's MAME config is untouched. The media is
/// loaded as a hard disk (`.hdi`) or a floppy (any other extension).
pub fn mame_spec(binary: &Path, log_path: &Path, o: &MameOpts) -> LaunchSpec {
    let home = o.mame_home.to_string_lossy();
    let mut args: Vec<String> = vec![
        o.machine.to_string(),
        "-rompath".into(),
        o.rompath.to_string_lossy().into_owned(),
        "-homepath".into(),
        home.clone().into_owned(),
        "-cfg_directory".into(),
        format!("{home}/cfg"),
        "-nvram_directory".into(),
        format!("{home}/nvram"),
        "-input_directory".into(),
        format!("{home}/inp"),
        "-state_directory".into(),
        format!("{home}/sta"),
        "-snapshot_directory".into(),
        format!("{home}/snap"),
        "-diff_directory".into(),
        format!("{home}/diff"),
        "-comment_directory".into(),
        format!("{home}/comments"),
        "-skip_gameinfo".into(),
        "-debug".into(),
        "-debugger".into(),
        "none".into(),
        "-pluginspath".into(),
        o.pluginspath.to_string_lossy().into_owned(),
        "-plugins".into(),
        "-plugin".into(),
        "emucap_gdbstub".into(),
        "-noreadconfig".into(),
    ];
    if o.headless {
        args.extend(
            [
                "-video",
                "none",
                "-videodriver",
                "dummy",
                "-window",
                "-nomaximize",
                "-sound",
                "none",
                "-keyboardprovider",
                "none",
                "-mouseprovider",
                "none",
                "-output",
                "none",
            ]
            .map(String::from),
        );
    } else {
        args.extend(["-window", "-nomaximize", "-sound", "none"].map(String::from));
    }
    if let Some(c) = o.cbus0 {
        args.push("-cbus:0".into());
        args.push(c.to_string());
    }
    let is_hard = Path::new(o.media)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("hdi"));
    args.push(if is_hard {
        "-hard".into()
    } else {
        "-flop1".into()
    });
    args.push(o.media.to_string());
    if let Some(f2) = o.flop2 {
        args.push("-flop2".into());
        args.push(f2.to_string());
    }

    let mut spec = LaunchSpec {
        program: binary.into(),
        args,
        env: Vec::new(),
        log_path: log_path.into(),
        cwd: None,
    }
    .env("EMUCAP_CONTENT", o.media);
    if o.headless {
        spec = spec.env("SDL_VIDEODRIVER", "dummy");
    }
    if let Some(name) = o.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = o.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec
}

#[cfg(test)]
mod tests {
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
        let spec = mednafen_spec(Path::new("/b"), Path::new("/l"), Some("ss"), &o);
        assert!(spec
            .env
            .contains(&("EMUCAP_NAME".to_string(), "saturn_session".to_string())));
        assert!(spec
            .env
            .contains(&("EMUCAP_SESSION_TOKEN".to_string(), "tok123".to_string())));
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
}
