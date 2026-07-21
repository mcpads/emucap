//! Isolated launcher for the debugger-enabled Mupen64Plus N64 adapter.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

use super::{emu_home_dir, find_on_path, is_runnable_file, spawn_detached, LaunchSpec, RuntimeEnv};

pub const REQUIRED_HOST_API: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildMetadata {
    pub upstream: String,
    pub version: String,
    pub core_commit: String,
    pub host_api: u32,
    pub bundle_sha256: String,
    pub test_rom_sha256: String,
    pub debugger: bool,
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub repo_root: &'a Path,
    pub root: &'a Path,
    pub content: &'a Path,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub build: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub pid: u32,
    pub runtime_home: PathBuf,
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "emucap-mupen64plus.exe"
    } else {
        "emucap-mupen64plus"
    }
}

pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    if !cfg!(unix) {
        return None;
    }
    if let Some(explicit) = std::env::var_os("EMUCAP_M64P_BIN") {
        let path = PathBuf::from(explicit);
        return is_runnable_file(&path).then_some(path);
    }
    let name = binary_name();
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(name));
        }
    }
    candidates.push(repo_root.join("target/release").join(name));
    candidates.push(repo_root.join("target/debug").join(name));
    candidates
        .into_iter()
        .find(|path| is_runnable_file(path))
        .or_else(|| find_on_path(name))
}

pub fn default_root(repo_root: &Path) -> PathBuf {
    let lock = std::fs::read_to_string(repo_root.join("adapters/mupen64plus/upstream.lock"))
        .unwrap_or_default();
    let version = lock_value(&lock, "M64P_VERSION").unwrap_or_else(|| "invalid-lock".into());
    repo_root.join(format!(
        "adapters/mupen64plus/work/mupen64plus-bundle-src-{version}/test"
    ))
}

pub fn resolve_root(repo_root: &Path, display: bool) -> Option<PathBuf> {
    let root = std::env::var_os("EMUCAP_M64P_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_root(repo_root));
    require_compatible_root(repo_root, &root, display)
        .ok()
        .map(|_| root)
}

fn lock_value(lock: &str, key: &str) -> Option<String> {
    lock.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

fn required_lock_value(lock: &str, key: &str) -> io::Result<String> {
    lock_value(lock, key).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{key} is missing from adapters/mupen64plus/upstream.lock"),
        )
    })
}

fn library_exists(root: &Path, stem: &str) -> bool {
    [".dylib", ".so", ".so.2"]
        .into_iter()
        .any(|suffix| root.join(format!("{stem}{suffix}")).is_file())
}

pub fn require_compatible_root(
    repo_root: &Path,
    root: &Path,
    display: bool,
) -> io::Result<BuildMetadata> {
    if !library_exists(root, "libmupen64plus") || !library_exists(root, "mupen64plus-rsp-hle") {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Mupen64Plus core or RSP plugin is missing under {}",
                root.display()
            ),
        ));
    }
    if display && !library_exists(root, "mupen64plus-video-rice") {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Mupen64Plus Rice video plugin is missing under {}",
                root.display()
            ),
        ));
    }

    let metadata_path = root.join("emucap-mupen64plus-build.json");
    let metadata: BuildMetadata =
        serde_json::from_slice(&std::fs::read(&metadata_path)?).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid Mupen64Plus build metadata at {}: {error}",
                    metadata_path.display()
                ),
            )
        })?;
    let lock = std::fs::read_to_string(repo_root.join("adapters/mupen64plus/upstream.lock"))?;
    let expected_api = required_lock_value(&lock, "M64P_HOST_API")?
        .parse::<u32>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "M64P_HOST_API is invalid"))?;
    let matches_lock = metadata.upstream == required_lock_value(&lock, "M64P_BUNDLE_URL")?
        && metadata.version == required_lock_value(&lock, "M64P_VERSION")?
        && metadata.core_commit == required_lock_value(&lock, "M64P_CORE_COMMIT")?
        && metadata.host_api == expected_api
        && metadata.host_api == REQUIRED_HOST_API
        && metadata.bundle_sha256 == required_lock_value(&lock, "M64P_BUNDLE_SHA256")?
        && metadata.test_rom_sha256 == required_lock_value(&lock, "M64P_TEST_ROM_SHA256")?
        && metadata.debugger;
    if !matches_lock {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Mupen64Plus build metadata does not match {}",
                repo_root
                    .join("adapters/mupen64plus/upstream.lock")
                    .display()
            ),
        ));
    }
    Ok(metadata)
}

pub fn validate_content(content: &Path) -> io::Result<()> {
    if !content.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("N64 ROM not found: {}", content.display()),
        ));
    }
    let supported = content
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "z64" | "n64" | "v64"));
    if !supported {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "N64 content must use a .z64, .n64, or .v64 cartridge extension",
        ));
    }
    Ok(())
}

pub fn launch_spec(launch: &Launch<'_>, runtime_home: &Path) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.binary, launch.log_path)
        .arg(launch.port.to_string())
        .arg(launch.content.to_string_lossy().into_owned())
        .arg(launch.root.to_string_lossy().into_owned())
        .arg(runtime_home.to_string_lossy().into_owned())
        .env("EMUCAP_N64_DISPLAY", if launch.display { "1" } else { "0" })
        .env(
            "EMUCAP_CONTENT",
            launch.content.to_string_lossy().into_owned(),
        );
    if let Some(name) = launch.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = launch.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    if let Some(build) = launch.build {
        spec = spec.env("EMUCAP_BUILD_HASH", build);
    }
    spec.runtime_env(launch.runtime)
}

pub fn launch(launch: &Launch<'_>) -> io::Result<Launched> {
    validate_content(launch.content)?;
    require_compatible_root(launch.repo_root, launch.root, launch.display)?;
    let runtime_home = emu_home_dir("mupen64plus", launch.port);
    std::fs::create_dir_all(&runtime_home)?;
    let pid = spawn_detached(&launch_spec(launch, &runtime_home))?;
    if launch.display {
        super::spawn_display_caffeinate(pid);
    }
    Ok(Launched { pid, runtime_home })
}

#[cfg(test)]
#[path = "mupen64plus_tests.rs"]
mod tests;
