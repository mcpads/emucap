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
