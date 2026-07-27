//! Isolated launcher for the stock openMSX XML-control adapter.
//!
//! The emulator remains unmodified. A separate Rust bridge owns openMSX's
//! XML stdio channel and relays the supported control surface to emucap.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{
    emu_home_dir, find_on_path, is_runnable_file, process_alive, spawn_detached,
    terminate_detached, LaunchSpec, RuntimeEnv,
};

pub const REQUIRED_HOST_API: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildMetadata {
    pub upstream: String,
    pub version: String,
    pub host_api: u32,
    pub archive_sha256: String,
    pub sdl2_compat_patch_sha256: String,
    pub native_patch: bool,
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub bridge: &'a Path,
    pub repo_root: &'a Path,
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
    pub openmsx_pid: u32,
    pub bridge_pid: u32,
    pub runtime_home: PathBuf,
}

fn bridge_binary_name() -> &'static str {
    if cfg!(windows) {
        "emucap-openmsx-bridge.exe"
    } else {
        "emucap-openmsx-bridge"
    }
}

fn lock_value(lock: &str, key: &str) -> Option<String> {
    lock.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

fn required_lock_value(lock: &str, key: &str) -> io::Result<String> {
    lock_value(lock, key).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{key} is missing from adapters/openmsx/upstream.lock"),
        )
    })
}

fn collect_openmsx_binaries(root: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_openmsx_binaries(&path, output);
            continue;
        }
        let name_matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                if cfg!(windows) {
                    name.eq_ignore_ascii_case("openmsx.exe")
                } else {
                    name == "openmsx"
                }
            });
        let has_sidecar = path
            .parent()
            .is_some_and(|parent| parent.join("emucap-openmsx-build.json").is_file());
        if name_matches && has_sidecar && is_runnable_file(&path) {
            output.push(path);
        }
    }
}

/// Resolve only the pinned repo build (or an explicit override). Arbitrary host
/// installations are not accepted because their XML/screenshot behavior is not
/// covered by this adapter's runtime proof.
pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_OPENMSX_BIN") {
        let path = PathBuf::from(explicit);
        return is_runnable_file(&path).then_some(path);
    }
    let lock = std::fs::read_to_string(repo_root.join("adapters/openmsx/upstream.lock")).ok()?;
    let version = lock_value(&lock, "OPENMSX_VERSION")?;
    let derived = repo_root.join(format!("adapters/openmsx/work/openmsx-{version}/derived"));
    let mut candidates = Vec::new();
    collect_openmsx_binaries(&derived, &mut candidates);
    candidates.sort();
    candidates.into_iter().next()
}

pub fn resolve_bridge(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_OPENMSX_BRIDGE_BIN") {
        let path = PathBuf::from(explicit);
        return is_runnable_file(&path).then_some(path);
    }
    let name = bridge_binary_name();
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(name));
        }
    }
    candidates.push(repo_root.join("target/release").join(name));
    candidates.push(repo_root.join("target/debug").join(name));
    if let Some(on_path) = find_on_path(name) {
        candidates.push(on_path);
    }
    candidates.into_iter().find(|path| is_runnable_file(path))
}

pub fn require_compatible_build(repo_root: &Path, binary: &Path) -> io::Result<BuildMetadata> {
    if !is_runnable_file(binary) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("openMSX executable is not runnable: {}", binary.display()),
        ));
    }
    let metadata_path = binary
        .parent()
        .ok_or_else(|| io::Error::other("openMSX binary has no parent directory"))?
        .join("emucap-openmsx-build.json");
    let metadata: BuildMetadata =
        serde_json::from_slice(&std::fs::read(&metadata_path)?).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid openMSX build metadata at {}: {error}",
                    metadata_path.display()
                ),
            )
        })?;
    let lock = std::fs::read_to_string(repo_root.join("adapters/openmsx/upstream.lock"))?;
    let expected_api = required_lock_value(&lock, "OPENMSX_HOST_API")?
        .parse::<u32>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "OPENMSX_HOST_API is invalid"))?;
    let matches_lock = metadata.upstream == required_lock_value(&lock, "OPENMSX_URL")?
        && metadata.version == required_lock_value(&lock, "OPENMSX_VERSION")?
        && metadata.host_api == expected_api
        && metadata.host_api == REQUIRED_HOST_API
        && metadata.archive_sha256 == required_lock_value(&lock, "OPENMSX_SHA256")?
        && metadata.sdl2_compat_patch_sha256
            == required_lock_value(&lock, "OPENMSX_SDL2_COMPAT_PATCH_SHA256")?
        && !metadata.native_patch;
    if !matches_lock {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "openMSX build metadata does not match {}",
                repo_root.join("adapters/openmsx/upstream.lock").display()
            ),
        ));
    }
    Ok(metadata)
}

pub fn validate_content(content: &Path) -> io::Result<()> {
    if !content.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("MSX cartridge not found: {}", content.display()),
        ));
    }
    let supported = content
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "rom" | "mx1" | "mx2" | "ri" | "sg"
            )
        });
    if !supported {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the first MSX profile accepts only cartridge files: .rom, .mx1, .mx2, .ri, or .sg",
        ));
    }
    Ok(())
}

pub fn launch_spec(launch: &Launch<'_>, runtime_home: &Path, pid_file: &Path) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.bridge, launch.log_path)
        .arg(launch.port.to_string())
        .arg(launch.binary.to_string_lossy().into_owned())
        .arg(launch.content.to_string_lossy().into_owned())
        .arg(runtime_home.to_string_lossy().into_owned())
        .arg(if launch.display { "1" } else { "0" })
        .arg(pid_file.to_string_lossy().into_owned())
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

fn wait_for_child_pid(bridge_pid: u32, pid_file: &Path, timeout: Duration) -> io::Result<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(bridge_pid) {
            return Err(io::Error::other(
                "openMSX bridge exited before publishing the emulator PID",
            ));
        }
        match std::fs::read_to_string(pid_file) {
            Ok(raw) => {
                let pid = raw.trim().parse::<u32>().ok().filter(|pid| *pid != 0);
                if let Some(pid) = pid {
                    if process_alive(pid) {
                        return Ok(pid);
                    }
                    return Err(io::Error::other(
                        "openMSX exited before launch ownership was captured",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "openMSX bridge did not publish the emulator PID within 10 seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn launch(launch: &Launch<'_>) -> io::Result<Launched> {
    validate_content(launch.content)?;
    require_compatible_build(launch.repo_root, launch.binary)?;
    let runtime_home = emu_home_dir("openmsx", launch.port);
    std::fs::create_dir_all(&runtime_home)?;
    let pid_file = runtime_home.join("emulator.pid");
    if pid_file.exists() {
        std::fs::remove_file(&pid_file)?;
    }
    if launch.display {
        super::wake_display_before_gui_launch();
    }
    let bridge_pid = spawn_detached(&launch_spec(launch, &runtime_home, &pid_file))?;
    let openmsx_pid = match wait_for_child_pid(bridge_pid, &pid_file, Duration::from_secs(10)) {
        Ok(pid) => pid,
        Err(error) => {
            let _ = terminate_detached(bridge_pid);
            return Err(error);
        }
    };
    if launch.display {
        super::spawn_display_caffeinate(openmsx_pid);
    }
    Ok(Launched {
        openmsx_pid,
        bridge_pid,
        runtime_home,
    })
}

#[cfg(test)]
#[path = "openmsx_tests.rs"]
mod tests;
