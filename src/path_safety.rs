use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Portable identifier grammar for values that become file or directory names.
///
/// Identifiers contain one or more non-empty ASCII-alphanumeric segments joined
/// by single hyphens. This deliberately excludes punctuation that has path or
/// shell meaning on any supported platform.
pub fn is_hyphenated_ascii_id(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.split('-').all(|segment| {
            !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        && !is_windows_reserved_component(value)
}

/// A relative member path whose components cannot change traversal roots.
/// Backslashes are rejected even on Unix so a record remains safe when moved to
/// Windows.
pub fn is_portable_relative_member(value: &str) -> bool {
    if value.is_empty() || value.len() > 4096 || value.contains('\\') {
        return false;
    }
    let path = Path::new(value);
    !path.is_absolute()
        && path.components().all(|component| match component {
            Component::Normal(component) => component.to_str().is_some_and(is_portable_component),
            _ => false,
        })
}

fn is_portable_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.ends_with(['.', ' '])
        && !value
            .chars()
            .any(|character| character <= '\u{1f}' || r#"<>:"/\\|?*"#.contains(character))
        && !is_windows_reserved_component(value)
}

fn is_windows_reserved_component(value: &str) -> bool {
    let stem = value.split('.').next().unwrap_or(value);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
        .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

/// A portable leaf file name. Dots are allowed inside the name for file
/// extensions, but hidden, current-directory, and parent-directory forms are
/// rejected.
pub fn is_portable_file_name(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && is_portable_relative_member(value)
        && Path::new(value)
            .parent()
            .is_some_and(|parent| parent.as_os_str().is_empty())
}

/// Resolve an existing regular member without accepting symlinks in any
/// bundle-owned component.
pub fn regular_member_path(root: &Path, relative: &str) -> io::Result<PathBuf> {
    if !is_portable_relative_member(relative) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "member path must remain relative to its managed root",
        ));
    }
    let root_metadata = fs::symlink_metadata(root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed root is not a real directory",
        ));
    }
    let canonical_root = fs::canonicalize(root)?;
    let components = Path::new(relative).components().collect::<Vec<_>>();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "member path contains an unsafe component",
            ));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        let last = index + 1 == components.len();
        if metadata.file_type().is_symlink()
            || (last && !metadata.is_file())
            || (!last && !metadata.is_dir())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed member contains a symlink or unexpected file type",
            ));
        }
    }
    let canonical = fs::canonicalize(&current)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed member escapes its root",
        ));
    }
    Ok(canonical)
}

/// Open an existing regular file without following a symbolic link in the
/// final path component. Callers that own a containing directory should still
/// validate those directory components separately.
pub fn open_regular_file_no_follow(path: &Path) -> io::Result<fs::File> {
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular non-symlink file",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK prevents a replacement FIFO from hanging the caller before fstat can
        // reject it. It has no effect on regular-file reads.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let handle_metadata = file.metadata()?;
    if !handle_metadata.is_file() || !same_opened_file(&path_metadata, &handle_metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "opened path is not the validated regular file",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn same_opened_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_opened_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

/// Open a regular member relative to an already selected real directory without following any
/// member-path symlink. On Unix every component is resolved from a directory handle, so renaming
/// or replacing a path between validation and open cannot redirect the read outside `root`.
#[cfg(unix)]
pub fn open_regular_member_no_follow(root: &Path, relative: &str) -> io::Result<fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    if !is_portable_relative_member(relative) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "member path must remain relative to its managed root",
        ));
    }

    fn open_owned(path: &Path, flags: libc::c_int) -> io::Result<OwnedFd> {
        use std::os::unix::ffi::OsStrExt;

        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: `path` is a NUL-terminated owned byte string and `flags` request read-only
        // descriptors. A successful descriptor is immediately wrapped in OwnedFd.
        let fd = unsafe { libc::open(path.as_ptr(), flags) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: `fd` is newly returned and exclusively owned by this call.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    fn openat_owned(
        directory: &OwnedFd,
        component: &std::ffi::OsStr,
        flags: libc::c_int,
    ) -> io::Result<OwnedFd> {
        let component = CString::new(component.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "member component contains NUL")
        })?;
        // SAFETY: `directory` remains alive for the call, `component` is NUL-terminated, and a
        // successful descriptor is immediately wrapped in OwnedFd.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: `fd` is newly returned and exclusively owned by this call.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    let directory_flags =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
    let mut directory = open_owned(root, directory_flags)?;
    let components = Path::new(relative).components().collect::<Vec<_>>();
    for component in &components[..components.len() - 1] {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "member path contains an unsafe component",
            ));
        };
        directory = openat_owned(&directory, component, directory_flags)?;
    }
    let Component::Normal(file_name) = components[components.len() - 1] else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "member path contains an unsafe file name",
        ));
    };
    let file = openat_owned(
        &directory,
        file_name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
    )?;
    let file = fs::File::from(file);
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed member is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
pub fn open_regular_member_no_follow(root: &Path, relative: &str) -> io::Result<fs::File> {
    let path = regular_member_path(root, relative)?;
    let canonical_root = fs::canonicalize(root)?;
    let canonical_path = fs::canonicalize(&path)?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed member escapes its root",
        ));
    }
    open_regular_file_no_follow(&canonical_path)
}

pub fn read_bounded_regular_member(
    root: &Path,
    relative: &str,
    max_bytes: u64,
) -> io::Result<Vec<u8>> {
    let file = open_regular_member_no_follow(root, relative)?;
    read_bounded_opened_regular_file(file, max_bytes)
}

pub fn read_bounded_regular_file_no_follow(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let file = open_regular_file_no_follow(path)?;
    read_bounded_opened_regular_file(file, max_bytes)
}

fn read_bounded_opened_regular_file(mut file: fs::File, max_bytes: u64) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if metadata.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("managed member exceeds {max_bytes} bytes"),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("managed member exceeds {max_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

pub fn read_bounded_utf8_regular_file_no_follow(path: &Path, max_bytes: u64) -> io::Result<String> {
    String::from_utf8(read_bounded_regular_file_no_follow(path, max_bytes)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Replace an explicitly authorized output file atomically. A destination
/// symlink is rejected rather than followed or silently replaced.
pub fn atomic_write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output path has no parent"))?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    fs::create_dir_all(parent)?;
    let destination_exists = validate_file_destination(path)?;
    let file_name = output_file_name(path)?;
    let temp = unique_output_sibling(parent, file_name, "tmp");
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        publish_prepared_file(&temp, path, parent, file_name, destination_exists)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Stream an explicitly authorized source into a sibling temporary file and
/// publish it atomically. The source path may be an operator-selected symlink;
/// the managed destination may not be one.
pub fn atomic_copy_file(source: &Path, path: &Path) -> io::Result<u64> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output path has no parent"))?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    fs::create_dir_all(parent)?;
    let destination_exists = validate_file_destination(path)?;
    let file_name = output_file_name(path)?;
    let temp = unique_output_sibling(parent, file_name, "copy");
    let result = (|| {
        let mut input = fs::File::open(source)?;
        if !input.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "copy source is not a regular file",
            ));
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut output = options.open(&temp)?;
        let copied = io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        if let Ok(permissions) = input.metadata().map(|metadata| metadata.permissions()) {
            fs::set_permissions(&temp, permissions)?;
        }
        publish_prepared_file(&temp, path, parent, file_name, destination_exists)?;
        Ok(copied)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn validate_file_destination(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path is a symbolic link",
        )),
        Ok(metadata) if metadata.is_dir() => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output path is a directory",
        )),
        Ok(metadata) if !metadata.is_file() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path is not a regular file",
        )),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn output_file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid output file name"))
}

fn unique_output_sibling(parent: &Path, file_name: &str, label: &str) -> PathBuf {
    parent.join(format!(
        ".{file_name}.{label}-{}",
        ulid::Ulid::generate().to_string().to_ascii_lowercase()
    ))
}

fn publish_prepared_file(
    temp: &Path,
    path: &Path,
    parent: &Path,
    file_name: &str,
    destination_exists: bool,
) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (parent, file_name, destination_exists);
        fs::rename(temp, path)
    }
    #[cfg(windows)]
    {
        if !destination_exists {
            return fs::rename(temp, path);
        }
        let backup = unique_output_sibling(parent, file_name, "old");
        fs::rename(path, &backup)?;
        match fs::rename(temp, path) {
            Ok(()) => {
                let _ = fs::remove_file(backup);
                Ok(())
            }
            Err(error) => {
                let _ = fs::rename(backup, path);
                Err(error)
            }
        }
    }
}
