use super::{link::EmulatorIdentity, runtime::CurrentManifest};
use crate::bundle::recording_manifest::{ContentIdentity, RuntimeIdentity};
use serde_json::Value;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

pub(super) fn runtime_identity(
    identity: &EmulatorIdentity,
    current: &CurrentManifest,
    revision: &str,
    max_bytes: u64,
) -> Result<RuntimeIdentity, std::io::Error> {
    let content_path = Path::new(&current.content);
    let metadata = fs::symlink_metadata(content_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::other("artifact must be a regular file"));
    }
    let (sha1, sha256, bytes) = hash_content(content_path, max_bytes)?;
    let host = identity
        .host_build
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| std::io::Error::other("emulator host identity is missing"))?;
    let field = |name: &str| {
        host.get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(String::from)
            .ok_or_else(|| std::io::Error::other(format!("emulator host identity lacks {name}")))
    };
    let upstream = field("commit")?;
    let patchset = field("patchset_sha256")?;
    let binary = field("binary_sha256")?;
    Ok(RuntimeIdentity {
        system: current.system.clone(),
        adapter_id: identity
            .adapter
            .clone()
            .ok_or_else(|| std::io::Error::other("adapter identity is missing"))?,
        server_build: crate::build_identity::BUILD_HASH.into(),
        adapter_build: identity
            .build
            .clone()
            .ok_or_else(|| std::io::Error::other("adapter build is missing"))?,
        emulator_id: host
            .get("upstream")
            .and_then(Value::as_str)
            .unwrap_or("emulator-host")
            .to_string(),
        emulator_build: binary,
        emulator_upstream_revision: upstream,
        emulator_patchset_sha256: patchset,
        launch_id: current.launch_id.clone(),
        capability_revision: revision.into(),
        content: ContentIdentity {
            sha1: Some(sha1),
            sha256: Some(sha256),
            bytes,
            path_hint: content_path
                .file_name()
                .and_then(|value| value.to_str())
                .map(String::from),
        },
    })
}

fn hash_content(path: &Path, max_bytes: u64) -> io::Result<(String, String, u64)> {
    let mut file = crate::path_safety::open_regular_file_no_follow(path)?;
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("artifact too large"))?;
        if bytes > max_bytes {
            return Err(io::Error::other("artifact exceeds byte bound"));
        }
        sha1.update(&buffer[..read]);
        sha256.update(&buffer[..read]);
    }
    Ok((
        hex::encode(sha1.finalize()),
        hex::encode(sha256.finalize()),
        bytes,
    ))
}
