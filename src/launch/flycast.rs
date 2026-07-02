//! Flycast (Dreamcast) launch orchestration.
//!
//! Flycast reads its config from a per-OS location: macOS uses `$HOME`, Linux uses XDG, and Windows
//! uses the executable directory. To avoid touching the user's real install we run an emucap-owned
//! runtime copy and seed an emucap-owned config: copy the user's emu.cfg into it when available and set
//! only the emucap needs — interpreter, mute, and GDB — on that copy.

use super::spec::{flycast_spec, SpecOpts};
use std::path::{Path, PathBuf};

/// Resolve the Flycast binary: `FLYCAST_APP` override, else the emucap-owned build output,
/// a legacy `$HOME/flycast/build` build, or the first Flycast executable on PATH.
pub fn build_home() -> PathBuf {
    std::env::var_os("EMUCAP_FLYCAST_BUILD_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::emu_home_base().join("flycast-build"))
}

pub fn resolve_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("FLYCAST_APP") {
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
    let build_home = build_home();
    for candidate in [
        "work/build/Flycast.app/Contents/MacOS/Flycast",
        "work/build/flycast",
        "work/build/Flycast.exe",
    ] {
        let p = build_home.join(candidate);
        if super::is_runnable_file(&p) {
            return Some(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let base = PathBuf::from(home).join("flycast/build");
        for candidate in ["Flycast.app/Contents/MacOS/Flycast", "flycast"] {
            let p = base.join(candidate);
            if super::is_runnable_file(&p) {
                return Some(p);
            }
        }
    }
    if let Some(default) = super::first_existing_file(default_install_candidates()) {
        return Some(default);
    }
    let exe = if cfg!(windows) {
        "Flycast.exe"
    } else {
        "flycast"
    };
    super::find_on_path(exe)
}

fn app_bundle_executable(path: &Path) -> Option<PathBuf> {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        .then(|| path.join("Contents/MacOS/Flycast"))
}

pub fn default_install_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from(
            "/Applications/Flycast.app/Contents/MacOS/Flycast",
        ));
    }
    #[cfg(windows)]
    {
        for key in [
            "LOCALAPPDATA",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "USERPROFILE",
        ] {
            if let Some(base) = std::env::var_os(key).map(PathBuf::from) {
                candidates.push(base.join("Programs/Flycast/Flycast.exe"));
                candidates.push(base.join("Flycast/Flycast.exe"));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            candidates.push(home.join(".local/bin/flycast"));
        }
    }
    candidates
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub content: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    /// Mute audio (default true for debugging).
    pub mute: bool,
    /// Enable Flycast's GDB stub (for the exec-breakpoint path).
    pub gdb: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRuntimeBinary {
    pub binary: PathBuf,
    pub portable_dir: PathBuf,
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

/// Run Flycast from an emucap-owned copy. This keeps rebuilds, per-port config writes, and Windows'
/// executable-directory config model away from the user's real install.
pub fn prepare_runtime_binary(
    source_binary: &Path,
    iso_home: &Path,
) -> std::io::Result<PreparedRuntimeBinary> {
    let portable_dir = iso_home.join("portable");
    std::fs::create_dir_all(&portable_dir)?;
    if source_binary.starts_with(&portable_dir) {
        if super::has_symlink_component_under(iso_home, source_binary) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "portable Flycast binary path contains a symlink, refusing to launch: {}",
                    source_binary.display()
                ),
            ));
        }
        return Ok(PreparedRuntimeBinary {
            binary: source_binary.to_path_buf(),
            portable_dir,
        });
    }

    let binary = if let Some((app_root, rel)) = app_bundle_root(source_binary) {
        let app_name = app_root.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Flycast app path: {}", app_root.display()),
            )
        })?;
        let dst_root = portable_dir.join(app_name);
        super::copy_dir_replace(app_root, &dst_root)?;
        dst_root.join(rel)
    } else {
        let exe_name = source_binary.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Flycast binary path: {}", source_binary.display()),
            )
        })?;
        let binary = portable_dir.join(exe_name);
        super::copy_file_replace(source_binary, &binary)?;
        binary
    };

    Ok(PreparedRuntimeBinary {
        binary,
        portable_dir,
    })
}

/// Seed the isolated config and launch Flycast detached, pointed at the emucap-owned home so the user's
/// real Flycast config is untouched. Returns the child pid.
pub fn launch(l: &Launch) -> std::io::Result<u32> {
    let iso_home = super::emu_home_dir("flycast", l.port);
    let runtime = prepare_runtime_binary(l.binary, &iso_home)?;
    let launch_binary = runtime.binary.clone();

    // Per-OS: where Flycast reads config, which env redirects it there, and where the user's real cfg is.
    #[cfg(target_os = "macos")]
    let (iso_cfg_dir, iso_env, user_srcs): (PathBuf, Vec<(String, String)>, Vec<PathBuf>) = {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let srcs = home
            .iter()
            .flat_map(|h| {
                [
                    h.join(".flycast/emu.cfg"),
                    h.join("Library/Application Support/flycast/emu.cfg"),
                    h.join("Library/Application Support/Flycast/emu.cfg"),
                ]
            })
            .collect();
        (
            iso_home.join(".flycast"),
            vec![("HOME".into(), iso_home.to_string_lossy().into_owned())],
            srcs,
        )
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (iso_cfg_dir, iso_env, user_srcs): (PathBuf, Vec<(String, String)>, Vec<PathBuf>) = {
        let cfg_base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        (
            iso_home.join("config/flycast"),
            vec![
                (
                    "XDG_CONFIG_HOME".into(),
                    iso_home.join("config").to_string_lossy().into_owned(),
                ),
                (
                    "XDG_DATA_HOME".into(),
                    iso_home.join("data").to_string_lossy().into_owned(),
                ),
            ],
            cfg_base
                .map(|b| b.join("flycast/emu.cfg"))
                .into_iter()
                .collect(),
        )
    };
    #[cfg(target_os = "windows")]
    let (iso_cfg_dir, iso_env, user_srcs): (PathBuf, Vec<(String, String)>, Vec<PathBuf>) = {
        let srcs = l
            .binary
            .parent()
            .map(|p| p.join("emu.cfg"))
            .into_iter()
            .collect();
        (runtime.portable_dir.clone(), Vec::new(), srcs)
    };

    std::fs::create_dir_all(&iso_cfg_dir)?;
    let iso_cfg = iso_cfg_dir.join("emu.cfg");
    // Copy the user's real cfg (BIOS path, controls) if present; else start minimal.
    let mut text = user_srcs
        .iter()
        .find(|p| p.is_file())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    if !text.contains("[config]") {
        text = format!("[config]\n{text}");
    }
    set_ini(&mut text, "Dynarec.Enabled", "no");
    set_ini(&mut text, "aica.Volume", if l.mute { "0" } else { "100" });
    set_ini(
        &mut text,
        "Debug.GDBEnabled",
        if l.gdb { "yes" } else { "no" },
    );
    std::fs::write(&iso_cfg, text)?;

    let opts = SpecOpts {
        content: l.content,
        port: l.port,
        name: l.name,
        session_token: l.session_token,
        headless: false,
    };
    let mut spec = flycast_spec(&launch_binary, l.log_path, &opts);
    for (k, v) in iso_env {
        spec = spec.env(k, v);
    }
    let pid = super::spawn_detached(&spec)?;
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

/// Set `key = value` under the `[config]` section of an emu.cfg-style ini: replace an existing line or
/// insert right after `[config]`. Operates on the isolated copy only.
fn set_ini(text: &mut String, key: &str, value: &str) {
    let prefix = format!("{key} = ");
    if let Some(start) = text
        .lines()
        .position(|l| l.trim_start().starts_with(&prefix))
    {
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines[start] = format!("{key} = {value}");
        *text = lines.join("\n");
        text.push('\n');
    } else if let Some(idx) = text.find("[config]\n") {
        let at = idx + "[config]\n".len();
        text.insert_str(at, &format!("{key} = {value}\n"));
    } else {
        text.push_str(&format!("[config]\n{key} = {value}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::set_ini;
    #[cfg(any(target_os = "macos", windows))]
    use std::path::PathBuf;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn set_ini_replaces_and_inserts_under_config() {
        let mut t = "[config]\naica.Volume = 100\nDynarec.Enabled = yes\n".to_string();
        super::set_ini(&mut t, "Dynarec.Enabled", "no"); // replace
        super::set_ini(&mut t, "Debug.GDBEnabled", "no"); // insert
        assert!(t.contains("Dynarec.Enabled = no"));
        assert!(!t.contains("Dynarec.Enabled = yes"));
        assert!(t.contains("Debug.GDBEnabled = no"));
        assert!(t.contains("aica.Volume = 100")); // preserved
    }

    #[test]
    fn set_ini_adds_config_section_when_absent() {
        let mut t = String::new();
        set_ini(&mut t, "aica.Volume", "0");
        assert!(t.contains("[config]"));
        assert!(t.contains("aica.Volume = 0"));
    }

    #[test]
    fn runtime_binary_uses_emucap_owned_plain_exe_dir() {
        let src = tempfile::tempdir().unwrap();
        let iso = tempfile::tempdir().unwrap();
        let source_bin = src.path().join("Flycast.exe");
        let source_cfg = src.path().join("emu.cfg");
        std::fs::write(&source_bin, "fake flycast").unwrap();
        std::fs::write(&source_cfg, "[config]\naica.Volume = 100\n").unwrap();

        let prepared = super::prepare_runtime_binary(&source_bin, iso.path()).unwrap();

        assert_eq!(prepared.portable_dir, iso.path().join("portable"));
        assert_eq!(prepared.binary, iso.path().join("portable/Flycast.exe"));
        assert!(prepared.binary.is_file());
        assert_eq!(
            std::fs::read_to_string(&source_cfg).unwrap(),
            "[config]\naica.Volume = 100\n"
        );
        assert!(prepared.binary.starts_with(iso.path()));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_binary_refuses_symlink_inside_portable_dir() {
        let outside = tempfile::tempdir().unwrap();
        let iso = tempfile::tempdir().unwrap();
        let target_bin = outside.path().join("flycast");
        std::fs::write(&target_bin, "user flycast").unwrap();
        make_executable(&target_bin);
        let portable_dir = iso.path().join("portable");
        std::fs::create_dir_all(&portable_dir).unwrap();
        let portable_link = portable_dir.join("flycast");
        std::os::unix::fs::symlink(&target_bin, &portable_link).unwrap();

        let err = super::prepare_runtime_binary(&portable_link, iso.path()).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(std::fs::symlink_metadata(&portable_link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&target_bin).unwrap(),
            "user flycast"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runtime_binary_refuses_symlinked_parent_inside_portable_dir() {
        let outside = tempfile::tempdir().unwrap();
        let iso = tempfile::tempdir().unwrap();
        let outside_portable = outside.path().join("portable-target");
        std::fs::create_dir_all(&outside_portable).unwrap();
        let target_bin = outside_portable.join("flycast");
        std::fs::write(&target_bin, "user flycast").unwrap();
        make_executable(&target_bin);
        let portable_link = iso.path().join("portable");
        std::os::unix::fs::symlink(&outside_portable, &portable_link).unwrap();
        let apparent_binary = portable_link.join("flycast");

        let err = super::prepare_runtime_binary(&apparent_binary, iso.path()).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_to_string(&target_bin).unwrap(),
            "user flycast"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn default_install_candidates_include_macos_app() {
        assert!(super::default_install_candidates().contains(&PathBuf::from(
            "/Applications/Flycast.app/Contents/MacOS/Flycast"
        )));
    }

    #[test]
    fn resolve_binary_accepts_explicit_app_bundle_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Flycast.app");
        let binary = app.join("Contents/MacOS/Flycast");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"fake flycast").unwrap();
        #[cfg(unix)]
        make_executable(&binary);

        let old = std::env::var_os("FLYCAST_APP");
        std::env::set_var("FLYCAST_APP", &app);
        let resolved = super::resolve_binary();
        match old {
            Some(v) => std::env::set_var("FLYCAST_APP", v),
            None => std::env::remove_var("FLYCAST_APP"),
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

        let candidates = super::default_install_candidates();

        match old {
            Some(v) => std::env::set_var("LOCALAPPDATA", v),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        assert!(candidates.contains(&base.join("Programs/Flycast/Flycast.exe")));
    }

    #[test]
    fn runtime_binary_copies_app_bundle_and_uses_inner_binary() {
        let src = tempfile::tempdir().unwrap();
        let iso = tempfile::tempdir().unwrap();
        let app = src.path().join("Flycast.app");
        let source_bin = app.join("Contents/MacOS/Flycast");
        let source_resource = app.join("Contents/Resources/icon.txt");
        std::fs::create_dir_all(source_bin.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_resource.parent().unwrap()).unwrap();
        std::fs::write(&source_bin, "fake flycast app").unwrap();
        std::fs::write(&source_resource, "resource").unwrap();

        let prepared = super::prepare_runtime_binary(&source_bin, iso.path()).unwrap();

        assert_eq!(
            prepared.binary,
            iso.path()
                .join("portable/Flycast.app/Contents/MacOS/Flycast")
        );
        assert!(iso
            .path()
            .join("portable/Flycast.app/Contents/Resources/icon.txt")
            .is_file());
        assert_eq!(
            std::fs::read_to_string(&source_bin).unwrap(),
            "fake flycast app"
        );
        assert_eq!(
            std::fs::read_to_string(&source_resource).unwrap(),
            "resource"
        );
    }
}
