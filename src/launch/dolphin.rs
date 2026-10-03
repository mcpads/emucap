//! Dolphin native-fork launch preparation for GameCube and Wii.
//!
//! Only builds carrying the sidecar produced by `adapters/dolphin/build.sh` or `build.ps1` are
//! accepted. Each launch runs from an emucap-owned portable copy and uses a per-port `--user`
//! directory, leaving an installed Dolphin and its configuration untouched.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::spec::{dolphin_spec, SpecOpts};

pub const REQUIRED_HOST_API: u32 = 8;

/// Admit native output evidence, or wait for an in-flight native transition. Stream evidence
/// describes actual native call results; it does not promise physical device audibility.
pub fn audio_output_ready(status: &serde_json::Value, sound: bool) -> Result<bool, String> {
    #[derive(Deserialize)]
    struct Observation {
        stream_generation: String,
        backend: Option<String>,
        initialized: bool,
        phase: String,
        last_run_result: String,
        start_verified: bool,
        failure: Option<String>,
    }
    let malformed = || "Dolphin did not report valid native audio output evidence".to_string();
    let value = status.get("audio_output").ok_or_else(malformed)?;
    if value.get("failure").is_none() || value.get("backend").is_none() {
        return Err(malformed());
    }
    let observed: Observation = serde_json::from_value(value.clone()).map_err(|_| malformed())?;
    if observed
        .stream_generation
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .is_none()
    {
        return Err(malformed());
    }
    if let Some(failure) = observed.failure {
        return Err(format!("Dolphin audio output unavailable: {failure}"));
    }
    if matches!(
        observed.phase.as_str(),
        "initializing" | "starting" | "stopping"
    ) {
        return Ok(false);
    }
    if observed.phase != "ready" || !observed.initialized {
        return Err("Dolphin audio output is not initialized".into());
    }
    let backend = observed
        .backend
        .filter(|name| !name.is_empty())
        .ok_or_else(malformed)?;
    if (backend != "No Audio Output") != sound {
        return Err(format!(
            "Dolphin audio backend {backend:?} does not satisfy sound:{sound}"
        ));
    }
    if !sound {
        return Ok(true);
    }
    if !observed.start_verified {
        return Err("Dolphin audio output has no successful native start".into());
    }
    match observed.last_run_result.as_str() {
        "started" => Ok(true),
        "stopped" => match status.get("state").and_then(serde_json::Value::as_str) {
            Some("frozen") => Ok(true),
            Some("running") => Ok(false),
            _ => Err(malformed()),
        },
        _ => Err("Dolphin audio output has no settled successful run result".into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildMetadata {
    pub upstream: String,
    pub commit: String,
    pub host_api: u32,
    pub patchset_sha256: String,
}

fn patch_required(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("dolphin-patch-required: {}", message.into()),
    )
}

pub fn build_metadata_path(binary: &Path) -> PathBuf {
    binary
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("emucap-dolphin-build.json")
}

pub fn read_build_metadata(binary: &Path) -> std::io::Result<BuildMetadata> {
    let path = build_metadata_path(binary);
    let raw = crate::path_safety::read_bounded_utf8_regular_file_no_follow(&path, 256 * 1024)
        .map_err(|error| {
        patch_required(format!(
            "compatible build metadata is missing at {} ({error}); run adapters/dolphin/build.sh or build.ps1",
            path.display()
        ))
    })?;
    let metadata: BuildMetadata = serde_json::from_str(&raw).map_err(|error| {
        patch_required(format!(
            "invalid build metadata at {}: {error}",
            path.display()
        ))
    })?;
    if metadata.host_api != REQUIRED_HOST_API {
        return Err(patch_required(format!(
            "host API {} is incompatible; expected {}",
            metadata.host_api, REQUIRED_HOST_API
        )));
    }
    if metadata.commit.len() != 40 || !metadata.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(patch_required(
            "metadata commit is not a full git object id",
        ));
    }
    if metadata.patchset_sha256.len() != 64
        || !metadata
            .patchset_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(patch_required("metadata patchset_sha256 is invalid"));
    }
    Ok(metadata)
}

fn lock_value(lock: &str, key: &str) -> Option<String> {
    lock.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

pub fn require_compatible_build(root: &Path, binary: &Path) -> std::io::Result<BuildMetadata> {
    let metadata = read_build_metadata(binary)?;
    let lock_path = root.join("adapters/dolphin/upstream.lock");
    let lock = std::fs::read_to_string(&lock_path)?;
    let expected_repo = lock_value(&lock, "DOLPHIN_REPO")
        .ok_or_else(|| patch_required("DOLPHIN_REPO missing from upstream.lock"))?;
    let expected_commit = lock_value(&lock, "DOLPHIN_COMMIT")
        .ok_or_else(|| patch_required("DOLPHIN_COMMIT missing from upstream.lock"))?;
    let expected_api = lock_value(&lock, "DOLPHIN_HOST_API")
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| patch_required("DOLPHIN_HOST_API invalid in upstream.lock"))?;
    let expected_patchset = lock_value(&lock, "DOLPHIN_PATCHSET_SHA256")
        .ok_or_else(|| patch_required("DOLPHIN_PATCHSET_SHA256 missing from upstream.lock"))?;
    if metadata.upstream != expected_repo
        || metadata.commit != expected_commit
        || metadata.host_api != expected_api
        || metadata.patchset_sha256 != expected_patchset
    {
        return Err(patch_required(format!(
            "build sidecar does not match {}",
            lock_path.display()
        )));
    }
    Ok(metadata)
}

fn app_bundle_executable(path: &Path) -> Option<PathBuf> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("app"))
    {
        return None;
    }
    ["DolphinQt", "Dolphin"]
        .into_iter()
        .map(|name| path.join("Contents/MacOS").join(name))
        .find(|candidate| super::is_runnable_file(candidate))
}

pub fn local_build_candidates(root: &Path, display: bool) -> Vec<PathBuf> {
    let source = root.join("adapters/dolphin/work/dolphin-src");
    if display {
        vec![
            source.join("build-emucap-gui/Binaries/DolphinQt.app/Contents/MacOS/DolphinQt"),
            source.join("build-emucap-gui/Binaries/DolphinQt"),
            source.join("Binary/x64/Dolphin.exe"),
        ]
    } else {
        vec![
            source.join("build-emucap-headless/Binaries/dolphin-emu-nogui"),
            source.join("build-emucap-headless/Binaries/DolphinNoGUI.exe"),
            source.join("Binary/x64/DolphinNoGUI.exe"),
        ]
    }
}

pub fn default_install_candidates(display: bool) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    if display {
        candidates.push(PathBuf::from(
            "/Applications/Dolphin.app/Contents/MacOS/Dolphin",
        ));
    }
    #[cfg(windows)]
    {
        let name = if display {
            "Dolphin.exe"
        } else {
            "DolphinNoGUI.exe"
        };
        for key in [
            "LOCALAPPDATA",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "USERPROFILE",
        ] {
            if let Some(base) = std::env::var_os(key).map(PathBuf::from) {
                candidates.push(base.join("Dolphin-x64").join(name));
                candidates.push(base.join("Programs/Dolphin").join(name));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let name = if display {
            "dolphin-emu"
        } else {
            "dolphin-emu-nogui"
        };
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            candidates.push(home.join(".local/bin").join(name));
        }
    }
    candidates
}

pub fn resolve_binary(root: &Path, display: bool) -> Option<PathBuf> {
    let mode_key = if display {
        "EMUCAP_DOLPHIN_GUI_BIN"
    } else {
        "EMUCAP_DOLPHIN_HEADLESS_BIN"
    };
    for key in [mode_key, "EMUCAP_DOLPHIN_BIN"] {
        if let Some(explicit) = std::env::var_os(key) {
            let path = PathBuf::from(explicit);
            if super::is_runnable_file(&path) {
                return Some(path);
            }
            if let Some(binary) = app_bundle_executable(&path) {
                return Some(binary);
            }
        }
    }
    if let Some(local) = super::first_existing_file(local_build_candidates(root, display)) {
        return Some(local);
    }
    if let Some(default) = super::first_existing_file(default_install_candidates(display)) {
        return Some(default);
    }
    let executable = if cfg!(windows) {
        if display {
            "Dolphin.exe"
        } else {
            "DolphinNoGUI.exe"
        }
    } else if display {
        "dolphin-emu"
    } else {
        "dolphin-emu-nogui"
    };
    super::find_on_path(executable)
}

fn app_bundle_root(binary: &Path) -> Option<(&Path, PathBuf)> {
    for ancestor in binary.ancestors() {
        if ancestor
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        {
            let relative = binary.strip_prefix(ancestor).ok()?.to_path_buf();
            return Some((ancestor, relative));
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRuntime {
    pub binary: PathBuf,
    pub home: PathBuf,
    pub user_dir: PathBuf,
}

fn prepare_owned_directories(
    home: &Path,
    runtime_dir: &Path,
    user_dir: &Path,
) -> std::io::Result<()> {
    let base = super::emu_home_base();
    let reject_symlinks = || {
        for path in [home, runtime_dir, user_dir] {
            if super::has_symlink_component_under(&base, path) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "portable Dolphin directory contains a symlink, refusing to launch: {}",
                        path.display()
                    ),
                ));
            }
        }
        Ok(())
    };

    // Dolphin writes configuration, saves, cache data, and GUI state below --user. Check both
    // before and after creation so an existing redirect cannot escape the emucap-owned tree.
    reject_symlinks()?;
    std::fs::create_dir_all(runtime_dir)?;
    std::fs::create_dir_all(user_dir)?;
    reject_symlinks()
}

pub fn prepare_runtime_binary(source_binary: &Path, port: u16) -> std::io::Result<PreparedRuntime> {
    let home = super::emu_home_dir("dolphin", port);
    let runtime_dir = home.join("runtime");
    let user_dir = home.join("user");
    prepare_owned_directories(&home, &runtime_dir, &user_dir)?;

    if source_binary.starts_with(&home) {
        if super::has_symlink_component_under(&home, source_binary) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "portable Dolphin binary path contains a symlink, refusing to launch: {}",
                    source_binary.display()
                ),
            ));
        }
        return Ok(PreparedRuntime {
            binary: source_binary.to_path_buf(),
            home,
            user_dir,
        });
    }

    let binary = if let Some((app_root, relative)) = app_bundle_root(source_binary) {
        let app_name = app_root.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Dolphin app path: {}", app_root.display()),
            )
        })?;
        let destination = runtime_dir.join(app_name);
        super::copy_dir_replace(app_root, &destination)?;
        destination.join(relative)
    } else {
        let executable_name = source_binary.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Dolphin binary path: {}", source_binary.display()),
            )
        })?;
        let destination = runtime_dir.join(executable_name);
        super::copy_file_replace(source_binary, &destination)?;
        super::copy_adjacent_dlls(source_binary, &runtime_dir)?;
        if source_binary
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            let source_dir = source_binary.parent().unwrap();
            for name in ["Sys", "QtPlugins"] {
                let source = source_dir.join(name);
                if source.is_dir() {
                    super::copy_dir_replace(&source, &runtime_dir.join(name))?;
                }
            }
            let qt_config = source_dir.join("qt.conf");
            if qt_config.is_file() {
                super::copy_file_replace(&qt_config, &runtime_dir.join("qt.conf"))?;
            }
        }
        let sidecar = build_metadata_path(source_binary);
        if sidecar.is_file() {
            super::copy_file_replace(&sidecar, &runtime_dir.join("emucap-dolphin-build.json"))?;
        }
        destination
    };

    Ok(PreparedRuntime {
        binary,
        home,
        user_dir,
    })
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub content: &'a str,
    pub system: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
    pub display: bool,
    pub sound: bool,
}

pub fn launch(launch: &Launch) -> std::io::Result<u32> {
    let host_build = read_build_metadata(launch.binary)?;
    let portable = prepare_runtime_binary(launch.binary, launch.port)?;
    let opts = SpecOpts {
        content: launch.content,
        port: launch.port,
        name: launch.name,
        session_token: launch.session_token,
        runtime: launch.runtime,
        headless: !launch.display,
    };
    let spec = dolphin_spec(
        &portable.binary,
        launch.log_path,
        &portable.user_dir,
        launch.system,
        launch.sound,
        &opts,
    )
    .env("EMUCAP_DOLPHIN_UPSTREAM_COMMIT", &host_build.commit)
    .env(
        "EMUCAP_DOLPHIN_PATCHSET_SHA256",
        &host_build.patchset_sha256,
    );
    let pid = super::spawn_detached(&spec)?;
    if launch.display {
        super::spawn_display_caffeinate(pid);
    }
    Ok(pid)
}

#[cfg(test)]
#[path = "dolphin_tests.rs"]
mod tests;
