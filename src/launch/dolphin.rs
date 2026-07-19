//! Dolphin native-fork launch preparation for GameCube and Wii.
//!
//! Only builds carrying the sidecar produced by `adapters/dolphin/build.sh` or `build.ps1` are
//! accepted. Each launch runs from an emucap-owned portable copy and uses a per-port `--user`
//! directory, leaving an installed Dolphin and its configuration untouched.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::spec::{dolphin_spec, SpecOpts};

pub const REQUIRED_HOST_API: u32 = 1;

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
    let raw = std::fs::read_to_string(&path).map_err(|error| {
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
            source.join("build-emucap-headless/Binaries/Dolphin.exe"),
            source.join("Binary/x64/Dolphin.exe"),
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
        let _ = display;
        for key in [
            "LOCALAPPDATA",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "USERPROFILE",
        ] {
            if let Some(base) = std::env::var_os(key).map(PathBuf::from) {
                candidates.push(base.join("Dolphin-x64/Dolphin.exe"));
                candidates.push(base.join("Programs/Dolphin/Dolphin.exe"));
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
        "Dolphin.exe"
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

pub fn prepare_runtime_binary(source_binary: &Path, port: u16) -> std::io::Result<PreparedRuntime> {
    let home = super::emu_home_dir("dolphin", port);
    let runtime_dir = home.join("runtime");
    let user_dir = home.join("user");
    std::fs::create_dir_all(&user_dir)?;

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
