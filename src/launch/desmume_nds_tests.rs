use super::{bridge_spec, emu_spec, resolve_binary, resolve_bridge, resolve_gdb_port, Launch};

#[cfg(unix)]
#[test]
fn wait_survives_passes_a_living_process_and_flags_an_exited_one() {
    use std::time::Duration;
    // A bridge (or desmume) still alive after the settle passes; one that already exited fails,
    // so a process that dies during startup surfaces as a launch error, not a false success.
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
        content: "/roms/game.nds",
        log_path: log,
        port: 47800,
        name: Some("nds_session"),
        session_token: Some("token"),
        runtime: None,
        display: false,
    }
}

#[test]
fn gdb_port_dynamic_allocates_distinct_free_ports() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["NDS_ARM9_GDB_PORT", "NDS_ARM7_GDB_PORT"]);
    std::env::remove_var("NDS_ARM9_GDB_PORT");
    std::env::remove_var("NDS_ARM7_GDB_PORT");
    // 두 예약을 동시에 쥐면 OS가 서로 다른 미사용 포트를 배정한다(파생 +1000/+1001의 인접-세션 겹침 없음).
    let a = resolve_gdb_port("NDS_ARM9_GDB_PORT").unwrap();
    let b = resolve_gdb_port("NDS_ARM7_GDB_PORT").unwrap();
    assert_ne!(a.port, 0);
    assert_ne!(b.port, 0);
    assert_ne!(a.port, b.port);
}

#[test]
fn gdb_port_env_override_wins() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["NDS_ARM9_GDB_PORT"]);
    std::env::set_var("NDS_ARM9_GDB_PORT", "51000");
    assert_eq!(resolve_gdb_port("NDS_ARM9_GDB_PORT").unwrap().port, 51000);
}

#[test]
fn gdb_port_env_override_rejects_non_numeric() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["NDS_ARM7_GDB_PORT"]);
    std::env::set_var("NDS_ARM7_GDB_PORT", "not-a-port");
    let err = resolve_gdb_port("NDS_ARM7_GDB_PORT").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn emu_spec_mirrors_launch_sh_argv() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("desmume-cli");
    let bridge = dir.path().join("bridge");
    let log = dir.path().join("nds.log");
    let l = launch_for(&binary, &bridge, &log);
    let spec = emu_spec(&l, 48800, 48801);
    assert_eq!(
        spec.args,
        vec![
            "--arm9gdb",
            "48800",
            "--arm7gdb",
            "48801",
            "--disable-sound",
            "/roms/game.nds",
        ]
    );
}

#[test]
fn bridge_spec_mirrors_launch_sh_argv_and_env() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("desmume-cli");
    let bridge = dir.path().join("bridge");
    let log = dir.path().join("nds.log");
    let l = launch_for(&binary, &bridge, &log);
    let spec = bridge_spec(&l, 48800, 48801);
    assert_eq!(spec.program, bridge);
    assert_eq!(
        spec.args,
        vec!["47800", "127.0.0.1:48800", "127.0.0.1:48801"]
    );
    assert!(spec
        .env
        .contains(&("EMUCAP_CONTENT".to_string(), "/roms/game.nds".to_string())));
    assert!(spec
        .env
        .contains(&("EMUCAP_NAME".to_string(), "nds_session".to_string())));
    assert!(spec
        .env
        .contains(&("EMUCAP_SESSION_TOKEN".to_string(), "token".to_string())));
}

#[test]
fn resolve_binary_uses_repo_local_build_headless() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_DESMUME_BIN"]);
    std::env::remove_var("EMUCAP_DESMUME_BIN");
    let dir = tempfile::tempdir().unwrap();
    let cli = dir
        .path()
        .join("adapters/desmume-nds/work/src/desmume/src/frontend/posix/build-headless/cli")
        .join(if cfg!(windows) {
            "desmume-cli.exe"
        } else {
            "desmume-cli"
        });
    std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
    std::fs::write(&cli, b"fake desmume-cli").unwrap();
    #[cfg(unix)]
    make_executable(&cli);
    assert_eq!(resolve_binary(dir.path()), Some(cli));
}

#[test]
fn resolve_binary_honors_explicit_env() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_DESMUME_BIN"]);
    let dir = tempfile::tempdir().unwrap();
    let explicit = dir.path().join("my-desmume");
    std::fs::write(&explicit, b"fake").unwrap();
    #[cfg(unix)]
    make_executable(&explicit);
    std::env::set_var("EMUCAP_DESMUME_BIN", &explicit);
    assert_eq!(resolve_binary(dir.path()), Some(explicit));
}

#[test]
fn resolve_binary_missing_returns_none() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_DESMUME_BIN"]);
    std::env::remove_var("EMUCAP_DESMUME_BIN");
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(resolve_binary(dir.path()), None);
}

#[test]
fn resolve_bridge_prefers_release_then_debug() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_NDS_BRIDGE_BIN"]);
    std::env::remove_var("EMUCAP_NDS_BRIDGE_BIN");
    let dir = tempfile::tempdir().unwrap();
    let name = super::bridge_binary_name();
    let debug = dir.path().join("target/debug").join(name);
    std::fs::create_dir_all(debug.parent().unwrap()).unwrap();
    std::fs::write(&debug, b"fake bridge").unwrap();
    #[cfg(unix)]
    make_executable(&debug);
    // Only debug exists → picked.
    assert_eq!(resolve_bridge(dir.path()), Some(debug.clone()));

    let release = dir.path().join("target/release").join(name);
    std::fs::create_dir_all(release.parent().unwrap()).unwrap();
    std::fs::write(&release, b"fake bridge").unwrap();
    #[cfg(unix)]
    make_executable(&release);
    // Release now exists → preferred over debug.
    assert_eq!(resolve_bridge(dir.path()), Some(release));
}

#[test]
fn resolve_bridge_honors_explicit_env() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_NDS_BRIDGE_BIN"]);
    let dir = tempfile::tempdir().unwrap();
    let explicit = dir.path().join("my-bridge");
    std::fs::write(&explicit, b"fake").unwrap();
    #[cfg(unix)]
    make_executable(&explicit);
    std::env::set_var("EMUCAP_NDS_BRIDGE_BIN", &explicit);
    assert_eq!(resolve_bridge(dir.path()), Some(explicit));
}
