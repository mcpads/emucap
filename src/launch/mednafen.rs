//! Mednafen (Saturn / PSX / PC Engine / PC-FX / Mega Drive / WonderSwan / Neo Geo Pocket) launch orchestration. One
//! built binary handles every system; the caller passes the force_module. We run a per-port *copy* of
//! the binary so that
//! rebuilding the shared work tree doesn't disturb a running instance (the copy is a separate inode).

use super::spec::{mednafen_spec, MednafenSpecOpts, SpecOpts};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

pub const PCFX_BIOS_SHA256: &str =
    "4b44ccf5d84cc83daa2e6a2bee00fdafa14eb58bdf5859e96d8861a891675417";
const PCFX_BIOS_SIZE: u64 = 1024 * 1024;
const SHARED_FIRMWARE_NAMES: &[&str] = &[
    "sega_101.bin",
    "mpr-17933.bin",
    "scph5500.bin",
    "scph5501.bin",
    "scph5502.bin",
    "syscard3.pce",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildMetadata {
    pub upstream: String,
    pub upstream_revision: String,
    pub patchset_sha256: String,
    pub binary_sha256: String,
}

fn metadata_path(binary: &Path) -> PathBuf {
    let mut value = binary.as_os_str().to_os_string();
    value.push(".emucap-build.json");
    PathBuf::from(value)
}

pub fn read_build_metadata(binary: &Path) -> std::io::Result<Option<BuildMetadata>> {
    let path = metadata_path(binary);
    let bytes = match crate::path_safety::read_bounded_regular_file_no_follow(&path, 256 * 1024) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata: BuildMetadata = serde_json::from_slice(&bytes).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid Mednafen build sidecar {}: {error}", path.display()),
        )
    })?;
    let digest =
        |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if metadata.upstream.is_empty()
        || metadata.upstream_revision.is_empty()
        || !digest(&metadata.patchset_sha256)
        || !digest(&metadata.binary_sha256)
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Mednafen build sidecar identity is incomplete",
        ));
    }
    let mut file = File::open(binary)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hex::encode(hasher.finalize());
    if !metadata.binary_sha256.eq_ignore_ascii_case(&actual) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "Mednafen build sidecar binary_sha256 does not match {}",
                binary.display()
            ),
        ));
    }
    Ok(Some(metadata))
}

fn default_pcfx_bios_source() -> PathBuf {
    super::emu_home_base().join("firmware/pcfx.rom")
}

/// Shared operator-supplied firmware inventory. Managed launches copy only the
/// canonical Mednafen filenames they understand into the port-owned profile.
pub fn default_firmware_root() -> PathBuf {
    super::emu_home_base().join("firmware")
}

fn resolve_firmware_root() -> std::io::Result<(PathBuf, bool)> {
    let explicit = std::env::var_os("EMUCAP_MEDNAFEN_FIRMWARE");
    let root = explicit
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_firmware_root);
    if explicit.is_some() && !root.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "EMUCAP_MEDNAFEN_FIRMWARE must be an absolute directory",
        ));
    }
    Ok((root, explicit.is_some()))
}

fn prepare_runtime_home(port: u16) -> std::io::Result<PathBuf> {
    let base = super::emu_home_base();
    let home = super::emu_home_dir("mednafen", port);
    if super::has_symlink_component_under(&base, &home) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "Mednafen runtime home is not private because it contains a symlink: {}",
                home.display()
            ),
        ));
    }
    std::fs::create_dir_all(&home)?;
    let firmware = home.join("firmware");
    if super::has_symlink_component_under(&base, &firmware) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "Mednafen runtime firmware directory is not private: {}",
                firmware.display()
            ),
        ));
    }
    std::fs::create_dir_all(&firmware)?;

    let (source, explicit) = resolve_firmware_root()?;
    if !source.exists() {
        if explicit {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!(
                    "EMUCAP_MEDNAFEN_FIRMWARE directory does not exist: {}",
                    source.display()
                ),
            ));
        }
        return Ok(home);
    }
    if !source.is_dir() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "Mednafen firmware inventory is not a directory: {}",
                source.display()
            ),
        ));
    }
    for name in SHARED_FIRMWARE_NAMES {
        let src = source.join(name);
        if src.is_file() {
            super::copy_file_replace(&src, &firmware.join(name))?;
        }
    }
    Ok(home)
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
    /// Halt before the first guest instruction and service control while halted.
    pub start_frozen: bool,
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

/// Launch Mednafen detached for emucap. Every module receives a port-owned Mednafen home. Known
/// firmware files are copied from the shared emucap inventory, and PC-FX additionally receives its
/// explicitly validated BIOS path. The user's Mednafen profile is never read or changed.
pub fn launch(l: &Launch) -> std::io::Result<u32> {
    let build_metadata = read_build_metadata(l.binary)?;
    let runtime_home = prepare_runtime_home(l.port)?;
    let run_binary = if l.explicit_binary {
        l.binary.to_path_buf()
    } else {
        let dst = run_binary_path(l.binary, &runtime_home);
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
    let mut spec = mednafen_spec(
        &run_binary,
        l.log_path,
        &runtime_home,
        &MednafenSpecOpts {
            module: l.module,
            sound: l.sound,
            pcfx_bios: pcfx_bios.as_deref(),
            start_frozen: l.start_frozen,
        },
        &opts,
    );
    if let Some(metadata) = build_metadata {
        spec = spec
            .env("EMUCAP_MEDNAFEN_BINARY_SHA256", metadata.binary_sha256)
            .env("EMUCAP_MEDNAFEN_UPSTREAM", metadata.upstream)
            .env(
                "EMUCAP_MEDNAFEN_UPSTREAM_REVISION",
                metadata.upstream_revision,
            )
            .env("EMUCAP_MEDNAFEN_PATCHSET_SHA256", metadata.patchset_sha256);
    }
    super::spawn_detached(&spec)
}

#[cfg(test)]
#[path = "mednafen_tests.rs"]
mod tests;
