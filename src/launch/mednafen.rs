//! Mednafen (Saturn / PSX / PC Engine / PC-FX / Mega Drive / WonderSwan / Neo Geo Pocket) launch orchestration. One
//! built binary handles every system; the caller passes the force_module. We run a per-port *copy* of
//! the binary so that
//! rebuilding the shared work tree doesn't disturb a running instance (the copy is a separate inode).

use super::spec::{mednafen_spec, SpecOpts};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

pub const PCFX_BIOS_SHA256: &str =
    "4b44ccf5d84cc83daa2e6a2bee00fdafa14eb58bdf5859e96d8861a891675417";
const PCFX_BIOS_SIZE: u64 = 1024 * 1024;

fn default_pcfx_bios_source() -> PathBuf {
    super::emu_home_base().join("firmware/pcfx.rom")
}

/// Resolve the operator-supplied PC-FX BIOS without touching Mednafen's user profile.
///
/// Source order: `EMUCAP_PCFX_BIOS`, then `<emucap-home>/firmware/pcfx.rom`.
/// Mednafen's PC-FX module requires the version 1.00 BIOS; fail before process spawn when
/// the file is missing, wrong-sized, or not that known image.
pub fn resolve_pcfx_bios() -> std::io::Result<PathBuf> {
    let explicit = std::env::var_os("EMUCAP_PCFX_BIOS");
    let path = explicit
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_pcfx_bios_source);
    if explicit.is_some() && !path.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "EMUCAP_PCFX_BIOS must be an absolute path",
        ));
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        Error::new(
            error.kind(),
            format!(
                "PC-FX needs the version 1.00 pcfx.rom BIOS: set EMUCAP_PCFX_BIOS or place it at {}: {error}",
                default_pcfx_bios_source().display()
            ),
        )
    })?;
    if bytes.len() as u64 != PCFX_BIOS_SIZE {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "PC-FX BIOS must be {PCFX_BIOS_SIZE} bytes, got {} at {}",
                bytes.len(),
                path.display()
            ),
        ));
    }
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != PCFX_BIOS_SHA256 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "PC-FX BIOS is not the supported version 1.00 image: sha256={actual} path={}",
                path.display()
            ),
        ));
    }
    Ok(path)
}

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
    /// force_module (ss / psx / pce / pcfx / md / wswan / ngp), or None to let Mednafen auto-detect.
    pub module: Option<&'a str>,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
    pub headless: bool,
    /// Explicit audio-output policy. False preserves the debugger-oriented silent default.
    pub sound: bool,
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

/// Launch Mednafen detached for emucap. PC-FX is given an explicitly validated operator-supplied
/// BIOS path so the launch never edits or depends on Mednafen's user profile. Other systems retain
/// Mednafen's normal firmware lookup. Returns the child pid.
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
    let pcfx_bios = if l.module == Some("pcfx") {
        Some(resolve_pcfx_bios()?)
    } else {
        None
    };
    let spec = mednafen_spec(
        &run_binary,
        l.log_path,
        l.module,
        l.sound,
        pcfx_bios.as_deref(),
        &opts,
    );
    super::spawn_detached(&spec)
}

#[cfg(test)]
#[path = "mednafen_tests.rs"]
mod tests;
