use super::{
    default_rompath, gdb_port_for_emucap_port, repo_local_binary, resolve_bridge_launch,
    resolve_cbus0, resolve_flop2, Launch,
};
use crate::test_env::{lock_env, EnvGuard};
#[cfg(unix)]
use std::path::Path;
#[cfg(any(target_os = "macos", windows))]
use std::path::PathBuf;

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
        sound: false,
        cbus0: None,
    };

    let selected = resolve_bridge_launch(&launch, 48800).unwrap();
    assert_eq!(selected.kind, "rust");
    assert_eq!(selected.spec.program, bridge_bin);
    assert_eq!(selected.spec.args, vec!["47800", "127.0.0.1:48800"]);
}

#[test]
fn python_bridge_selection_is_rejected() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_PC98_BRIDGE"]);
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_PC98_BRIDGE", "python");

    let err = super::resolve_bridge_runtime(dir.path()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("Python fallback was removed"));
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
        sound: false,
        cbus0: None,
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
fn resolve_cbus0_prefers_the_typed_launch_choice_over_the_legacy_environment() {
    assert_eq!(
        resolve_cbus0(Some("pc9801_86"), Some("pc9801_26")),
        Some("pc9801_86")
    );
    assert_eq!(resolve_cbus0(None, Some("pc9801_26")), Some("pc9801_26"));
    assert_eq!(resolve_cbus0(None, None), None);
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
        sound: false,
        cbus0: None,
    };

    let err = resolve_bridge_launch(&launch, 48800).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert!(err.to_string().contains("EMUCAP_PC98_BRIDGE_BIN"));
}
