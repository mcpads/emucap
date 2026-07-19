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

/// Mednafen (Saturn / PSX / PCE / MD / WonderSwan). One binary handles every system; `module`
/// selects it. Mirrors adapters/mednafen/launch.sh: explicit `-sound 0|1`, a 6-button pad for MD
/// so the raw input mask has a stable 2-byte buffer, `-force_module`, then the content path.
pub fn mednafen_spec(
    binary: &Path,
    log_path: &Path,
    module: Option<&str>,
    sound: bool,
    opts: &SpecOpts,
) -> LaunchSpec {
    let mut spec =
        LaunchSpec::new(binary, log_path).args(["-sound", if sound { "1" } else { "0" }]);
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

/// Dolphin (GameCube / Wii). The launcher resolves and copies the compatible native fork, then
/// provides a per-port user directory. GUI batch mode hides only the game list while retaining the
/// render window; the no-GUI build explicitly selects the headless window platform.
pub fn dolphin_spec(
    binary: &Path,
    log_path: &Path,
    user_dir: &Path,
    system: &str,
    opts: &SpecOpts,
) -> LaunchSpec {
    let mut spec = LaunchSpec::new(binary, log_path)
        .args([
            "--user".to_string(),
            user_dir.to_string_lossy().into_owned(),
            "--exec".to_string(),
            opts.content.to_string(),
            "--config=Dolphin.Interface.ConfirmStop=False".to_string(),
            "--config=Dolphin.Interface.UsePanicHandlers=False".to_string(),
            "--config=Dolphin.Analytics.Enabled=False".to_string(),
            "--config=Dolphin.Analytics.PermissionAsked=True".to_string(),
            "--config=Dolphin.DSP.Backend=No Audio Output".to_string(),
        ])
        .env("EMUCAP_PORT", opts.port.to_string())
        .env("EMUCAP_CONTENT", opts.content)
        .env("EMUCAP_SYSTEM", system);
    if opts.headless {
        spec = spec.arg("--platform=headless");
    } else {
        spec = spec.arg("--batch");
    }
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
    } else {
        // The repo-local MAME binary is a fail-closed wrapper that appends `-video none` unless
        // visible mode is explicitly authorized. Without this flag, display=true is converted
        // back to headless after the Rust launcher has built the correct visible arguments.
        spec = spec.env("MAME_ALLOW_VISIBLE", "1");
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
#[path = "spec_tests.rs"]
mod tests;
