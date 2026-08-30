use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

use super::files::identity_for_regular_file;
use crate::launch::{find_on_path, is_runnable_file};

const LOCK: &str = include_str!("../../../adapters/xemu/upstream.lock");
pub const REQUIRED_HOST_API: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildMetadata {
    pub upstream: String,
    pub tag: String,
    pub commit: String,
    pub host_api: u32,
    pub patchset_sha256: String,
    pub binary_sha256: String,
}

fn patch_required(message: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("xemu-patch-required: {}", message.into()),
    )
}

pub(super) fn lock_value(key: &str) -> Option<&str> {
    LOCK.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn local_build_candidates(root: &Path) -> Vec<PathBuf> {
    let dist = root.join("adapters/xemu/work/xemu/dist");
    vec![
        dist.join("xemu.app/Contents/MacOS/xemu"),
        dist.join("xemu"),
        dist.join("xemu.exe"),
    ]
}

pub fn resolve_binary(root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_XEMU_BIN") {
        let path = PathBuf::from(explicit);
        return is_runnable_file(&path).then_some(path);
    }
    local_build_candidates(root)
        .into_iter()
        .find(|path| is_runnable_file(path))
        .or_else(|| find_on_path(if cfg!(windows) { "xemu.exe" } else { "xemu" }))
}

fn bridge_name() -> &'static str {
    if cfg!(windows) {
        "emucap-xemu-bridge.exe"
    } else {
        "emucap-xemu-bridge"
    }
}

pub fn resolve_bridge(root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_XEMU_BRIDGE_BIN") {
        let path = PathBuf::from(explicit);
        return is_runnable_file(&path).then_some(path);
    }
    let name = bridge_name();
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(name));
        }
    }
    candidates.push(root.join("target/release").join(name));
    candidates.push(root.join("target/debug").join(name));
    if let Some(path) = find_on_path(name) {
        candidates.push(path);
    }
    candidates.into_iter().find(|path| is_runnable_file(path))
}

pub fn build_metadata_path(binary: &Path) -> PathBuf {
    if let Some(dist) = binary
        .ancestors()
        .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("dist"))
    {
        return dist.join("emucap-xemu-build.json");
    }
    binary
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("emucap-xemu-build.json")
}

pub fn read_build_metadata(binary: &Path) -> io::Result<BuildMetadata> {
    let path = build_metadata_path(binary);
    let raw = crate::path_safety::read_bounded_utf8_regular_file_no_follow(&path, 256 * 1024)
        .map_err(|error| {
            patch_required(format!(
                "compatible build metadata is missing at {} ({error}); run adapters/xemu/build.sh",
                path.display()
            ))
        })?;
    serde_json::from_str(&raw).map_err(|error| {
        patch_required(format!(
            "invalid build metadata at {}: {error}",
            path.display()
        ))
    })
}

pub fn require_compatible_build(_root: &Path, binary: &Path) -> io::Result<BuildMetadata> {
    let metadata = read_build_metadata(binary)?;
    let expected = [
        ("XEMU_REPO", metadata.upstream.as_str()),
        ("XEMU_TAG", metadata.tag.as_str()),
        ("XEMU_COMMIT", metadata.commit.as_str()),
        ("XEMU_PATCHSET_SHA256", metadata.patchset_sha256.as_str()),
    ];
    for (key, actual) in expected {
        let wanted = lock_value(key)
            .ok_or_else(|| patch_required(format!("{key} is missing from upstream.lock")))?;
        if actual != wanted {
            return Err(patch_required(format!(
                "build metadata {key} mismatch: expected {wanted}, got {actual}"
            )));
        }
    }
    let expected_api = lock_value("XEMU_HOST_API")
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| patch_required("XEMU_HOST_API is invalid in upstream.lock"))?;
    if metadata.host_api != expected_api || metadata.host_api != REQUIRED_HOST_API {
        return Err(patch_required(format!(
            "host API mismatch: expected {expected_api}, got {}",
            metadata.host_api
        )));
    }
    for (name, value) in [
        ("commit", metadata.commit.as_str()),
        ("patchset_sha256", metadata.patchset_sha256.as_str()),
        ("binary_sha256", metadata.binary_sha256.as_str()),
    ] {
        let valid = if name == "commit" {
            value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        } else {
            valid_sha256(value)
        };
        if !valid {
            return Err(patch_required(format!("invalid {name} in build metadata")));
        }
    }
    let binary_identity = identity_for_regular_file(binary)?;
    if binary_identity.sha256 != metadata.binary_sha256.to_ascii_lowercase() {
        return Err(patch_required(format!(
            "binary digest mismatch: expected {}, got {}",
            metadata.binary_sha256, binary_identity.sha256
        )));
    }
    Ok(metadata)
}
