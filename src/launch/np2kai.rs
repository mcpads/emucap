//! Direct NP2kai libretro host for the PC-98 HDI compatibility backend.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{emu_home_dir, find_on_path, is_runnable_file, spawn_detached, LaunchSpec, RuntimeEnv};

const LOCK: &str = include_str!("../../adapters/np2kai/upstream.lock");

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct BuildIdentity {
    pub upstream: String,
    pub commit: String,
    pub archive_sha256: String,
    pub patchset_sha256: String,
    pub build_profile: String,
    pub compiled_defines_sha256: String,
    pub compiled_sources_sha256: String,
    pub license_manifest_sha256: String,
    pub core_sha256: String,
    pub required_defines: Vec<String>,
    pub excluded_components: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CompatibleCore {
    pub path: PathBuf,
    pub identity: BuildIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HdiGeometry {
    pub header_size: u32,
    pub disk_size: u32,
    pub sector_size: u32,
    pub sectors: u32,
    pub heads: u32,
    pub cylinders: u32,
}

pub fn inspect_hdi_geometry(path: &Path) -> std::io::Result<Option<HdiGeometry>> {
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("hdi"))
    {
        return Ok(None);
    }
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 32];
    if file.read_exact(&mut header).is_err() {
        return Ok(None);
    }
    let word = |offset: usize| {
        u32::from_le_bytes(
            header[offset..offset + 4]
                .try_into()
                .expect("four-byte HDI field"),
        )
    };
    let geometry = HdiGeometry {
        header_size: word(8),
        disk_size: word(12),
        sector_size: word(16),
        sectors: word(20),
        heads: word(24),
        cylinders: word(28),
    };
    let described = u64::from(geometry.sector_size)
        .checked_mul(u64::from(geometry.sectors))
        .and_then(|value| value.checked_mul(u64::from(geometry.heads)))
        .and_then(|value| value.checked_mul(u64::from(geometry.cylinders)));
    let file_size = file.metadata()?.len();
    if geometry.header_size < 32
        || described != Some(u64::from(geometry.disk_size))
        || u64::from(geometry.header_size).checked_add(u64::from(geometry.disk_size))
            != Some(file_size)
    {
        return Ok(None);
    }
    Ok(Some(geometry))
}

pub fn default_core_path(repo_root: &Path) -> PathBuf {
    let name = if cfg!(target_os = "macos") {
        "np2kai_libretro.dylib"
    } else {
        "np2kai_libretro.so"
    };
    repo_root.join("adapters/np2kai/work/np2kai/sdl").join(name)
}

pub fn default_build_info_path(repo_root: &Path) -> PathBuf {
    repo_root.join("adapters/np2kai/work/np2kai/emucap-np2kai-build.json")
}

pub fn default_firmware_root() -> PathBuf {
    if let Some(path) = std::env::var_os("EMUCAP_NP2KAI_FIRMWARE") {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("workspace/retrobios/bios/NEC/PC-98");
    }
    PathBuf::from("retrobios/bios/NEC/PC-98")
}

pub fn resolve_frontend(repo_root: &Path) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("EMUCAP_NP2KAI_BIN").map(PathBuf::from) {
        return is_runnable_file(&path).then_some(path);
    }
    let name = if cfg!(windows) {
        "emucap-np2kai.exe"
    } else {
        "emucap-np2kai"
    };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    candidates.push(repo_root.join("target/release").join(name));
    candidates.push(repo_root.join("target/debug").join(name));
    if let Some(path) = find_on_path(name) {
        candidates.push(path);
    }
    candidates.into_iter().find(|path| is_runnable_file(path))
}

pub fn require_compatible_core(repo_root: &Path) -> std::io::Result<CompatibleCore> {
    let core = std::env::var_os("EMUCAP_NP2KAI_CORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_core_path(repo_root));
    if !core.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("NP2kai core not found: {}", core.display()),
        ));
    }
    let info = std::env::var_os("EMUCAP_NP2KAI_BUILD_INFO")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_build_info_path(repo_root));
    let identity: BuildIdentity = serde_json::from_slice(&fs::read(&info)?).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid NP2kai build identity {}: {error}", info.display()),
        )
    })?;
    for (key, actual) in [
        ("NP2KAI_UPSTREAM", identity.upstream.as_str()),
        ("NP2KAI_COMMIT", identity.commit.as_str()),
        ("NP2KAI_ARCHIVE_SHA256", identity.archive_sha256.as_str()),
        ("NP2KAI_PATCHSET_SHA256", identity.patchset_sha256.as_str()),
        ("NP2KAI_BUILD_PROFILE", identity.build_profile.as_str()),
    ] {
        let expected = lock_value(key).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("missing {key} in NP2kai upstream.lock"),
            )
        })?;
        if actual != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("NP2kai build identity {key} mismatch: expected {expected}, got {actual}"),
            ));
        }
    }
    validate_build_identity(&identity)?;
    let core_sha256 = sha256_file(&core)?;
    if core_sha256 != identity.core_sha256 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "NP2kai core digest mismatch: expected {}, got {core_sha256}",
                identity.core_sha256
            ),
        ));
    }
    Ok(CompatibleCore {
        path: core,
        identity,
    })
}

fn validate_build_identity(identity: &BuildIdentity) -> std::io::Result<()> {
    for (name, value) in [
        ("archive_sha256", identity.archive_sha256.as_str()),
        ("patchset_sha256", identity.patchset_sha256.as_str()),
        (
            "compiled_defines_sha256",
            identity.compiled_defines_sha256.as_str(),
        ),
        (
            "compiled_sources_sha256",
            identity.compiled_sources_sha256.as_str(),
        ),
        (
            "license_manifest_sha256",
            identity.license_manifest_sha256.as_str(),
        ),
        ("core_sha256", identity.core_sha256.as_str()),
    ] {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid NP2kai {name}"),
            ));
        }
    }
    let exact = |actual: &[String], expected: &[&str]| {
        actual.len() == expected.len()
            && expected
                .iter()
                .all(|entry| actual.iter().any(|actual| actual == entry))
    };
    if !exact(
        &identity.required_defines,
        &[
            "USE_MAME_BSD",
            "SUPPORT_FPU_SOFTFLOAT3",
            "SUPPORT_EMUCAP_DEBUG",
        ],
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "NP2kai required-define attestation does not match the supported profile",
        ));
    }
    if !exact(
        &identity.excluded_components,
        &[
            "fmgen",
            "mame-gpl-sound",
            "dosbox-fpu",
            "softfloat-legacy",
            "trident-tgui",
        ],
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "NP2kai excluded-component attestation does not match the supported profile",
        ));
    }
    Ok(())
}

pub struct Launch<'a> {
    pub repo_root: &'a Path,
    pub content: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    pub start_frozen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub pid: u32,
    pub frontend: PathBuf,
    pub core: PathBuf,
    pub firmware: PathBuf,
    pub runtime_home: PathBuf,
    pub mounted_content: PathBuf,
    pub source_content_sha256: String,
    pub identity: BuildIdentity,
}

pub fn launch(request: &Launch<'_>) -> std::io::Result<Launched> {
    let frontend = resolve_frontend(request.repo_root).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "emucap-np2kai frontend not found; build the release binaries",
        )
    })?;
    let bundle = require_compatible_core(request.repo_root)?;
    let firmware = default_firmware_root();
    if !firmware.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "NP2kai firmware directory not found: {}",
                firmware.display()
            ),
        ));
    }
    let runtime_home = emu_home_dir("np2kai", request.port);
    fs::create_dir_all(&runtime_home)?;
    let (mounted_content, source_content_sha256) =
        prepare_working_copy(request.content, &runtime_home)?;
    let log = runtime_home.join("np2kai.log");
    let mut spec = LaunchSpec::new(&frontend, log)
        .arg(request.port.to_string())
        .arg(mounted_content.to_string_lossy())
        .arg(bundle.path.to_string_lossy())
        .arg(firmware.to_string_lossy())
        .arg(runtime_home.to_string_lossy())
        .arg(&bundle.identity.commit)
        .arg(&bundle.identity.patchset_sha256)
        .arg(&bundle.identity.build_profile)
        .env(
            "EMUCAP_START_FROZEN",
            if request.start_frozen { "1" } else { "0" },
        )
        .runtime_env(request.runtime);
    if let Some(name) = request.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = request.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    let pid = spawn_detached(&spec)?;
    Ok(Launched {
        pid,
        frontend,
        core: bundle.path,
        firmware,
        runtime_home,
        mounted_content,
        source_content_sha256,
        identity: bundle.identity,
    })
}

pub fn accepts_content_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("hdi"))
}

fn prepare_working_copy(source: &Path, runtime_home: &Path) -> std::io::Result<(PathBuf, String)> {
    if !accepts_content_path(source) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the NP2kai backend accepts .hdi hard-disk images only",
        ));
    }
    if !source.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("PC-98 HDI not found: {}", source.display()),
        ));
    }
    let source_before = sha256_file(source)?;
    let media_dir = runtime_home.join("media");
    fs::create_dir_all(&media_dir)?;
    let media_metadata = fs::symlink_metadata(&media_dir)?;
    if media_metadata.file_type().is_symlink() || !media_metadata.is_dir() {
        return Err(std::io::Error::other(format!(
            "managed NP2kai media directory is not a plain directory: {}",
            media_dir.display()
        )));
    }
    let destination = media_dir.join("content.hdi");
    let partial = media_dir.join(format!(".content.hdi.partial-{}", std::process::id()));
    let replace_destination = match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            return Err(std::io::Error::other(format!(
                "managed NP2kai media path is not a plain regular file: {}",
                destination.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let mut partial_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)?;
    let result = (|| -> std::io::Result<()> {
        let mut source_file = fs::File::open(source)?;
        std::io::copy(&mut source_file, &mut partial_file)?;
        partial_file.flush()?;
        partial_file.sync_all()?;
        drop(partial_file);
        let source_after = sha256_file(source)?;
        let copied = sha256_file(&partial)?;
        if source_before != source_after || copied != source_before {
            return Err(std::io::Error::other(
                "PC-98 HDI changed while creating the managed working copy",
            ));
        }
        if replace_destination {
            fs::remove_file(&destination)?;
        }
        fs::rename(&partial, &destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result?;
    Ok((destination, source_before))
}

fn lock_value(key: &str) -> Option<&str> {
    LOCK.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
#[path = "np2kai_tests.rs"]
mod tests;
