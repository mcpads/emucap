use super::*;
use std::path::Path;

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn emu_home_layout_is_base_emu_port() {
    let got = join_emu_home(Path::new("/x/emucap"), "flycast", 47800);
    assert_eq!(got, Path::new("/x/emucap/flycast/47800"));
}

#[test]
fn launch_spec_accumulates_args_env_cwd() {
    let spec = LaunchSpec::new("/bin/emu", "/tmp/emu.log")
        .arg("game.cue")
        .args(["-force_module", "ss"])
        .env("EMUCAP_PORT", "47800")
        .env("HOME", "/x/home")
        .cwd("/work");
    assert_eq!(spec.program, Path::new("/bin/emu"));
    assert_eq!(spec.args, vec!["game.cue", "-force_module", "ss"]);
    assert_eq!(
        spec.env,
        vec![
            ("EMUCAP_PORT".to_string(), "47800".to_string()),
            ("HOME".to_string(), "/x/home".to_string()),
        ]
    );
    assert_eq!(spec.cwd.as_deref(), Some(Path::new("/work")));
    assert_eq!(spec.log_path, Path::new("/tmp/emu.log"));
}

#[cfg(not(windows))]
#[test]
fn path_lookup_candidates_use_plain_name_on_unix() {
    assert_eq!(
        executable_candidates(Path::new("/usr/bin"), "python3"),
        vec![Path::new("/usr/bin/python3")]
    );
}

#[cfg(windows)]
#[test]
fn path_lookup_candidates_include_windows_exe_suffix() {
    let candidates = executable_candidates(Path::new(r"C:\Python"), "python");
    assert!(candidates.iter().any(|p| p
        .to_string_lossy()
        .eq_ignore_ascii_case(r"C:\Python\python.exe")));
}

#[test]
fn copy_dir_replace_replaces_directory_without_stale_files() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(src.join("Contents/Resources")).unwrap();
    std::fs::create_dir_all(dst.join("Contents/Resources")).unwrap();
    std::fs::write(src.join("Contents/Resources/new.txt"), "new").unwrap();
    std::fs::write(dst.join("Contents/Resources/stale.txt"), "old").unwrap();

    copy_dir_replace(&src, &dst).unwrap();

    assert_eq!(
        std::fs::read_to_string(dst.join("Contents/Resources/new.txt")).unwrap(),
        "new"
    );
    assert!(!dst.join("Contents/Resources/stale.txt").exists());
}

#[test]
fn copy_file_replace_replaces_file_without_touching_source() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::write(&src, "new").unwrap();
    std::fs::write(&dst, "old").unwrap();

    copy_file_replace(&src, &dst).unwrap();

    assert_eq!(std::fs::read_to_string(&src).unwrap(), "new");
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "new");
}

#[test]
fn copy_file_replace_refuses_directory_destination() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::write(&src, "new").unwrap();
    std::fs::create_dir_all(&dst).unwrap();

    let err = copy_file_replace(&src, &dst).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(dst.is_dir());
}

#[test]
fn first_existing_file_skips_missing_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    let existing = dir.path().join("existing");
    std::fs::write(&existing, "ok").unwrap();
    #[cfg(unix)]
    make_executable(&existing);

    assert_eq!(
        first_existing_file([missing, existing.clone()]).unwrap(),
        existing
    );
}

#[cfg(unix)]
#[test]
fn first_existing_file_skips_non_executable_files_on_unix() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    let runnable = dir.path().join("runnable");
    std::fs::write(&plain, "plain").unwrap();
    std::fs::write(&runnable, "run").unwrap();
    make_executable(&runnable);

    assert_eq!(
        first_existing_file([plain, runnable.clone()]).unwrap(),
        runnable
    );
}

#[test]
fn copy_dir_replace_refuses_file_destination() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("new.txt"), "new").unwrap();
    std::fs::write(&dst, "old").unwrap();

    let err = copy_dir_replace(&src, &dst).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "old");
}

#[test]
fn copy_dir_replace_removes_staging_temp_on_copy_failure() {
    let dir = tempfile::tempdir().unwrap();
    // A missing source makes the initial recursive copy fail *after* the staging temp dir has
    // already been created (copy_dir_contents mkdirs the temp, then read_dir(src) errors), which
    // exercises the initial-copy error path.
    let missing_src = dir.path().join("missing-src");
    let dst = dir.path().join("dst");

    let err = copy_dir_replace(&missing_src, &dst).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

    // No staging temp (`.dst.tmp.<pid>.<nanos>`) may be left behind on the failed initial copy.
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".dst.tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging temp left behind after failed initial copy: {leftovers:?}"
    );
    assert!(!dst.exists());
}

#[cfg(unix)]
#[test]
fn copy_dir_replace_refuses_symlinked_directory_destination() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let outside = dir.path().join("outside");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(src.join("new.txt"), "new").unwrap();
    std::fs::write(outside.join("sentinel.txt"), "keep").unwrap();
    std::os::unix::fs::symlink(&outside, &dst).unwrap();

    let err = copy_dir_replace(&src, &dst).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(std::fs::symlink_metadata(&dst)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_to_string(outside.join("sentinel.txt")).unwrap(),
        "keep"
    );
    assert!(!outside.join("new.txt").exists());
}

#[cfg(unix)]
#[test]
fn copy_dir_replace_preserves_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(src.join("Contents/Resources")).unwrap();
    std::fs::write(src.join("Contents/Resources/real.txt"), "real").unwrap();
    std::os::unix::fs::symlink("real.txt", src.join("Contents/Resources/link.txt")).unwrap();

    copy_dir_replace(&src, &dst).unwrap();

    let copied_link = dst.join("Contents/Resources/link.txt");
    assert!(std::fs::symlink_metadata(&copied_link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_link(copied_link).unwrap(),
        Path::new("real.txt")
    );
}

// Verifies the actual detached spawn + log redirection on this platform (Unix here).
#[cfg(unix)]
#[test]
fn spawn_detached_runs_and_redirects_to_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("out.log");
    let spec = LaunchSpec::new("/bin/echo", &log).arg("emucap-spawn-ok");
    let pid = spawn_detached(&spec).expect("spawn");
    assert!(pid > 0);
    for _ in 0..50 {
        if std::fs::read_to_string(&log)
            .map(|s| s.contains("emucap-spawn-ok"))
            .unwrap_or(false)
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!(
        "log did not receive echo output: {:?}",
        std::fs::read_to_string(&log)
    );
}

// A reaped helper (the caffeinate spawn path) must not linger as a zombie in the long-lived MCP.
// A fast-exiting child whose `Child` is merely dropped stays a zombie — `kill(pid,0)` still returns
// 0 — until its parent exits. `spawn_reaped`'s reaper thread waits on it, freeing the pid so
// `process_alive` turns false; without the reaper this would spin to the deadline and fail.
#[cfg(unix)]
#[test]
fn spawn_reaped_reaps_fast_child_so_it_does_not_linger_as_zombie() {
    let mut cmd = std::process::Command::new("true");
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let pid = spawn_reaped(cmd).expect("spawn");
    assert!(pid > 0);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while process_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !process_alive(pid),
        "child pid {pid} still present after exit — not reaped (zombie)"
    );
}

// A process that ignores SIGTERM (like desmume-cli, per adapters/desmume-nds/README.md) must still
// be killed by terminate_detached's SIGTERM→SIGKILL escalation, so a failed NDS launch never strands
// it untracked. The shell installs SIG_IGN for SIGTERM, writes a ready marker, then `exec sleep`
// (SIG_IGN survives exec) — waiting for the marker avoids racing the SIGTERM against trap setup.
#[cfg(unix)]
#[test]
fn terminate_detached_escalates_to_sigkill_when_sigterm_ignored() {
    use std::os::unix::process::ExitStatusExt;
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready");
    let script = format!("trap '' TERM; : > '{}'; exec sleep 30", ready.display());
    let mut child = std::process::Command::new("sh")
        .args(["-c", &script])
        .spawn()
        .expect("spawn SIGTERM-ignoring test process");
    for _ in 0..200 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(ready.exists(), "test process never signalled ready");
    terminate_detached(child.id()).expect("terminate");
    let status = child.wait().expect("wait");
    assert_eq!(
        status.signal(),
        Some(9),
        "a SIGTERM-ignoring process must be escalated to SIGKILL"
    );
}
