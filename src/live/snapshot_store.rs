use super::runtime::RuntimeStore;
use crate::bundle::{
    recording_manifest::RuntimeIdentity,
    snapshot::{Receipt, MAX_RECEIPT_BYTES, MAX_SNAPSHOT_BYTES},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub destination: String,
    pub source: RuntimeIdentity,
    pub snapshot_id: String,
}

pub(super) struct Store {
    pub directory: PathBuf,
    _lock: File,
}
fn error(message: &str) -> io::Error {
    io::Error::other(message)
}

impl Store {
    pub fn open(runtime: &RuntimeStore, key: &str, create: bool) -> io::Result<(Self, bool)> {
        if key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(error(
                "snapshot_key must contain 1..64 lowercase ASCII letters, digits or hyphens",
            ));
        }
        let root = runtime.root().join("state-snapshots");
        if create {
            runtime.create_managed_dir(&root)?;
        }
        let directory = root.join(key);
        let fresh = if create {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&directory) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
                Err(e) => return Err(e),
            }
        } else {
            false
        };
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(error("unsafe snapshot directory"));
        }
        if fresh {
            // Persist both directory entries before the request can trigger serialization.
            super::runtime::sync_parent(&root)?;
            super::runtime::sync_parent(runtime.root())?;
        }
        // Check every ancestor through the runtime-owned path resolver before opening the lock.
        runtime.create_managed_dir(&directory)?;
        let lock_path = directory.join("lock");
        let lock = match private_file(&lock_path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                crate::path_safety::open_regular_file_no_follow(&lock_path)?
            }
            Err(e) => return Err(e),
        };
        fs2::FileExt::try_lock_exclusive(&lock)?;
        Ok((
            Self {
                directory,
                _lock: lock,
            },
            fresh,
        ))
    }
    pub fn reserve(&self, request: &Request) -> io::Result<()> {
        super::runtime::write_atomic_json(&self.directory.join("request.json"), request)
    }
    pub fn request(&self) -> io::Result<Request> {
        serde_json::from_slice(&crate::path_safety::read_bounded_regular_member(
            &self.directory,
            "request.json",
            MAX_RECEIPT_BYTES,
        )?)
        .map_err(io::Error::other)
    }
    pub fn terminal(&self, value: &Value) -> io::Result<()> {
        super::runtime::write_atomic_json(&self.directory.join("terminal.json"), value)
    }
    pub fn outcome(&self) -> io::Result<Option<Value>> {
        match crate::path_safety::read_bounded_regular_member(
            &self.directory,
            "terminal.json",
            MAX_RECEIPT_BYTES,
        ) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub fn publish(&self, receipt: &Receipt, bytes: &[u8]) -> io::Result<()> {
        let stage = self.directory.join("staging");
        fs::create_dir(&stage)?;
        let result = (|| {
            write_new(&stage.join("state.bin"), bytes)?;
            write_new(&stage.join("receipt.json"), &serde_json::to_vec(receipt)?)?;
            super::runtime::sync_parent(&stage)?;
            fs::rename(&stage, self.directory.join("published"))?;
            super::runtime::sync_parent(&self.directory)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(stage);
        }
        result
    }
    pub fn acquire(&self) -> io::Result<(Receipt, Vec<u8>)> {
        let receipt: Receipt =
            serde_json::from_slice(&crate::path_safety::read_bounded_regular_member(
                &self.directory,
                "published/receipt.json",
                MAX_RECEIPT_BYTES,
            )?)
            .map_err(io::Error::other)?;
        let bytes = crate::path_safety::read_bounded_regular_member(
            &self.directory,
            "published/state.bin",
            MAX_SNAPSHOT_BYTES,
        )?;
        receipt.verify(&bytes).map_err(io::Error::other)?;
        let request = self.request()?;
        if request.source != receipt.body.source || request.snapshot_id != receipt.body.snapshot_id
        {
            return Err(error(
                "receipt does not match the producer's reserved request",
            ));
        }
        Ok((receipt, bytes))
    }
}
fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = private_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
pub(super) fn export(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(error("snapshot destination must be absolute"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| error("missing destination parent"))?;
    let temporary = parent.join(format!(".emucap-snapshot-{}.tmp", ulid::Ulid::generate()));
    let result = (|| {
        write_new(&temporary, bytes)?;
        crate::path_safety::replace_file_atomically(&temporary, path)?;
        super::runtime::sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Export must not overwrite producer-retained evidence or lifecycle metadata, including aliases.
pub(super) fn validate_destination(runtime: &RuntimeStore, path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(error("snapshot destination must be absolute"));
    }
    let root = fs::canonicalize(runtime.root())?;
    let mut parent = path
        .parent()
        .ok_or_else(|| error("missing destination parent"))?;
    loop {
        match fs::canonicalize(parent) {
            Ok(resolved) => {
                if resolved.starts_with(&root) {
                    return Err(error(
                        "snapshot export cannot replace producer-owned runtime storage",
                    ));
                }
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                parent = parent.parent().ok_or(e)?;
            }
            Err(e) => return Err(e),
        }
    }
}
