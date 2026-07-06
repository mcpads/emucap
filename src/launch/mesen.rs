//! Mesen2 portable launch preparation, cross-platform.
//!
//! The emucap Lua's socket needs Mesen's script I/O + network access enabled, a script timeout large
//! enough for one big read (dump_memory reads a whole region in a single call), and SingleInstance off
//! so starting an emucap instance doesn't take over the user's other open ROM, and a controller
//! connected on the game port (emu.setInput reaches no device otherwise). The launcher copies the
//! Mesen executable/app into an emucap-owned portable directory but writes NO settings.json there, so
//! Mesen loads the user's own settings and inherits their keymaps/controller (hands-on HITL works);
//! the required keys are passed as CLI config overrides in `mesen_spec` with `--donotSaveSettings` so
//! they never persist to the user's file.

use std::path::{Path, PathBuf};

/// Resolve the Mesen executable: the `MESEN_BIN` override if it points at an existing file, else the
/// per-OS default install location, else the first `Mesen`/`Mesen.exe` found on `PATH`.
pub fn resolve_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("MESEN_BIN") {
        let p = PathBuf::from(explicit);
        if super::is_runnable_file(&p) {
            return Some(p);
        }
        if let Some(binary) = app_bundle_executable(&p) {
            if super::is_runnable_file(&binary) {
                return Some(binary);
            }
        }
    }
    if let Some(default) = super::first_existing_file(default_install_candidates()) {
        return Some(default);
    }
    let exe = if cfg!(windows) { "Mesen.exe" } else { "Mesen" };
    super::find_on_path(exe)
}

fn app_bundle_executable(path: &Path) -> Option<PathBuf> {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        .then(|| path.join("Contents/MacOS/Mesen"))
}

pub fn default_install_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from(
            "/Applications/Mesen.app/Contents/MacOS/Mesen",
        )]
    }
    #[cfg(windows)]
    {
        let mut candidates = Vec::new();
        for key in [
            "LOCALAPPDATA",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "USERPROFILE",
        ] {
            if let Some(base) = std::env::var_os(key).map(PathBuf::from) {
                candidates.push(base.join("Programs/Mesen/Mesen.exe"));
                candidates.push(base.join("Mesen/Mesen.exe"));
            }
        }
        candidates
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPortable {
    pub binary: PathBuf,
    pub settings: PathBuf,
    pub home: PathBuf,
}

fn app_bundle_root(binary: &Path) -> Option<(&Path, PathBuf)> {
    for ancestor in binary.ancestors() {
        if ancestor
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        {
            let rel = binary.strip_prefix(ancestor).ok()?.to_path_buf();
            return Some((ancestor, rel));
        }
    }
    None
}

/// Copy Mesen into an emucap-owned portable directory and write the required settings next to the
/// copied executable. The source binary/app is read-only input.
pub fn prepare_portable_binary(
    source_binary: &Path,
    port: u16,
) -> std::io::Result<PreparedPortable> {
    let home = super::emu_home_dir("mesen2", port);
    std::fs::create_dir_all(&home)?;

    let (binary, settings) = if source_binary.starts_with(&home) {
        if super::has_symlink_component_under(&home, source_binary) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "portable Mesen binary path contains a symlink, refusing to launch: {}",
                    source_binary.display()
                ),
            ));
        }
        let settings = source_binary
            .parent()
            .unwrap_or(&home)
            .join("settings.json");
        (source_binary.to_path_buf(), settings)
    } else if let Some((app_root, rel)) = app_bundle_root(source_binary) {
        let app_name = app_root.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Mesen app path: {}", app_root.display()),
            )
        })?;
        let dst_root = home.join(app_name);
        super::copy_dir_replace(app_root, &dst_root)?;
        let binary = dst_root.join(rel);
        let settings = binary.parent().unwrap_or(&dst_root).join("settings.json");
        (binary, settings)
    } else {
        let exe_name = source_binary.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Mesen binary path: {}", source_binary.display()),
            )
        })?;
        let dst_dir = home.join("portable");
        let binary = dst_dir.join(exe_name);
        super::copy_file_replace(source_binary, &binary)?;
        let settings = dst_dir.join("settings.json");
        (binary, settings)
    };

    // settings.json을 만들지 않는다: 그러면 Mesen이 사용자 기본 settings(~/Library 등)를 로드해
    // 사용자의 키매핑/컨트롤러를 그대로 상속한다(사람이 GUI로 조작 가능). emucap 필수 설정
    // (script I/O·network, SingleInstance, 컨트롤러 타입)은 mesen_spec의 CLI config override로
    // 넣고, --donotSaveSettings로 그 override가 사용자 파일에 저장되는 것을 막는다.
    Ok(PreparedPortable {
        binary,
        settings,
        home,
    })
}

/// Everything the launcher needs to bring up Mesen for emucap. `binary`/`lua` are resolved paths;
/// `content` is the ROM; `port` is the MCP's listening_port.
pub struct Launch<'a> {
    pub binary: &'a Path,
    pub content: &'a str,
    pub lua: &'a Path,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
}

/// Prepare an emucap-owned portable Mesen copy and launch it detached with the ROM + adapter Lua and
/// the emucap environment. Returns the child pid.
pub fn launch(l: &Launch) -> std::io::Result<u32> {
    let portable = prepare_portable_binary(l.binary, l.port)?;
    provision_gba_bios(l, &portable)?;
    let opts = crate::launch::spec::SpecOpts {
        content: l.content,
        port: l.port,
        name: l.name,
        session_token: l.session_token,
        headless: false, // Mesen renders a GUI window; there is no headless mode.
    };
    let spec = crate::launch::spec::mesen_spec(&portable.binary, l.log_path, l.lua, &opts);
    let pid = crate::launch::spawn_detached(&spec)?;
    // Keep the macOS display awake for the HITL window and reap the helper (no-op off macOS).
    crate::launch::spawn_display_caffeinate(pid);
    Ok(pid)
}

/// The default GBA BIOS source when `EMUCAP_GBA_BIOS` is unset: the emucap-owned firmware directory
/// `<emucap-home>/firmware/gba_bios.bin`. This is the shared firmware dir alongside the per-emulator
/// homes (matching the docs and legacy launcher's `$EMUCAP_MESEN_BASE/firmware/gba_bios.bin`), NOT
/// under `mesen2/<port>` — a per-port location would never be found at the documented path.
fn default_gba_bios_source() -> PathBuf {
    super::emu_home_base().join("firmware/gba_bios.bin")
}

/// GBA needs a real BIOS (gba_bios.bin), which Mesen looks for in its data folder's `Firmware`
/// directory; without it Mesen pops a firmware prompt that breaks the headless/agent flow, and a
/// freshly-copied portable starts with an empty Firmware. When launching the GBA entry script, stage
/// the BIOS into the portable Firmware directory first. Source order: explicit `EMUCAP_GBA_BIOS`,
/// else `default_gba_bios_source()`. When no explicit source is set and the destination BIOS is
/// already staged (a prior run), accept it rather than failing if the shared source has since gone.
/// A missing *explicitly-configured* source fails fast with a clear precondition instead of hanging
/// on the prompt.
fn provision_gba_bios(l: &Launch, portable: &PreparedPortable) -> std::io::Result<()> {
    if l.lua.file_name().and_then(|n| n.to_str()) != Some("emucap-gba.lua") {
        return Ok(());
    }
    let firmware = portable
        .binary
        .parent()
        .ok_or_else(|| {
            std::io::Error::other("cannot locate the portable Mesen Firmware directory")
        })?
        .join("Firmware");
    let dst = firmware.join("gba_bios.bin");

    let explicit = std::env::var_os("EMUCAP_GBA_BIOS");
    // No explicit source + an already-staged BIOS: honour the staged copy so a launch does not fail
    // when the shared firmware source has been moved/removed since the first run.
    if explicit.is_none() && dst.is_file() {
        return Ok(());
    }
    let src = match explicit {
        Some(p) => PathBuf::from(p),
        None => default_gba_bios_source(),
    };
    if !src.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "GBA needs a real BIOS (gba_bios.bin): set EMUCAP_GBA_BIOS to it, or place it at {}. \
                 Without it Mesen would pop a firmware prompt and break the headless launch.",
                src.display()
            ),
        ));
    }
    std::fs::create_dir_all(&firmware)?;
    super::copy_file_replace(&src, &dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        let _guard = ENV_LOCK.lock().unwrap();
        let old = std::env::var_os("EMUCAP_EMU_HOME");
        std::env::set_var("EMUCAP_EMU_HOME", base);
        let out = f();
        match old {
            Some(v) => std::env::set_var("EMUCAP_EMU_HOME", v),
            None => std::env::remove_var("EMUCAP_EMU_HOME"),
        }
        out
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
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Mesen.app");
        let binary = app.join("Contents/MacOS/Mesen");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"fake mesen").unwrap();
        #[cfg(unix)]
        make_executable(&binary);

        let old = std::env::var_os("MESEN_BIN");
        std::env::set_var("MESEN_BIN", &app);
        let resolved = resolve_binary();
        match old {
            Some(v) => std::env::set_var("MESEN_BIN", v),
            None => std::env::remove_var("MESEN_BIN"),
        }

        assert_eq!(resolved, Some(binary));
    }

    #[cfg(windows)]
    #[test]
    fn default_install_candidates_include_windows_user_installs() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old = std::env::var_os("LOCALAPPDATA");
        let base = PathBuf::from(r"C:\Users\alice\AppData\Local");
        std::env::set_var("LOCALAPPDATA", &base);

        let candidates = default_install_candidates();

        match old {
            Some(v) => std::env::set_var("LOCALAPPDATA", v),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
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
        let source_settings = app.join("Contents/MacOS/settings.json");
        let source_resource = app.join("Contents/Resources/icon.txt");
        std::fs::create_dir_all(source_bin.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_resource.parent().unwrap()).unwrap();
        std::fs::write(&source_bin, "fake app mesen").unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
        let old_home = std::env::var_os("EMUCAP_EMU_HOME");
        let old_bios = std::env::var_os("EMUCAP_GBA_BIOS");
        std::env::set_var("EMUCAP_EMU_HOME", emu_home);
        match gba_bios {
            Some(p) => std::env::set_var("EMUCAP_GBA_BIOS", p),
            None => std::env::remove_var("EMUCAP_GBA_BIOS"),
        }
        let out = f();
        match old_home {
            Some(v) => std::env::set_var("EMUCAP_EMU_HOME", v),
            None => std::env::remove_var("EMUCAP_EMU_HOME"),
        }
        match old_bios {
            Some(v) => std::env::set_var("EMUCAP_GBA_BIOS", v),
            None => std::env::remove_var("EMUCAP_GBA_BIOS"),
        }
        out
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
        };
        (l, portable)
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
        std::fs::write(&src, b"BIOSBYTES").unwrap();
        let lua = dir.path().join("emucap-gba.lua");
        let log = dir.path().join("launch.log");
        let (l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
        std::fs::create_dir_all(portable.binary.parent().unwrap()).unwrap();

        with_gba_env(dir.path(), None, || provision_gba_bios(&l, &portable)).unwrap();

        let staged = portable.binary.parent().unwrap().join("Firmware/gba_bios.bin");
        assert_eq!(std::fs::read(&staged).unwrap(), b"BIOSBYTES");
    }

    #[test]
    fn provision_accepts_already_staged_bios_when_source_gone_and_unset() {
        let dir = tempfile::tempdir().unwrap();
        let lua = dir.path().join("emucap-gba.lua");
        let log = dir.path().join("launch.log");
        let (l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
        // A BIOS was staged by a prior run; the shared source dir does NOT exist now.
        let staged = portable.binary.parent().unwrap().join("Firmware/gba_bios.bin");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"PRIORRUN").unwrap();

        with_gba_env(dir.path(), None, || provision_gba_bios(&l, &portable)).unwrap();

        // Accepted as-is: not overwritten, not failed.
        assert_eq!(std::fs::read(&staged).unwrap(), b"PRIORRUN");
    }

    #[test]
    fn provision_fails_fast_when_explicit_source_missing_even_if_staged() {
        let dir = tempfile::tempdir().unwrap();
        let lua = dir.path().join("emucap-gba.lua");
        let log = dir.path().join("launch.log");
        let (l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
        // Even with a staged BIOS, an explicitly-configured but missing source must fail fast.
        let staged = portable.binary.parent().unwrap().join("Firmware/gba_bios.bin");
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
    fn provision_skips_non_gba_lua_entry() {
        let dir = tempfile::tempdir().unwrap();
        let lua = dir.path().join("emucap-snes.lua");
        let log = dir.path().join("launch.log");
        let (l, portable) = gba_provision_inputs(dir.path(), &lua, &log);
        // No BIOS anywhere, but a non-GBA entry must not attempt provisioning.
        with_gba_env(dir.path(), None, || provision_gba_bios(&l, &portable)).unwrap();
    }
}
