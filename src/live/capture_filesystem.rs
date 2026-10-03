use crate::bundle::publish::RECOVERY_OWNER_MEMBER;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::{fs, io, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemIdentity {
    pub canonical_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
    // Inodes can be reused as soon as a directory is removed. Missing birth
    // time (including older capsules) cannot establish ownership for recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<std::time::SystemTime>,
}

impl FilesystemIdentity {
    pub fn capture(path: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "capture-owned path is not a real directory: {}",
                    path.display()
                ),
            ));
        }
        let canonical_path = fs::canonicalize(path)?.display().to_string();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                canonical_path,
                device: Some(metadata.dev()),
                inode: Some(metadata.ino()),
                created_at: metadata.created().ok(),
            })
        }
        #[cfg(windows)]
        {
            let (device, inode) = directory_ids(path)?;
            Ok(Self {
                canonical_path,
                device: Some(device),
                inode: Some(inode),
                created_at: metadata.created().ok(),
            })
        }
    }

    pub fn matches(&self, path: &Path) -> io::Result<bool> {
        let current = Self::capture(path)?;
        Ok(self.canonical_path == current.canonical_path
            && self.device.is_some()
            && self.device == current.device
            && self.inode.is_some()
            && self.inode == current.inode
            && self.created_at.is_some()
            && self.created_at == current.created_at)
    }
}

pub(super) fn create_staging_owner(staging: &Path) -> io::Result<String> {
    let token = ulid::Ulid::generate().to_string();
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(staging.join(RECOVERY_OWNER_MEMBER))?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    Ok(token)
}

pub(super) fn staging_owner_matches(staging: &Path, expected: Option<&str>) -> io::Result<bool> {
    let Some(expected) = expected.filter(|value| value.len() == 26) else {
        return Ok(false);
    };
    let file =
        match crate::path_safety::open_regular_member_no_follow(staging, RECOVERY_OWNER_MEMBER) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                ) =>
            {
                return Ok(false)
            }
            Err(error) => return Err(error),
        };
    let mut bytes = Vec::new();
    file.take(27).read_to_end(&mut bytes)?;
    Ok(bytes == expected.as_bytes())
}

#[cfg(windows)]
fn directory_ids(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle remains open and info is writable for this call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture path is not a real directory",
        ));
    }
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}
