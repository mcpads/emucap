use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use super::recording_capability::RecordingInputMovieCapability;
use crate::bundle::recording_manifest::InputMovieIdentity;
use crate::input_movie::canonical_recording_movie;

#[derive(Debug, Clone)]
pub struct AcquiredRecordingMovie {
    pub canonical_bytes: Vec<u8>,
    pub identity: InputMovieIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingInputError {
    #[error("input movie path must be absolute")]
    RelativePath,
    #[error("input movie must be a real regular file")]
    UnsafeFile,
    #[error("input movie exceeds {0} bytes")]
    ByteLimit(u64),
    #[error("input movie is not UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("input movie is invalid: {0}")]
    Invalid(String),
    #[error("input movie changed while it was acquired")]
    Changed,
    #[error("input movie I/O failed: {0}")]
    Io(#[from] io::Error),
}

fn open_without_following(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[cfg(unix)]
fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(not(unix))]
fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

pub fn acquire_recording_movie(
    path: &Path,
    frames: u64,
    capability: &RecordingInputMovieCapability,
) -> Result<AcquiredRecordingMovie, RecordingInputError> {
    if !path.is_absolute() {
        return Err(RecordingInputError::RelativePath);
    }
    let path_before = fs::symlink_metadata(path)?;
    if path_before.file_type().is_symlink() || !path_before.is_file() {
        return Err(RecordingInputError::UnsafeFile);
    }
    let mut file = open_without_following(path)?;
    let handle_before = file.metadata()?;
    if !same_identity(&path_before, &handle_before) {
        return Err(RecordingInputError::Changed);
    }
    if handle_before.len() > capability.max_bytes {
        return Err(RecordingInputError::ByteLimit(capability.max_bytes));
    }

    let mut raw = Vec::with_capacity(usize::try_from(handle_before.len()).unwrap_or(0));
    file.by_ref()
        .take(capability.max_bytes.saturating_add(1))
        .read_to_end(&mut raw)?;
    if u64::try_from(raw.len()).unwrap_or(u64::MAX) > capability.max_bytes {
        return Err(RecordingInputError::ByteLimit(capability.max_bytes));
    }

    let handle_after = file.metadata()?;
    let path_after = fs::symlink_metadata(path)?;
    if path_after.file_type().is_symlink()
        || !path_after.is_file()
        || !same_identity(&handle_before, &handle_after)
        || !same_identity(&handle_after, &path_after)
    {
        return Err(RecordingInputError::Changed);
    }

    let text = std::str::from_utf8(&raw)?;
    let canonical = canonical_recording_movie(text, frames, capability.max_buttons_per_frame)
        .map_err(RecordingInputError::Invalid)?;
    if u64::try_from(canonical.bytes.len()).unwrap_or(u64::MAX) > capability.max_bytes {
        return Err(RecordingInputError::ByteLimit(capability.max_bytes));
    }
    let identity = InputMovieIdentity {
        format: capability.format.clone(),
        port: capability.port,
        frames,
        bytes: u64::try_from(canonical.bytes.len()).unwrap_or(u64::MAX),
        sha256: hex::encode(Sha256::digest(&canonical.bytes)),
    };
    Ok(AcquiredRecordingMovie {
        canonical_bytes: canonical.bytes,
        identity,
    })
}
