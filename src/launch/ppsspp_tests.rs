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

use crate::test_env::{lock_env, EnvGuard};
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
    let bin = dir
        .path()
        .join("adapters/ppsspp/work/ppsspp/build-headless/PPSSPPSDL.app/Contents/MacOS/PPSSPPSDL");
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
        .join("adapters/ppsspp/work/ppsspp/build-headless")
        .join(if cfg!(windows) {
            "Release/PPSSPPHeadless.exe"
        } else {
            "PPSSPPHeadless"
        });
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
