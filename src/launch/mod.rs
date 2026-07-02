//! Cross-platform emulator launch.
//!
//! Starts an emulator from the (cross-platform) Rust core instead of a per-OS shell
//! script, and gives each emulator an emucap-owned config/data directory so the user's
//! real emulator install is never touched.
//!
//! This is the launcher foundation: a `LaunchSpec` describing one process, the
//! emucap-owned directory resolution, and a detached spawn. The per-adapter spec
//! builders and the MCP tool that drives them are layered on top.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub mod flycast;
pub mod mame;
pub mod mednafen;
pub mod mesen;
pub mod spec;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

/// Base directory for emucap-owned emulator data, per OS. `EMUCAP_EMU_HOME` overrides it.
fn emu_home_base() -> PathBuf {
    if let Some(base) = std::env::var_os("EMUCAP_EMU_HOME") {
        return PathBuf::from(base);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home).join("Library/Application Support/emucap");
    }
    #[cfg(target_os = "windows")]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return Path::new(&local).join("emucap");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return Path::new(&xdg).join("emucap");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(".local/share/emucap");
        }
    }
    std::env::temp_dir().join("emucap")
}

fn join_emu_home(base: &Path, emu: &str, port: u16) -> PathBuf {
    base.join(emu).join(port.to_string())
}

/// The emucap-owned directory for one emulator + port. The emulator runs against this so
/// its config and saves stay out of the user's real emulator directory.
pub fn emu_home_dir(emu: &str, port: u16) -> PathBuf {
    join_emu_home(&emu_home_base(), emu, port)
}

/// OS-agnostic description of how to start one emulator process.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaunchSpec {
    /// The emulator executable to run.
    pub program: PathBuf,
    /// Arguments passed to the executable, in order.
    pub args: Vec<String>,
    /// Environment variables set for the child, on top of the inherited environment.
    pub env: Vec<(String, String)>,
    /// File that receives the child's stdout + stderr.
    pub log_path: PathBuf,
    /// Working directory for the child, if it must run from a specific place.
    pub cwd: Option<PathBuf>,
}

impl LaunchSpec {
    pub fn new(program: impl Into<PathBuf>, log_path: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            log_path: log_path.into(),
            ..Default::default()
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(it.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    fn build_command(&self) -> std::io::Result<Command> {
        if let Some(parent) = self.log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        let err = out.try_clone()?;
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        Ok(cmd)
    }
}

/// Spawn the emulator detached from this process, stdio redirected to the log; return its PID.
///
/// The emulator must survive an MCP restart, so on Unix it starts in a new session (setsid)
/// and on Windows with DETACHED_PROCESS + a new process group. A reaper thread waits on the
/// child so it does not linger as a zombie while the MCP runs; if the MCP exits first, the
/// child has already left this session and is reparented to init.
pub fn spawn_detached(spec: &LaunchSpec) -> std::io::Result<u32> {
    let mut cmd = spec.build_command()?;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid() is async-signal-safe and is the only action before exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    let mut child = cmd.spawn()?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

pub(crate) fn terminate_detached(pid: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        let pid_s = pid.to_string();
        let status = Command::new("taskkill")
            .args(["/PID", &pid_s, "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "taskkill failed for pid {pid}"
            )))
        }
    }
}

pub(crate) fn copy_file_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if dst.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("destination is a directory: {}", dst.display()),
        ));
    }
    let tmp = unique_sibling_path(dst, "tmp");
    if let Err(e) = std::fs::copy(src, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Ok(perms) = std::fs::metadata(src).map(|m| m.permissions()) {
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    #[cfg(windows)]
    {
        if !path_exists_or_symlink(dst) {
            return rename_file_tmp(&tmp, dst);
        }
        let backup = unique_sibling_path(dst, "old");
        if let Err(e) = std::fs::rename(dst, &backup) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        match std::fs::rename(&tmp, dst) {
            Ok(()) => {
                let _ = std::fs::remove_file(&backup);
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::rename(&backup, dst);
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
    #[cfg(not(windows))]
    {
        rename_file_tmp(&tmp, dst)
    }
}

#[cfg(windows)]
fn path_exists_or_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

pub(crate) fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

pub(crate) fn has_symlink_component_under(base: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(base) else {
        return false;
    };
    let mut cur = base.to_path_buf();
    for component in rel.components() {
        cur.push(component.as_os_str());
        if is_symlink(&cur) {
            return true;
        }
    }
    false
}

fn rename_file_tmp(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(tmp, dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(tmp);
            Err(e)
        }
    }
}

#[cfg(unix)]
fn copy_symlink_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let target = std::fs::read_link(src)?;
    let tmp = unique_sibling_path(dst, "link");
    std::os::unix::fs::symlink(target, &tmp)?;
    if dst.exists() || dst.is_symlink() {
        std::fs::remove_file(dst)?;
    }
    match std::fs::rename(&tmp, dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub(crate) fn copy_dir_contents(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_contents(&src_path, &dst_path)?;
        } else if ty.is_file() {
            copy_file_replace(&src_path, &dst_path)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                copy_symlink_replace(&src_path, &dst_path)?;
            }
            #[cfg(not(unix))]
            {
                copy_file_replace(&src_path, &dst_path)?;
            }
        }
    }
    Ok(())
}

fn unique_sibling_path(path: &Path, label: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("runtime-dir");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!(".{name}.{label}.{}.{}", std::process::id(), nanos))
}

pub(crate) fn copy_dir_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if is_symlink(dst) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "destination is a symlink, refusing to replace: {}",
                dst.display()
            ),
        ));
    }
    if dst.exists() && !dst.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("destination is not a directory: {}", dst.display()),
        ));
    }

    let tmp = unique_sibling_path(dst, "tmp");
    let backup = unique_sibling_path(dst, "old");
    copy_dir_contents(src, &tmp)?;

    if !dst.exists() {
        return match std::fs::rename(&tmp, dst) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                Err(e)
            }
        };
    }

    if let Err(e) = std::fs::rename(dst, &backup) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    match std::fs::rename(&tmp, dst) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&backup);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::rename(&backup, dst);
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

fn executable_candidates(dir: &Path, exe: &str) -> Vec<PathBuf> {
    let plain = dir.join(exe);
    #[cfg(windows)]
    {
        if Path::new(exe).extension().is_some() {
            return vec![plain];
        }
        let pathext = std::env::var_os("PATHEXT")
            .unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD"));
        let mut candidates = vec![plain];
        candidates.extend(
            pathext
                .to_string_lossy()
                .split(';')
                .filter(|ext| !ext.trim().is_empty())
                .map(|ext| dir.join(format!("{exe}{}", ext.trim()))),
        );
        candidates
    }
    #[cfg(not(windows))]
    {
        vec![plain]
    }
}

pub(crate) fn first_existing_file(
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    candidates.into_iter().find(|p| is_runnable_file(p))
}

/// First executable entry on `PATH`. `std::env::split_paths` handles the per-OS separator.
pub(crate) fn find_on_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| executable_candidates(&dir, exe))
        .find(|c| is_runnable_file(c))
}

pub(crate) fn is_runnable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        true
    }
}
