//! Mednafen (Saturn / PSX / PC Engine / Mega Drive) launch orchestration. One built binary handles all
//! four systems; the caller passes the force_module. We run a per-port *copy* of the binary so that
//! rebuilding the shared work tree doesn't disturb a running instance (the copy is a separate inode).

use super::spec::{mednafen_spec, SpecOpts};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Resolve the Mednafen binary. Returns `(path, explicit)`: `explicit == true` means the caller pinned
/// `MEDNAFEN_BIN`, so it's trusted as-is and must not be copied. Repo-local and PATH binaries are copied
/// per port before launch.
pub fn resolve_binary(repo_root: &Path) -> Option<(PathBuf, bool)> {
    if let Some(explicit) = std::env::var_os("MEDNAFEN_BIN") {
        let p = PathBuf::from(explicit);
        if super::is_runnable_file(&p) {
            return Some((p, true));
        }
    }
    repo_local_binary(repo_root)
        .or_else(|| super::first_existing_file(default_install_candidates()))
        .or_else(|| super::find_on_path("mednafen"))
        .map(|p| (p, false))
}

pub fn default_install_candidates() -> Vec<PathBuf> {
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        Vec::new()
    }
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    {
        let mut candidates = Vec::new();
        #[cfg(windows)]
        {
            for key in [
                "LOCALAPPDATA",
                "ProgramFiles",
                "ProgramFiles(x86)",
                "USERPROFILE",
            ] {
                if let Some(base) = std::env::var_os(key).map(PathBuf::from) {
                    candidates.push(base.join("Programs/Mednafen/mednafen.exe"));
                    candidates.push(base.join("Mednafen/mednafen.exe"));
                    candidates.push(base.join("mednafen/mednafen.exe"));
                }
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                candidates.push(home.join(".local/bin/mednafen"));
            }
        }
        candidates
    }
}

fn repo_local_binary(repo_root: &Path) -> Option<PathBuf> {
    let src = repo_root.join("adapters/mednafen/work/mednafen/src");
    let name = if cfg!(windows) {
        "mednafen.exe"
    } else {
        "mednafen"
    };
    let p = src.join(name);
    super::is_runnable_file(&p).then_some(p)
}

fn default_binary_name() -> &'static str {
    if cfg!(windows) {
        "mednafen.exe"
    } else {
        "mednafen"
    }
}

fn run_binary_path(src: &Path, dir: &Path) -> PathBuf {
    dir.join(
        src.file_name()
            .unwrap_or_else(|| OsStr::new(default_binary_name())),
    )
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    /// The caller pinned MEDNAFEN_BIN — run it in place instead of copying.
    pub explicit_binary: bool,
    pub content: &'a str,
    /// force_module (ss / psx / pce / md), or None to let Mednafen auto-detect.
    pub module: Option<&'a str>,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
    pub headless: bool,
}

fn copy_run_binary(src: &Path, dst: &Path) -> std::io::Result<()> {
    super::copy_file_replace(src, dst)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Launch Mednafen detached for emucap. BIOS (PSX scph550x, PCE syscard) is the user's, read by Mednafen
/// from its own firmware dir — not the launcher's concern. Returns the child pid.
pub fn launch(l: &Launch) -> std::io::Result<u32> {
    let run_binary = if l.explicit_binary {
        l.binary.to_path_buf()
    } else {
        let dir = super::emu_home_dir("mednafen", l.port);
        std::fs::create_dir_all(&dir)?;
        let dst = run_binary_path(l.binary, &dir);
        copy_run_binary(l.binary, &dst)?;
        dst
    };
    let opts = SpecOpts {
        content: l.content,
        port: l.port,
        name: l.name,
        session_token: l.session_token,
        runtime: l.runtime,
        headless: l.headless,
    };
    let spec = mednafen_spec(&run_binary, l.log_path, l.module, &opts);
    super::spawn_detached(&spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::sync::Mutex;

    #[cfg(windows)]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn copy_run_binary_replaces_existing_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::write(&src, b"new").unwrap();
        std::fs::write(&dst, b"old").unwrap();

        copy_run_binary(&src, &dst).unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"new");
        assert_eq!(std::fs::read(&src).unwrap(), b"new");
    }

    #[test]
    fn repo_local_candidate_is_platform_native() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("adapters/mednafen/work/mednafen/src");
        std::fs::create_dir_all(&src).unwrap();
        let expected = src.join(default_binary_name());
        std::fs::write(&expected, b"fake mednafen").unwrap();
        #[cfg(unix)]
        make_executable(&expected);

        assert_eq!(repo_local_binary(dir.path()).unwrap(), expected);
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
        assert!(candidates.contains(&base.join("Programs/Mednafen/mednafen.exe")));
    }

    #[test]
    fn run_copy_preserves_source_binary_name() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(if cfg!(windows) {
            "mednafen.exe"
        } else {
            "mednafen"
        });
        let run_dir = dir.path().join("run");

        assert_eq!(
            run_binary_path(&src, &run_dir),
            run_dir.join(default_binary_name())
        );
    }
}
