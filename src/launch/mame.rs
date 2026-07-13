//! MAME (PC-98) launch orchestration. MAME exposes its state through a Lua GDB-stub plugin
//! (`emucap_gdbstub`) that listens on a GDB port, and a bridge process relays that GDB stub to emucap
//! on the MCP's port. This spawns two processes: MAME and the bridge.
//!
//! The default bridge entrypoint is the Rust `emucap-mame-pc98-bridge` binary. The Python bridge is
//! retained as an explicit `EMUCAP_PC98_BRIDGE=python` fallback.

use super::spec::{mame_spec, MameOpts};
use super::{
    emu_home_base, emu_home_dir, find_on_path, first_existing_file, spawn_detached,
    terminate_detached, LaunchSpec,
};
use std::path::{Path, PathBuf};

/// Resolve the MAME binary: `MAME_BIN` override, else the repo-local safe headless wrapper if built,
/// else `mame` on `PATH`.
pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("MAME_BIN") {
        let p = PathBuf::from(explicit);
        if super::is_runnable_file(&p) {
            return Some(p);
        }
    }
    if let Some(local) = repo_local_binary(repo_root) {
        return Some(local);
    }
    if let Some(default) = first_existing_file(default_install_candidates()) {
        return Some(default);
    }
    find_on_path("mame")
}

pub fn default_install_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from("/Applications/MAME.app/Contents/MacOS/mame"));
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
                candidates.push(base.join("Programs/MAME/mame.exe"));
                candidates.push(base.join("MAME/mame.exe"));
                candidates.push(base.join("mame/mame.exe"));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            candidates.push(home.join(".local/bin/mame"));
        }
    }
    candidates
}

fn repo_local_binary(repo_root: &Path) -> Option<PathBuf> {
    let work = repo_root.join("adapters/mame-pc98/work");
    let names: &[&str] = if cfg!(windows) {
        &["mame.exe"]
    } else {
        &["mame"]
    };
    names
        .iter()
        .map(|name| work.join(name))
        .find(|p| super::is_runnable_file(p))
}

fn default_rompath() -> PathBuf {
    for base in [
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        let candidate = base.join("mame/roms");
        if candidate.is_dir() {
            return candidate;
        }
    }
    emu_home_base().join("mame-pc98").join("roms")
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub repo_root: &'a Path,
    pub content: &'a str,
    /// 2번째 플로피(선택). None이면 MAME_FLOP2 환경변수를 폴백으로 읽는다(legacy launch.sh와 동형).
    pub flop2: Option<&'a str>,
    pub machine: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
    pub headless: bool,
}

/// 2번째 플로피 경로 결정: 명시 param(launch 툴 `content_path2` → `Launch.flop2`)이 우선, 없으면
/// `MAME_FLOP2` 환경변수를 폴백으로(legacy launch.sh 동형). 둘 다 없으면 None(단일 매체).
fn resolve_flop2<'a>(explicit: Option<&'a str>, env: Option<&'a str>) -> Option<&'a str> {
    explicit.or(env)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub mame_pid: u32,
    pub bridge_pid: u32,
    pub gdb_port: u16,
    pub bridge_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeRuntime {
    pub kind: String,
    pub program: PathBuf,
    pub script: Option<PathBuf>,
}

fn gdb_port_for_emucap_port(port: u16) -> std::io::Result<u16> {
    port.checked_add(1000).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("MAME GDB port would overflow for EMUCAP_PORT={port}"),
        )
    })
}

fn resolve_bridge_script(repo_root: &Path) -> std::io::Result<PathBuf> {
    let bridge = repo_root.join("adapters/mame-pc98/emucap-gdb-bridge.py");
    if bridge.is_file() {
        Ok(bridge)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("MAME PC-98 bridge script not found: {}", bridge.display()),
        ))
    }
}

fn resolve_bridge_python() -> std::io::Result<PathBuf> {
    find_on_path("python3")
        .or_else(|| find_on_path("python"))
        .or_else(|| find_on_path("py"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Python not found on PATH; MAME PC-98 needs it to run emucap-gdb-bridge.py",
            )
        })
}

fn rust_bridge_binary_name() -> &'static str {
    if cfg!(windows) {
        "emucap-mame-pc98-bridge.exe"
    } else {
        "emucap-mame-pc98-bridge"
    }
}

fn resolve_rust_bridge_binary(repo_root: &Path) -> std::io::Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PC98_BRIDGE_BIN") {
        let p = PathBuf::from(explicit);
        if super::is_runnable_file(&p) {
            return Ok(p);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("EMUCAP_PC98_BRIDGE_BIN not found: {}", p.display()),
        ));
    }
    let name = rust_bridge_binary_name();
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    candidates.push(repo_root.join("target/release").join(name));
    candidates.push(repo_root.join("target/debug").join(name));
    if let Some(on_path) = find_on_path(name) {
        candidates.push(on_path);
    }
    candidates
        .into_iter()
        .find(|p| super::is_runnable_file(p))
        .ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Rust PC-98 bridge binary not found; build emucap-mame-pc98-bridge or set EMUCAP_PC98_BRIDGE_BIN",
        )
    })
}

pub fn resolve_bridge_runtime(repo_root: &Path) -> std::io::Result<BridgeRuntime> {
    let requested_raw = std::env::var("EMUCAP_PC98_BRIDGE")
        .unwrap_or_else(|_| "rust".into())
        .to_ascii_lowercase();
    let requested = if requested_raw.trim().is_empty() {
        "rust".to_string()
    } else {
        requested_raw
    };
    match requested.as_str() {
        "python" => Ok(BridgeRuntime {
            kind: requested,
            program: resolve_bridge_python()?,
            script: Some(resolve_bridge_script(repo_root)?),
        }),
        "rust" => Ok(BridgeRuntime {
            kind: requested,
            program: resolve_rust_bridge_binary(repo_root)?,
            script: None,
        }),
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("EMUCAP_PC98_BRIDGE must be python or rust, got {other:?}"),
        )),
    }
}

#[derive(Debug, Clone, PartialEq)]
struct BridgeLaunch {
    kind: String,
    spec: LaunchSpec,
}

fn resolve_bridge_launch(l: &Launch, gdb_port: u16) -> std::io::Result<BridgeLaunch> {
    let endpoint = format!("127.0.0.1:{gdb_port}");
    let runtime = resolve_bridge_runtime(l.repo_root)?;
    let mut spec = LaunchSpec::new(runtime.program.clone(), l.log_path);
    if let Some(script) = &runtime.script {
        spec = spec.arg(script.to_string_lossy().into_owned());
    }
    spec = spec
        .arg(l.port.to_string())
        .arg(endpoint)
        .env("EMUCAP_CONTENT", l.content);
    if let Some(name) = l.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = l.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec = spec.runtime_env(l.runtime);
    Ok(BridgeLaunch {
        kind: runtime.kind,
        spec,
    })
}

/// Spawn the bridge that relays to emucap on `port`, then MAME with the emucap GDB-stub plugin on
/// `port + 1000`.
pub fn launch(l: &Launch) -> std::io::Result<Launched> {
    let gdb_port = gdb_port_for_emucap_port(l.port)?;
    let bridge = resolve_bridge_launch(l, gdb_port)?;
    let mame_home = emu_home_dir("mame-pc98", l.port);
    std::fs::create_dir_all(&mame_home)?;
    let rompath = if let Some(explicit) = std::env::var_os("MAME_ROMPATH") {
        PathBuf::from(explicit)
    } else {
        let path = default_rompath();
        std::fs::create_dir_all(&path)?;
        path
    };
    let pluginspath = l.repo_root.join("adapters/mame-pc98/plugins");
    // Disable the pc9801rs C-bus slot 0 (the pc9801_26 sound board) by default: its ROMs (26k_wyka*)
    // are usually absent from a user's romset and MAME refuses to start the machine without them.
    // `MAME_CBUS0` overrides (e.g. to load a specific board).
    let cbus0 = std::env::var("MAME_CBUS0").unwrap_or_default();
    // 2번째 플로피: 명시 param 우선, 없으면 MAME_FLOP2 폴백(legacy launch.sh 동형). System+Sampling
    // 2장짜리 게임은 두 장을 동시에 물려야 인게임까지 부팅된다 — 1장이면 검정 hang.
    let flop2_env = std::env::var("MAME_FLOP2").ok();
    let flop2 = resolve_flop2(l.flop2, flop2_env.as_deref());

    let opts = MameOpts {
        machine: l.machine,
        rompath: &rompath,
        mame_home: &mame_home,
        pluginspath: &pluginspath,
        media: l.content,
        headless: l.headless,
        cbus0: Some(&cbus0),
        flop2,
        name: l.name,
        session_token: l.session_token,
    };
    let bridge_pid = spawn_detached(&bridge.spec)?;

    let mame = mame_spec(l.binary, l.log_path, &opts).env("MAME_GDB_PORT", gdb_port.to_string());
    let mame_pid = match spawn_detached(&mame) {
        Ok(pid) => pid,
        Err(e) => {
            let _ = terminate_detached(bridge_pid);
            return Err(e);
        }
    };
    if !l.headless {
        // Keep the macOS HITL window awake for the lifetime of this MAME process.
        super::spawn_display_caffeinate(mame_pid);
    }
    Ok(Launched {
        mame_pid,
        bridge_pid,
        gdb_port,
        bridge_kind: bridge.kind,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        default_rompath, gdb_port_for_emucap_port, repo_local_binary, resolve_bridge_launch,
        resolve_bridge_script, resolve_flop2, Launch,
    };
    use crate::launch::test_env::{lock_env, EnvGuard};
    use std::path::Path;
    #[cfg(any(target_os = "macos", windows))]
    use std::path::PathBuf;

    fn assert_os_path_eq(actual: &Path, expected: &Path) {
        #[cfg(windows)]
        assert_eq!(
            actual.to_string_lossy().to_ascii_lowercase(),
            expected.to_string_lossy().to_ascii_lowercase()
        );
        #[cfg(not(windows))]
        assert_eq!(actual, expected);
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn gdb_port_rejects_overflow_instead_of_wrapping() {
        assert_eq!(gdb_port_for_emucap_port(47800).unwrap(), 48800);
        let err = gdb_port_for_emucap_port(65000).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("65000"));
    }

    #[test]
    fn bridge_script_must_exist_before_launching_mame() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_bridge_script(dir.path()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(err.to_string().contains("emucap-gdb-bridge.py"));
    }

    #[test]
    fn bridge_script_resolves_under_repo_root() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = dir.path().join("adapters/mame-pc98/emucap-gdb-bridge.py");
        std::fs::create_dir_all(bridge.parent().unwrap()).unwrap();
        std::fs::write(&bridge, b"#!/usr/bin/env python3\n").unwrap();

        assert_eq!(resolve_bridge_script(dir.path()).unwrap(), bridge);
    }

    #[test]
    fn repo_local_mame_candidate_is_platform_native() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("adapters/mame-pc98/work");
        std::fs::create_dir_all(&work).unwrap();
        let expected = if cfg!(windows) {
            work.join("mame.exe")
        } else {
            work.join("mame")
        };
        std::fs::write(&expected, b"fake mame").unwrap();
        #[cfg(unix)]
        make_executable(&expected);
        assert_eq!(repo_local_binary(dir.path()).unwrap(), expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn default_install_candidates_include_macos_app() {
        assert!(super::default_install_candidates()
            .contains(&PathBuf::from("/Applications/MAME.app/Contents/MacOS/mame")));
    }

    #[cfg(windows)]
    #[test]
    fn default_install_candidates_include_windows_user_installs() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["LOCALAPPDATA"]);
        let base = PathBuf::from(r"C:\Users\alice\AppData\Local");
        std::env::set_var("LOCALAPPDATA", &base);

        let candidates = super::default_install_candidates();

        assert!(candidates.contains(&base.join("Programs/MAME/mame.exe")));
    }

    #[test]
    fn default_rompath_uses_existing_home_roms_dir() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["HOME", "USERPROFILE", "EMUCAP_EMU_HOME"]);
        let dir = tempfile::tempdir().unwrap();
        let roms = dir.path().join("mame/roms");
        std::fs::create_dir_all(&roms).unwrap();
        std::env::set_var("HOME", dir.path());
        std::env::remove_var("USERPROFILE");
        std::env::set_var("EMUCAP_EMU_HOME", dir.path().join("emucap"));

        assert_eq!(default_rompath(), roms);
    }

    #[test]
    fn default_rompath_falls_back_to_emucap_home_without_user_roms() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["HOME", "USERPROFILE", "EMUCAP_EMU_HOME"]);
        let dir = tempfile::tempdir().unwrap();
        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");
        std::env::set_var("EMUCAP_EMU_HOME", dir.path());

        assert_eq!(default_rompath(), dir.path().join("mame-pc98").join("roms"));
    }

    #[test]
    fn default_bridge_selection_uses_rust_binary() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PC98_BRIDGE", "EMUCAP_PC98_BRIDGE_BIN"]);
        let dir = tempfile::tempdir().unwrap();
        std::env::remove_var("EMUCAP_PC98_BRIDGE");
        std::env::remove_var("EMUCAP_PC98_BRIDGE_BIN");
        let bridge_bin = dir.path().join("target/debug").join(if cfg!(windows) {
            "emucap-mame-pc98-bridge.exe"
        } else {
            "emucap-mame-pc98-bridge"
        });
        std::fs::create_dir_all(bridge_bin.parent().unwrap()).unwrap();
        std::fs::write(&bridge_bin, b"fake bridge").unwrap();
        #[cfg(unix)]
        make_executable(&bridge_bin);
        let mame_bin = dir.path().join("mame");
        let log = dir.path().join("mame.log");
        let launch = Launch {
            binary: &mame_bin,
            repo_root: dir.path(),
            content: "/game.hdi",
            flop2: None,
            machine: "pc9801rs",
            log_path: &log,
            port: 47800,
            name: None,
            session_token: None,
            runtime: None,
            headless: true,
        };

        let selected = resolve_bridge_launch(&launch, 48800).unwrap();
        assert_eq!(selected.kind, "rust");
        assert_eq!(selected.spec.program, bridge_bin);
        assert_eq!(selected.spec.args, vec!["47800", "127.0.0.1:48800"]);
    }

    #[test]
    fn python_bridge_remains_explicit_fallback() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PC98_BRIDGE", "PATH"]);
        let dir = tempfile::tempdir().unwrap();
        let bridge = dir.path().join("adapters/mame-pc98/emucap-gdb-bridge.py");
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(bridge.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(&bridge, b"#!/usr/bin/env python3\n").unwrap();
        let python = bin_dir.join(if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        });
        std::fs::write(&python, b"fake python").unwrap();
        #[cfg(unix)]
        make_executable(&python);
        std::env::set_var("PATH", &bin_dir);
        std::env::set_var("EMUCAP_PC98_BRIDGE", "python");
        let mame_bin = dir.path().join("mame");
        let log = dir.path().join("mame.log");
        let launch = Launch {
            binary: &mame_bin,
            repo_root: dir.path(),
            content: "/game.hdi",
            flop2: None,
            machine: "pc9801rs",
            log_path: &log,
            port: 47800,
            name: None,
            session_token: None,
            runtime: None,
            headless: true,
        };

        let selected = resolve_bridge_launch(&launch, 48800).unwrap();
        assert_eq!(selected.kind, "python");
        assert_os_path_eq(&selected.spec.program, &python);
        assert_eq!(
            selected.spec.args,
            vec![
                bridge.to_string_lossy().into_owned(),
                "47800".into(),
                "127.0.0.1:48800".into()
            ]
        );
    }

    #[test]
    fn rust_bridge_selection_uses_explicit_binary() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PC98_BRIDGE", "EMUCAP_PC98_BRIDGE_BIN"]);
        let dir = tempfile::tempdir().unwrap();
        let bridge_bin = dir.path().join(if cfg!(windows) {
            "emucap-mame-pc98-bridge.exe"
        } else {
            "emucap-mame-pc98-bridge"
        });
        std::fs::write(&bridge_bin, b"fake bridge").unwrap();
        #[cfg(unix)]
        make_executable(&bridge_bin);
        std::env::set_var("EMUCAP_PC98_BRIDGE", "rust");
        std::env::set_var("EMUCAP_PC98_BRIDGE_BIN", &bridge_bin);
        let mame_bin = dir.path().join("mame");
        let log = dir.path().join("mame.log");
        let launch = Launch {
            binary: &mame_bin,
            repo_root: dir.path(),
            content: "/game.hdi",
            flop2: None,
            machine: "pc9801rs",
            log_path: &log,
            port: 47800,
            name: Some("pc98"),
            session_token: Some("token"),
            runtime: None,
            headless: true,
        };

        let selected = resolve_bridge_launch(&launch, 48800).unwrap();
        assert_eq!(selected.kind, "rust");
        assert_eq!(selected.spec.program, bridge_bin);
        assert_eq!(selected.spec.args, vec!["47800", "127.0.0.1:48800"]);
        assert!(selected
            .spec
            .env
            .iter()
            .any(|(k, v)| k == "EMUCAP_SESSION_TOKEN" && v == "token"));
    }

    #[test]
    fn resolve_flop2_prefers_explicit_over_env() {
        // 명시 param(launch 툴 content_path2)이 MAME_FLOP2 폴백보다 우선.
        assert_eq!(
            resolve_flop2(Some("/a.d88"), Some("/b.d88")),
            Some("/a.d88")
        );
        // param 없으면 env 폴백(legacy launch.sh 동형).
        assert_eq!(resolve_flop2(None, Some("/b.d88")), Some("/b.d88"));
        // 둘 다 없으면 단일 매체.
        assert_eq!(resolve_flop2(None, None), None);
    }

    #[test]
    fn rust_bridge_selection_fails_before_mame_when_binary_missing() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PC98_BRIDGE", "EMUCAP_PC98_BRIDGE_BIN"]);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("EMUCAP_PC98_BRIDGE", "rust");
        std::env::set_var("EMUCAP_PC98_BRIDGE_BIN", dir.path().join("missing"));
        let mame_bin = dir.path().join("mame");
        let log = dir.path().join("mame.log");
        let launch = Launch {
            binary: &mame_bin,
            repo_root: dir.path(),
            content: "/game.hdi",
            flop2: None,
            machine: "pc9801rs",
            log_path: &log,
            port: 47800,
            name: None,
            session_token: None,
            runtime: None,
            headless: true,
        };

        let err = resolve_bridge_launch(&launch, 48800).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(err.to_string().contains("EMUCAP_PC98_BRIDGE_BIN"));
    }
}
