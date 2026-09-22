//! Adapter-private snapshot envelope. Native code owns device serialization.
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

use super::{BridgeResult, OpenMsxBridgeError, PreparedSession, OPENMSX_HOST_API};

const MAX_PAYLOAD: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE: u64 = 2 * MAX_PAYLOAD + 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    host_api: u32,
    system: String,
    machine: String,
    firmware_manifest_sha256: Option<String>,
    source_sha1: String,
    source_size: u64,
    machine_sha256: String,
    disk_sha256: String,
}

pub(super) struct StateScratch {
    root: PathBuf,
    retained: bool,
}

impl StateScratch {
    pub(super) fn new(session: &PreparedSession) -> BridgeResult<Self> {
        let parent = session
            .user_data
            .parent()
            .ok_or_else(|| invalid("missing generation root"))?;
        let root = parent.join(format!("state-{}", ulid::Ulid::generate()));
        fs::create_dir(&root)?;
        Ok(Self {
            root,
            retained: false,
        })
    }

    pub(super) fn machine(&self) -> PathBuf {
        self.root.join("machine.state")
    }
    pub(super) fn disk(&self) -> PathBuf {
        self.root.join("disk.dsk")
    }

    // After activation the native board owns an open disk in this directory.
    pub(super) fn retain(&mut self) {
        self.retained = true;
    }
}

impl Drop for StateScratch {
    fn drop(&mut self) {
        if !self.retained {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub(super) fn publish(
    path: &Path,
    session: &PreparedSession,
    scratch: &StateScratch,
) -> BridgeResult<()> {
    let machine = read_payload(&scratch.machine())?;
    let disk = read_payload(&scratch.disk())?;
    validate_disk(&disk)?;
    let manifest = Manifest {
        format: "emucap-openmsx-disk-state".into(),
        version: 1,
        host_api: OPENMSX_HOST_API,
        system: session.system.clone(),
        machine: session.machine.clone(),
        firmware_manifest_sha256: session.firmware_manifest_sha256.clone(),
        source_sha1: session.media.source_sha1.clone(),
        source_size: session.media.source_size,
        machine_sha256: digest(&machine),
        disk_sha256: digest(&disk),
    };
    let manifest = serde_json::to_vec(&manifest).map_err(|e| invalid(e.to_string()))?;
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", manifest.as_slice()),
        ("machine.state", &machine),
        ("disk.dsk", &disk),
    ] {
        archive
            .start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .map_err(|e| invalid(e.to_string()))?;
        archive.write_all(bytes)?;
    }
    let bytes = archive
        .finish()
        .map_err(|e| invalid(e.to_string()))?
        .into_inner();
    crate::path_safety::atomic_write_file(path, &bytes)?;
    Ok(())
}

pub(super) fn prepare(path: &Path, session: &PreparedSession) -> BridgeResult<StateScratch> {
    let bytes = crate::path_safety::read_bounded_regular_file_no_follow(path, MAX_ARCHIVE)?;
    if !bytes.starts_with(b"PK") {
        return prepare_legacy(bytes, session);
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| invalid(format!("invalid snapshot archive: {error}")))?;
    if archive.len() != 3 {
        return Err(invalid("expected exactly three snapshot members"));
    }
    let metadata = member(&mut archive, "manifest.json", 16 * 1024)?;
    let manifest: Manifest =
        serde_json::from_slice(&metadata).map_err(|e| invalid(e.to_string()))?;
    if manifest.format != "emucap-openmsx-disk-state"
        || manifest.version != 1
        || manifest.host_api != OPENMSX_HOST_API
        || manifest.system != session.system
        || manifest.machine != session.machine
        || manifest.firmware_manifest_sha256 != session.firmware_manifest_sha256
        || manifest.source_sha1 != session.media.source_sha1
        || manifest.source_size != session.media.source_size
    {
        return Err(invalid(
            "snapshot source, machine, firmware or host ABI does not match this launch",
        ));
    }
    let machine = member(&mut archive, "machine.state", MAX_PAYLOAD)?;
    let disk = member(&mut archive, "disk.dsk", MAX_PAYLOAD)?;
    validate_disk(&disk)?;
    if machine.is_empty()
        || digest(&machine) != manifest.machine_sha256
        || digest(&disk) != manifest.disk_sha256
    {
        return Err(invalid("snapshot payload digest mismatch"));
    }
    let scratch = StateScratch::new(session)?;
    fs::write(scratch.machine(), machine)?;
    fs::write(scratch.disk(), disk)?;
    Ok(scratch)
}

fn prepare_legacy(machine: Vec<u8>, session: &PreparedSession) -> BridgeResult<StateScratch> {
    if machine.len() as u64 > MAX_PAYLOAD
        || !(machine.starts_with(b"\x1f\x8b") || machine.starts_with(b"<?xml"))
    {
        return Err(invalid("unrecognized legacy/raw disk state"));
    }
    // Legacy states only have a disk checksum. Supply the explicitly admitted
    // source, never a historical path from the state. Native restore must still
    // prove its checksum equals the saved disk checksum before using the candidate.
    let disk = read_payload(&session.media.source_path)?;
    validate_disk(&disk)?;
    if disk.len() as u64 != session.media.source_size
        || hex::encode(sha1::Sha1::digest(&disk)) != session.media.source_sha1
    {
        return Err(invalid(
            "legacy restore requires unchanged admitted source bytes",
        ));
    }
    let scratch = StateScratch::new(session)?;
    fs::write(scratch.machine(), machine)?;
    fs::write(scratch.disk(), disk)?;
    Ok(scratch)
}

fn member(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    name: &str,
    max: u64,
) -> BridgeResult<Vec<u8>> {
    let file = archive.by_name(name).map_err(|e| invalid(e.to_string()))?;
    if file.size() > max {
        return Err(invalid(format!(
            "snapshot member {name} exceeds size limit"
        )));
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| invalid(e.to_string()))?;
    if bytes.len() as u64 > max {
        return Err(invalid("snapshot decompression exceeded size limit"));
    }
    Ok(bytes)
}

fn read_payload(path: &Path) -> BridgeResult<Vec<u8>> {
    Ok(crate::path_safety::read_bounded_regular_file_no_follow(
        path,
        MAX_PAYLOAD,
    )?)
}

fn validate_disk(bytes: &[u8]) -> BridgeResult<()> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(512) {
        return Err(invalid("invalid sector image size"));
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn invalid(message: impl std::fmt::Display) -> OpenMsxBridgeError {
    OpenMsxBridgeError::BadParams(format!("openMSX disk state: {message}"))
}

#[cfg(test)]
#[path = "disk_state_tests.rs"]
mod tests;
