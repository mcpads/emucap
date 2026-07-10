//! PPSSPP (PSP) launch orchestration. Headless PPSSPP exposes CPU/memory/state through its own
//! `debugger.ppsspp.org` WebSocket (`--debugger=<port>`), and a bridge process
//! (`emucap-ppsspp-bridge`) relays that WebSocket to emucap on the MCP's port. This spawns two
//! processes: headless PPSSPP and the bridge, mirroring `adapters/ppsspp/launch.sh` (same shape as
//! DeSmuME NDS: an emulator + a bridge).

use super::{
    find_on_path, is_runnable_file, process_alive, spawn_detached, terminate_detached, LaunchSpec,
};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Resolve the headless PPSSPPHeadless binary: `EMUCAP_PPSSPP_BIN` override, else the repo-local
/// build-headless output from `adapters/ppsspp/build.sh`. An explicit override that is not a
/// runnable file resolves to `None` (like `launch.sh`, which uses the override with no fallback).
pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PPSSPP_BIN") {
        let p = PathBuf::from(explicit);
        return is_runnable_file(&p).then_some(p);
    }
    let local = repo_root.join("adapters/ppsspp/work/ppsspp/build-headless/PPSSPPHeadless");
    is_runnable_file(&local).then_some(local)
}

/// Resolve the GUI/SDL `PPSSPPSDL` binary — the HITL `display:true` build that opens a real window a
/// human can see and play while the agent drives the same debugger WebSocket. `EMUCAP_PPSSPP_GUI_BIN`
/// override, else the repo-local `PPSSPPSDL` target output from `adapters/ppsspp/build.sh` (built
/// alongside `PPSSPPHeadless` in the same `build-headless` tree). On macOS the target is an `.app`
/// bundle, so the executable lives under `Contents/MacOS`; elsewhere it is a bare `PPSSPPSDL`. An
/// explicit override that is not a runnable file resolves to `None` (like `resolve_binary`).
pub fn resolve_gui_binary(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PPSSPP_GUI_BIN") {
        let p = PathBuf::from(explicit);
        return is_runnable_file(&p).then_some(p);
    }
    let base = repo_root.join("adapters/ppsspp/work/ppsspp/build-headless");
    [
        base.join("PPSSPPSDL.app/Contents/MacOS/PPSSPPSDL"),
        base.join("PPSSPPSDL"),
    ]
    .into_iter()
    .find(|p| is_runnable_file(p))
}

fn bridge_binary_name() -> &'static str {
    if cfg!(windows) {
        "emucap-ppsspp-bridge.exe"
    } else {
        "emucap-ppsspp-bridge"
    }
}

/// Resolve the Rust PSP bridge binary: `EMUCAP_PSP_BRIDGE_BIN` override, else the sibling of this
/// executable, `target/release`, `target/debug`, then `PATH` (matching `launch.sh`).
pub fn resolve_bridge(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PSP_BRIDGE_BIN") {
        let p = PathBuf::from(explicit);
        return is_runnable_file(&p).then_some(p);
    }
    let name = bridge_binary_name();
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
    candidates.into_iter().find(|p| is_runnable_file(p))
}

pub struct Launch<'a> {
    /// The headless PPSSPPHeadless binary.
    pub binary: &'a Path,
    /// The `emucap-ppsspp-bridge` binary that relays the debugger WebSocket to emucap.
    pub bridge: &'a Path,
    /// The `.iso`/`.cso`/`.pbp` content, passed as a **positional** boot argument (see `emu_spec`).
    pub content: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
    /// Open a native PPSSPP window (HITL viewing/play) by launching the `PPSSPPSDL` GUI build instead
    /// of `PPSSPPHeadless`; false = headless (debugger WebSocket only, the default). `binary` must
    /// already point at the GUI build when this is true (the caller resolves it via
    /// `resolve_gui_binary`). On macOS a caffeinate keeps the display awake while the window lives.
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub ppsspp_pid: u32,
    pub bridge_pid: u32,
    pub ws_port: u16,
}

/// The PPSSPP debugger WebSocket port. `PSP_DEBUGGER_PORT` overrides it (decimal TCP port), else an
/// OS-assigned free port whose reservation listener is held until just before PPSSPP is spawned (so
/// concurrent PSP sessions never collide over a fixed/derived port, same rationale as the NDS GDB
/// ports).
#[derive(Debug)]
struct WsPort {
    port: u16,
    _reservation: Option<TcpListener>,
}

fn resolve_ws_port() -> io::Result<WsPort> {
    if let Some(raw) = std::env::var_os("PSP_DEBUGGER_PORT") {
        let value = raw.to_string_lossy();
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            let port: u16 = trimmed.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("PSP_DEBUGGER_PORT must be a decimal TCP port, got {trimmed:?}"),
                )
            })?;
            if port == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "PSP_DEBUGGER_PORT must be in 1..=65535",
                ));
            }
            return Ok(WsPort {
                port,
                _reservation: None,
            });
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    Ok(WsPort {
        port,
        _reservation: Some(listener),
    })
}

/// Confirm a just-spawned process survives a brief startup window — an early exit (PPSSPPHeadless
/// crashing on a bad content path/flag combo, or the bridge failing on bad args / an immediate
/// connect failure) means the launch failed even though `spawn_detached` returned a pid.
fn wait_survives(pid: u32, settle: Duration, on_exit: &str) -> io::Result<()> {
    let deadline = Instant::now() + settle;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return Err(io::Error::other(on_exit.to_string()));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Wait for PPSSPP's debugger WebSocket port to accept a TCP connection. Unlike the NDS bridge
/// (whose `GdbRspClient` retries the GDB stubs for 30s on its own), `emucap-ppsspp-bridge`'s
/// `TungsteniteWs::connect` makes exactly one connection attempt and fails immediately — so if the
/// launcher spawned the bridge before PPSSPP opened the port, the bridge would die on a bare
/// liveness-only settle (verified: `wait_survives`'s 2s process-liveness check alone is not a
/// readiness check). Empirically the port opens within ~1s of spawn even against a 1.4GB ISO (the
/// debugger listener starts before any disc I/O — `--debugger` forces `startBreak=true` ahead of
/// boot), so this polls briefly rather than trusting timing.
fn wait_ws_ready(pid: u32, port: u16, timeout: Duration) -> io::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pid) {
            return Err(io::Error::other(
                "PPSSPPHeadless exited before opening the debugger WebSocket port (crashed — check the launch log)",
            ));
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "PPSSPPHeadless did not open the debugger WebSocket port {port} within {timeout:?} (the bridge has no connect retry — check the launch log)"
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Headless PPSSPP with the debugger WebSocket open. Mirrors `launch.sh`:
/// `PPSSPPHeadless --debugger=<port> --graphics=software <content>`.
///
/// Two upstream command-line constraints define this launch shape:
/// - The content is a **positional** boot argument, not `-m`/`--mount` — `-m` only mounts a
///   *second* image on `umd1:` for ELF+CSO test harnesses; passed alone it leaves PPSSPP's boot
///   list empty and nothing boots.
/// - `--timeout=<sec>` (a headless test-harness flag) aborts the run after that many wall-clock
///   seconds regardless of debugger/WebSocket activity — it is never passed here. Omitting it lets
///   PPSSPPHeadless run with no wall-clock deadline while the debugger remains interactive.
///
/// Per-port emucap-owned profile environment for the PPSSPP process, so a session — headless *or* a
/// HITL `display:true` window the human plays with PPSSPP's own control mappings — reads and writes
/// only isolated per-session state, never the operator's real PPSSPP config/saves. Applied to both
/// the headless and GUI specs (uniform; harmless where a var is unused). Three redirects, because
/// PPSSPP resolves its memory stick (config + saves) differently per build/OS:
/// - `HOME`: headless keys its memstick off `$HOME/.ppsspp` (`headless/Headless.cpp`); the GUI keys
///   its `defaultCurrentDirectory` (and, absent a stored preference, `$HOME/.config/ppsspp`) off it.
/// - `XDG_CONFIG_HOME`/`XDG_DATA_HOME` (non-macOS): the SDL GUI's Linux memstick is
///   `$XDG_CONFIG_HOME/ppsspp` (`UI/NativeApp.cpp`).
/// - `EMUCAP_PPSSPP_MEMSTICK`: the emucap fork (`patches/0006-emucap-gui-isolated-memstick.patch`)
///   pins the GUI's memory-stick root to this before its config `Load()`. This is the deterministic
///   fix for macOS, where the stock memstick path comes from `NSUserDefaults`
///   (`UserPreferredMemoryStickDirectoryPath`, else `defaultCurrentDirectory/.config/ppsspp`) which
///   `HOME`/`XDG` cannot fully redirect — so a HITL window can never touch the real profile even if
///   the operator configured a custom memory stick in their own PPSSPP.
fn isolation_env(port: u16) -> Vec<(String, String)> {
    let home = super::emu_home_dir("ppsspp", port);
    let memstick = home.join("memstick");
    // `mut` is used only on non-macOS, where the XDG vars are pushed below.
    #[cfg_attr(target_os = "macos", allow(unused_mut))]
    let mut env = vec![
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        (
            "EMUCAP_PPSSPP_MEMSTICK".to_string(),
            memstick.to_string_lossy().into_owned(),
        ),
    ];
    #[cfg(not(target_os = "macos"))]
    {
        env.push((
            "XDG_CONFIG_HOME".to_string(),
            home.join("config").to_string_lossy().into_owned(),
        ));
        env.push((
            "XDG_DATA_HOME".to_string(),
            home.join("data").to_string_lossy().into_owned(),
        ));
    }
    env
}

/// Display (HITL) mode launches the `PPSSPPSDL` GUI build with the same `--debugger=<port>` and
/// positional content, but omits `--graphics=software` so the window renders with the real GPU
/// backend (Vulkan/GL). The GUI honors `--debugger=<port>` via the repo fork patch
/// (`patches/0005-emucap-gui-debugger-port.patch`): it pins `iRemoteISOPort` to that port and arms
/// `bRemoteDebuggerOnStartup`, so the debugger WebServer binds the requested loopback port at boot.
/// Unlike headless, the GUI does not set `startBreak`, so the game boots running and the human can
/// play immediately while the agent attaches. Both modes run under an isolated per-port profile
/// (`isolation_env`) so a session never reads or pollutes the operator's real PPSSPP config/saves.
fn emu_spec(l: &Launch, ws_port: u16) -> LaunchSpec {
    let mut spec = LaunchSpec::new(l.binary, l.log_path).arg(format!("--debugger={ws_port}"));
    for (k, v) in isolation_env(l.port) {
        spec = spec.env(k, v);
    }
    if l.display {
        spec.arg(l.content)
    } else {
        spec.arg("--graphics=software").arg(l.content)
    }
}

/// The bridge that relays the debugger WebSocket to emucap on `l.port`. Mirrors `launch.sh`:
/// `emucap-ppsspp-bridge <port> <ws_port>` with `EMUCAP_*` env (content/name/session token —
/// `PpssppBridge::new` reads `EMUCAP_CONTENT` itself for `get_rom_info`'s sha1/size).
fn bridge_spec(l: &Launch, ws_port: u16) -> LaunchSpec {
    let mut spec = LaunchSpec::new(l.bridge, l.log_path)
        .arg(l.port.to_string())
        .arg(ws_port.to_string())
        .env("EMUCAP_CONTENT", l.content);
    if let Some(name) = l.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = l.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec.runtime_env(l.runtime)
}

/// Spawn headless PPSSPP (debugger WebSocket), wait until it survives startup and the WebSocket
/// port is actually accepting connections, then spawn the bridge that relays it to emucap on
/// `port`. If PPSSPP never opens the port the launcher fails explicitly and cleans PPSSPP up
/// (rather than an opaque bridge connect failure). If the bridge spawn/settle fails, the launcher
/// terminates PPSSPP.
pub fn launch(l: &Launch) -> io::Result<Launched> {
    let ws_res = resolve_ws_port()?;
    let ws_port = ws_res.port;
    // Free the reserved port for PPSSPP to bind before spawning it.
    drop(ws_res);

    // Pre-create the isolated per-port profile so PPSSPP writes its config/saves there from the
    // first boot, never the operator's real profile (see `isolation_env`).
    let _ = std::fs::create_dir_all(super::emu_home_dir("ppsspp", l.port).join("memstick"));

    let ppsspp_pid = spawn_detached(&emu_spec(l, ws_port))?;
    // PPSSPP PID를 spawn 직후 즉시 기록한다 — 이후 단계(WS 대기·bridge spawn)가 실패해 종료 처리로
    // 넘어가도, terminate가 혹시 놓친 프로세스를 status.owned_instance가 이 pidfile로 찾아 정리할 수
    // 있게 한다. 성공 시 bridge.pid는 아래에서 추가 기록한다.
    write_pidfile(l.log_path, "ppsspp.pid", ppsspp_pid);
    if let Err(e) = wait_survives(
        ppsspp_pid,
        Duration::from_secs(2),
        "PPSSPP exited during startup (crashed — check the launch log)",
    ) {
        let _ = terminate_detached(ppsspp_pid);
        return Err(e);
    }
    if l.display {
        // Keep the macOS display awake for the HITL window and reap the helper (no-op off macOS);
        // the window dies if the display sleeps, same failure mode as the other GUI adapters.
        super::spawn_display_caffeinate(ppsspp_pid);
    }
    if let Err(e) = wait_ws_ready(ppsspp_pid, ws_port, Duration::from_secs(8)) {
        let _ = terminate_detached(ppsspp_pid);
        return Err(e);
    }
    let bridge_pid = match spawn_detached(&bridge_spec(l, ws_port)) {
        Ok(pid) => pid,
        Err(e) => {
            let _ = terminate_detached(ppsspp_pid);
            return Err(e);
        }
    };
    // 브리지가 곧바로 죽지 않았는지 확인한다(잘못된 인자·MCP 즉시 연결실패 등). 죽었으면 launch가 성공을
    // 오보하고 ppsspp를 orphan으로 남기지 않도록 둘 다 정리하고 에러를 낸다.
    if let Err(e) = wait_survives(
        bridge_pid,
        Duration::from_secs(2),
        "emucap-ppsspp-bridge exited during startup (couldn't reach the debugger WebSocket or the MCP listener — check the launch log)",
    ) {
        let _ = terminate_detached(bridge_pid);
        let _ = terminate_detached(ppsspp_pid);
        return Err(e);
    }
    write_pidfile(l.log_path, "bridge.pid", bridge_pid);
    Ok(Launched {
        ppsspp_pid,
        bridge_pid,
        ws_port,
    })
}

/// per-port pidfile을 RUN_DIR(log_path의 부모)에 기록한다(legacy launch.sh와 동일 규약). status가 이걸 읽어
/// 소유 인스턴스 PID를 재발견하므로, agent가 launch 응답을 지나쳐도(다음 턴 등) 자기 것만 kill할 수 있다.
fn write_pidfile(log_path: &Path, name: &str, pid: u32) {
    if let Some(run_dir) = log_path.parent() {
        let _ = std::fs::write(run_dir.join(name), format!("{pid}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bridge_spec, emu_spec, resolve_binary, resolve_bridge, resolve_gui_binary, resolve_ws_port,
        Launch,
    };

    #[cfg(unix)]
    #[test]
    fn wait_survives_passes_a_living_process_and_flags_an_exited_one() {
        use std::time::Duration;
        let mut alive = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        assert!(super::wait_survives(alive.id(), Duration::from_millis(400), "died").is_ok());
        let _ = alive.kill();
        let _ = alive.wait();

        let mut dead = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let dead_pid = dead.id();
        let _ = dead.wait(); // reap so the pid is gone
        assert!(super::wait_survives(dead_pid, Duration::from_secs(1), "died").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn wait_ws_ready_succeeds_once_the_port_is_listening_and_fails_on_dead_process() {
        use std::net::TcpListener;
        use std::time::Duration;

        // A process that stays alive and a port that is already listening → ready immediately.
        let alive = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(super::wait_ws_ready(alive.id(), port, Duration::from_secs(2)).is_ok());
        let mut alive = alive;
        let _ = alive.kill();
        let _ = alive.wait();
        drop(listener);

        // A process that has already exited, and nothing listening → fails fast (dead, not a timeout).
        let mut dead = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let dead_pid = dead.id();
        let _ = dead.wait();
        let free_port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = super::wait_ws_ready(dead_pid, free_port, Duration::from_secs(1)).unwrap_err();
        assert!(err.to_string().contains("exited"));
    }

    use crate::launch::test_env::{lock_env, EnvGuard};
    use std::path::Path;

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn launch_for<'a>(binary: &'a Path, bridge: &'a Path, log: &'a Path) -> Launch<'a> {
        Launch {
            binary,
            bridge,
            content: "/roms/game.iso",
            log_path: log,
            port: 47800,
            name: Some("psp_session"),
            session_token: Some("token"),
            runtime: None,
            display: false,
        }
    }

    #[test]
    fn ws_port_dynamic_allocates_a_free_port() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["PSP_DEBUGGER_PORT"]);
        std::env::remove_var("PSP_DEBUGGER_PORT");
        let a = resolve_ws_port().unwrap();
        assert_ne!(a.port, 0);
    }

    #[test]
    fn ws_port_env_override_wins() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["PSP_DEBUGGER_PORT"]);
        std::env::set_var("PSP_DEBUGGER_PORT", "51500");
        assert_eq!(resolve_ws_port().unwrap().port, 51500);
    }

    #[test]
    fn ws_port_env_override_rejects_non_numeric() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["PSP_DEBUGGER_PORT"]);
        std::env::set_var("PSP_DEBUGGER_PORT", "not-a-port");
        let err = resolve_ws_port().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn emu_spec_passes_content_positionally_not_as_mount_flag() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("PPSSPPHeadless");
        let bridge = dir.path().join("bridge");
        let log = dir.path().join("ppsspp.log");
        let l = launch_for(&binary, &bridge, &log);
        let spec = emu_spec(&l, 48900);
        assert_eq!(
            spec.args,
            vec!["--debugger=48900", "--graphics=software", "/roms/game.iso"]
        );
        // The content must never be attached to `-m`/`--mount` (that only mounts a *second* image
        // on umd1: for ELF+CSO test harnesses — passed alone it leaves the boot list empty).
        assert!(!spec.args.iter().any(|a| a == "-m" || a == "--mount"));
        // --timeout is never passed: it aborts the run on a wall-clock deadline regardless of
        // debugger/WebSocket activity, which would kill an interactive debugging session.
        assert!(!spec.args.iter().any(|a| a.starts_with("--timeout")));
    }

    #[test]
    fn emu_spec_display_drops_software_graphics_for_a_real_window() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("PPSSPPSDL");
        let bridge = dir.path().join("bridge");
        let log = dir.path().join("ppsspp.log");
        let mut l = launch_for(&binary, &bridge, &log);
        l.display = true;
        let spec = emu_spec(&l, 48900);
        // Display (HITL) mode keeps the same --debugger=<port> and positional content, but omits
        // --graphics=software so the GUI window renders with the real GPU backend.
        assert_eq!(spec.args, vec!["--debugger=48900", "/roms/game.iso"]);
        assert!(!spec.args.iter().any(|a| a == "--graphics=software"));
        // The GUI honors --debugger=<port> (fork patch 0005); the port is still passed as the flag.
        assert!(spec.args.iter().any(|a| a == "--debugger=48900"));
    }

    #[test]
    fn emu_spec_display_isolates_the_profile_from_the_real_home() {
        // A HITL window must never read or write the operator's real PPSSPP config/saves: the spec
        // redirects HOME (and the fork's memstick pin) to an emucap-owned per-port dir, not the real
        // profile. Regression guard for the display:true isolation fix.
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_EMU_HOME", "HOME"]);
        let emu_home = tempfile::tempdir().unwrap();
        std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());
        // A distinct "real" HOME that must not leak into the launched GUI.
        let real_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", real_home.path());

        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("PPSSPPSDL");
        let bridge = dir.path().join("bridge");
        let log = dir.path().join("ppsspp.log");
        let mut l = launch_for(&binary, &bridge, &log);
        l.port = 47850;
        l.display = true;
        let spec = emu_spec(&l, 48900);

        // HOME points at the emucap-owned per-port dir (under EMUCAP_EMU_HOME), not the real HOME.
        let home = &spec
            .env
            .iter()
            .find(|(k, _)| k == "HOME")
            .expect("display spec must set an isolated HOME")
            .1;
        let expected_home = emu_home.path().join("ppsspp/47850");
        assert_eq!(Path::new(home), expected_home);
        assert_ne!(Path::new(home), real_home.path());
        assert!(!home.contains(real_home.path().to_str().unwrap()));

        // The memory stick (config + saves) is pinned into that isolated dir — the deterministic
        // macOS fix, where HOME/XDG alone cannot redirect the NSUserDefaults-derived memstick.
        let memstick = &spec
            .env
            .iter()
            .find(|(k, _)| k == "EMUCAP_PPSSPP_MEMSTICK")
            .expect("display spec must pin an isolated memstick")
            .1;
        assert!(Path::new(memstick).starts_with(&expected_home));
    }

    #[test]
    fn resolve_gui_binary_uses_repo_local_sdl_app() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PPSSPP_GUI_BIN"]);
        std::env::remove_var("EMUCAP_PPSSPP_GUI_BIN");
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(
            "adapters/ppsspp/work/ppsspp/build-headless/PPSSPPSDL.app/Contents/MacOS/PPSSPPSDL",
        );
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"fake PPSSPPSDL").unwrap();
        #[cfg(unix)]
        make_executable(&bin);
        assert_eq!(resolve_gui_binary(dir.path()), Some(bin));
    }

    #[test]
    fn resolve_gui_binary_honors_explicit_env() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PPSSPP_GUI_BIN"]);
        let dir = tempfile::tempdir().unwrap();
        let explicit = dir.path().join("my-ppsspp-sdl");
        std::fs::write(&explicit, b"fake").unwrap();
        #[cfg(unix)]
        make_executable(&explicit);
        std::env::set_var("EMUCAP_PPSSPP_GUI_BIN", &explicit);
        assert_eq!(resolve_gui_binary(dir.path()), Some(explicit));
    }

    #[test]
    fn resolve_gui_binary_missing_returns_none() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PPSSPP_GUI_BIN"]);
        std::env::remove_var("EMUCAP_PPSSPP_GUI_BIN");
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_gui_binary(dir.path()), None);
    }

    #[test]
    fn bridge_spec_mirrors_launch_sh_argv_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("PPSSPPHeadless");
        let bridge = dir.path().join("bridge");
        let log = dir.path().join("ppsspp.log");
        let l = launch_for(&binary, &bridge, &log);
        let spec = bridge_spec(&l, 48900);
        assert_eq!(spec.program, bridge);
        assert_eq!(spec.args, vec!["47800", "48900"]);
        assert!(spec
            .env
            .contains(&("EMUCAP_CONTENT".to_string(), "/roms/game.iso".to_string())));
        assert!(spec
            .env
            .contains(&("EMUCAP_NAME".to_string(), "psp_session".to_string())));
        assert!(spec
            .env
            .contains(&("EMUCAP_SESSION_TOKEN".to_string(), "token".to_string())));
    }

    #[test]
    fn resolve_binary_uses_repo_local_build_headless() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PPSSPP_BIN"]);
        std::env::remove_var("EMUCAP_PPSSPP_BIN");
        let dir = tempfile::tempdir().unwrap();
        let bin = dir
            .path()
            .join("adapters/ppsspp/work/ppsspp/build-headless/PPSSPPHeadless");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"fake PPSSPPHeadless").unwrap();
        #[cfg(unix)]
        make_executable(&bin);
        assert_eq!(resolve_binary(dir.path()), Some(bin));
    }

    #[test]
    fn resolve_binary_honors_explicit_env() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PPSSPP_BIN"]);
        let dir = tempfile::tempdir().unwrap();
        let explicit = dir.path().join("my-ppsspp-headless");
        std::fs::write(&explicit, b"fake").unwrap();
        #[cfg(unix)]
        make_executable(&explicit);
        std::env::set_var("EMUCAP_PPSSPP_BIN", &explicit);
        assert_eq!(resolve_binary(dir.path()), Some(explicit));
    }

    #[test]
    fn resolve_binary_missing_returns_none() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PPSSPP_BIN"]);
        std::env::remove_var("EMUCAP_PPSSPP_BIN");
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_binary(dir.path()), None);
    }

    #[test]
    fn resolve_bridge_prefers_release_then_debug() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PSP_BRIDGE_BIN"]);
        std::env::remove_var("EMUCAP_PSP_BRIDGE_BIN");
        let dir = tempfile::tempdir().unwrap();
        let name = super::bridge_binary_name();
        let debug = dir.path().join("target/debug").join(name);
        std::fs::create_dir_all(debug.parent().unwrap()).unwrap();
        std::fs::write(&debug, b"fake bridge").unwrap();
        #[cfg(unix)]
        make_executable(&debug);
        assert_eq!(resolve_bridge(dir.path()), Some(debug.clone()));

        let release = dir.path().join("target/release").join(name);
        std::fs::create_dir_all(release.parent().unwrap()).unwrap();
        std::fs::write(&release, b"fake bridge").unwrap();
        #[cfg(unix)]
        make_executable(&release);
        assert_eq!(resolve_bridge(dir.path()), Some(release));
    }

    #[test]
    fn resolve_bridge_honors_explicit_env() {
        let _lock = lock_env();
        let _env = EnvGuard::new(&["EMUCAP_PSP_BRIDGE_BIN"]);
        let dir = tempfile::tempdir().unwrap();
        let explicit = dir.path().join("my-bridge");
        std::fs::write(&explicit, b"fake").unwrap();
        #[cfg(unix)]
        make_executable(&explicit);
        std::env::set_var("EMUCAP_PSP_BRIDGE_BIN", &explicit);
        assert_eq!(resolve_bridge(dir.path()), Some(explicit));
    }
}
