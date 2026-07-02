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
    let opts = crate::launch::spec::SpecOpts {
        content: l.content,
        port: l.port,
        name: l.name,
        session_token: l.session_token,
        headless: false, // Mesen renders a GUI window; there is no headless mode.
    };
    let spec = crate::launch::spec::mesen_spec(&portable.binary, l.log_path, l.lua, &opts);
    let pid = crate::launch::spawn_detached(&spec)?;
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("caffeinate")
            .args(["-d", "-w", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    Ok(pid)
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
}
