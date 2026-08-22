use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub(super) fn default_adapter_home() -> PathBuf {
    std::env::temp_dir().join(format!(
        "emucap-neogeo-{}-{}",
        std::process::id(),
        ulid::Ulid::generate().to_string().to_ascii_lowercase()
    ))
}

pub(super) fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn state_partial_sibling(path: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("save path has no parent directory: {}", path.display()),
        )
    })?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    Ok(parent.join(format!(
        ".{name}.partial.{}",
        ulid::Ulid::generate().to_string().to_ascii_lowercase()
    )))
}

pub(super) fn sha256_regular_file(path: &Path) -> io::Result<(u64, String)> {
    let mut file = crate::path_safety::open_regular_file_no_follow(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("save-state size overflow"))?;
        hasher.update(&buffer[..read]);
    }
    Ok((bytes, hex::encode(hasher.finalize())))
}
