//! MAME (PC-98) launch orchestration. MAME exposes its state through a Lua GDB-stub plugin
//! (`emucap_gdbstub`) that listens on a GDB port, and a bridge process relays that GDB stub to emucap
//! on the MCP's port. This spawns two processes: MAME and the bridge.
//!
//! The bridge entrypoint is the Rust `emucap-mame-pc98-bridge` binary. Keeping one production
//! implementation prevents launch-time fallback from silently changing protocol behavior.

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
}

fn gdb_port_for_emucap_port(port: u16) -> std::io::Result<u16> {
    port.checked_add(1000).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("MAME GDB port would overflow for EMUCAP_PORT={port}"),
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
        "rust" => Ok(BridgeRuntime {
            kind: requested,
            program: resolve_rust_bridge_binary(repo_root)?,
        }),
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "EMUCAP_PC98_BRIDGE only supports rust; the Python fallback was removed, got {other:?}"
            ),
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

/// Spawn MAME with the emucap GDB-stub plugin on `port + 1000`, then the bridge that relays it to
/// emucap on `port`. The bridge receives MAME's exact process identity and exits when that
/// generation ends, including while the front connection is idle.
pub fn launch(l: &Launch) -> std::io::Result<Launched> {
    let gdb_port = gdb_port_for_emucap_port(l.port)?;
    let BridgeLaunch {
        kind: bridge_kind,
        spec: bridge_spec,
    } = resolve_bridge_launch(l, gdb_port)?;
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
    let mame = mame_spec(l.binary, l.log_path, &opts).env("MAME_GDB_PORT", gdb_port.to_string());
    let mame_pid = spawn_detached(&mame)?;
    let bridge_spec = match bridge_spec.emulator_dependency(mame_pid) {
        Ok(spec) => spec,
        Err(error) => {
            let _ = terminate_detached(mame_pid);
            return Err(error);
        }
    };
    let bridge_pid = match spawn_detached(&bridge_spec) {
        Ok(pid) => pid,
        Err(error) => {
            let _ = terminate_detached(mame_pid);
            return Err(error);
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
        bridge_kind,
    })
}

#[cfg(test)]
#[path = "mame_tests.rs"]
mod tests;
