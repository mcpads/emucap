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

/// Mednafen-specific choices layered on top of the common process options.
pub struct MednafenSpecOpts<'a> {
    pub module: Option<&'a str>,
    pub sound: bool,
    pub pcfx_bios: Option<&'a Path>,
    pub start_frozen: bool,
    pub repeatable: bool,
}

/// Mednafen (Saturn / PSX / PCE / PC-FX / MD / WonderSwan / Neo Geo Pocket). One binary handles every system; `module`
/// selects it. Mirrors adapters/mednafen/launch.sh: explicit `-sound 0|1`, a 6-button pad for MD
/// so the raw input mask has a stable 2-byte buffer, `-force_module`, then the content path.
pub fn mednafen_spec(
    binary: &Path,
    log_path: &Path,
    runtime_home: &Path,
    mednafen: &MednafenSpecOpts,
    opts: &SpecOpts,
) -> LaunchSpec {
    let mut spec = LaunchSpec::new(binary, log_path);
    // Homebrew's current `sdl2` is SDL3-backed sdl2-compat. Its macOS OpenGL swap can wait
    // indefinitely before Mednafen releases the first video sync, which also prevents the
    // emulation-thread adapter from connecting. The software framebuffer avoids that startup
    // deadlock and works for both the visible Cocoa and headless SDL providers.
    #[cfg(target_os = "macos")]
    {
        spec = spec.args(["-video.driver", "softfb"]);
    }
    spec = spec.args(["-sound", if mednafen.sound { "1" } else { "0" }]);
    spec = spec.env("MEDNAFEN_HOME", runtime_home.to_string_lossy().into_owned());
    if mednafen.start_frozen || mednafen.repeatable {
        spec = spec.env("EMUCAP_START_FROZEN", "1");
    }
    if mednafen.repeatable {
        spec = spec
            .env("EMUCAP_EXECUTION_PROFILE", "repeatable")
            .env(
                "EMUCAP_REPEATABLE_PROFILE_ID",
                super::mednafen::MD_REPEATABLE_PROFILE_ID,
            )
            .env(
                "EMUCAP_REPEATABLE_CONDITIONS_SHA256",
                super::mednafen::MD_REPEATABLE_CONDITIONS_SHA256,
            );
    }
    if mednafen.module == Some("md") {
        spec = spec.args(["-md.input.auto", "0", "-md.input.port1", "gamepad6"]);
    }
    if let Some(path) = mednafen.pcfx_bios {
        spec = spec.args([
            "-pcfx.bios".to_string(),
            path.to_string_lossy().into_owned(),
        ]);
    }
    if let Some(m) = mednafen.module {
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
    if system == "wii" {
        // The per-port user directory is intentionally reusable. Select the emulated source in
        // Dolphin's current-run layer so a prior GUI choice cannot turn this generation into a
        // real or absent Wii Remote while the adapter advertises core-button injection.
        spec = spec.arg("--config=Wiimote.Wiimote1.Source=1");
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
        // user's settings.json. The portable settings template initializes Mesen's built-in
        // native mappings so a human can drive the GUI; --donotSaveSettings keeps the isolated
        // runtime profile unchanged. Cross-platform: CommandLineHelper parses these identically
        // on macOS/Linux/Windows. ScriptWindow lives under Debug; SingleInstance under Preferences.
        // snes.port1.type is forced so emucap set_input always reaches a controller.
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
    pub sound: bool,
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
        "-mouse".into(),
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
        args.extend(["-window", "-nomaximize"].map(String::from));
    }
    if !o.sound {
        args.extend(["-sound", "none"].map(String::from));
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
    if o.sound {
        // The repo-local safe wrapper defaults to silence. Explicit authorization lets MAME's
        // platform-neutral `auto` provider choose an available host audio backend.
        spec = spec.env("MAME_ALLOW_SOUND", "1");
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
