//! Shared bounds and hashing for descriptor-based optical media.

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const MAX_DESCRIPTOR_BYTES: u64 = 1024 * 1024;
pub const MAX_GRAPH_MEMBERS: usize = 256;
pub const MAX_MEMBER_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_GRAPH_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_DECLARED_NAME_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaReference {
    /// Portable path relative to the graph's entry directory.
    pub declared_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFileIdentity {
    pub declared_name: String,
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaGraphIdentity {
    pub sha256: String,
    pub size: u64,
    pub files: Vec<MediaFileIdentity>,
}

pub fn read_descriptor(path: &Path, kind: &str, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut file = crate::path_safety::open_regular_file_no_follow(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "{kind} entry file is missing or unsafe: {}: {error}",
                path.display()
            ),
        )
    })?;
    let before = file.metadata()?;
    if before.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{kind} entry file exceeds {max_bytes} bytes: {}",
                path.display()
            ),
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{kind} entry file exceeds {max_bytes} bytes: {}",
                path.display()
            ),
        ));
    }
    if bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{kind} entry file changed while it was read: {}",
                path.display()
            ),
        ));
    }
    Ok(bytes)
}

/// Decode a text descriptor without accepting control bytes that C/C++ loader
/// parsers may treat as string terminators or alternate line boundaries.
pub fn descriptor_text<'a>(bytes: &'a [u8], kind: &str) -> io::Result<&'a str> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if let Some((offset, _)) = text
        .char_indices()
        .find(|(_, character)| character.is_control() && !matches!(character, '\t' | '\n' | '\r'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{kind} contains an unsupported control character at byte {offset}"),
        ));
    }
    Ok(text)
}

pub fn quoted_fields(
    line: &str,
    kind: &str,
    line_number: usize,
    max_fields: usize,
) -> io::Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in line.chars() {
        match character {
            '"' if quoted => {
                quoted = false;
                started = true;
            }
            '"' if !started => {
                quoted = true;
                started = true;
            }
            '"' => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{kind} has an unexpected quote at line {line_number}"),
                ));
            }
            character if character.is_whitespace() && !quoted => {
                if started {
                    if fields.len() >= max_fields {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("{kind} has too many fields at line {line_number}"),
                        ));
                    }
                    fields.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            character => {
                current.push(character);
                started = true;
            }
        }
    }
    if quoted {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{kind} has an unterminated quote at line {line_number}"),
        ));
    }
    if started {
        if fields.len() >= max_fields {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{kind} has too many fields at line {line_number}"),
            ));
        }
        fields.push(current);
    }
    Ok(fields)
}

pub fn validate_references(
    entry: &Path,
    kind: &str,
    references: &[MediaReference],
) -> io::Result<Vec<PathBuf>> {
    if references.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{kind} contains no media references: {}", entry.display()),
        ));
    }
    if references.len() > MAX_GRAPH_MEMBERS - 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{kind} references too many files: {}", entry.display()),
        ));
    }
    let root = entry.parent().unwrap_or_else(|| Path::new("."));
    let canonical_entry = entry.canonicalize()?;
    let mut paths = Vec::with_capacity(references.len());
    let mut seen = HashSet::new();
    let mut total_size = entry.metadata()?.len();
    let mut total_name_bytes = 0_usize;
    for reference in references {
        total_name_bytes = total_name_bytes
            .checked_add(reference.declared_name.len())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "media member name size overflow",
                )
            })?;
        if total_name_bytes > MAX_DECLARED_NAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "media graph declares too much file-name data",
            ));
        }
        if !crate::path_safety::is_portable_relative_member(&reference.declared_name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{kind} member path escapes or is not portable: {:?}",
                    reference.declared_name
                ),
            ));
        }
        let file =
            crate::path_safety::open_regular_member_no_follow(root, &reference.declared_name)
                .map_err(|error| member_error(kind, reference, error))?;
        let metadata = file.metadata()?;
        if metadata.len() == 0 || metadata.len() > MAX_MEMBER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{kind} member size is outside 1..={MAX_MEMBER_BYTES}: {:?}",
                    reference.declared_name
                ),
            ));
        }
        let path = crate::path_safety::regular_member_path(root, &reference.declared_name)
            .map_err(|error| member_error(kind, reference, error))?;
        if path == canonical_entry {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{kind} entry file cannot also be a media member"),
            ));
        }
        if seen.insert(path.clone()) {
            total_size = total_size.checked_add(metadata.len()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "media graph size overflow")
            })?;
            if total_size > MAX_GRAPH_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{kind} graph exceeds {MAX_GRAPH_BYTES} bytes: {}",
                        entry.display()
                    ),
                ));
            }
        }
        paths.push(path);
    }
    Ok(paths)
}

fn member_error(kind: &str, reference: &MediaReference, error: io::Error) -> io::Error {
    io::Error::new(
        if error.kind() == io::ErrorKind::NotFound {
            io::ErrorKind::NotFound
        } else {
            io::ErrorKind::InvalidData
        },
        format!(
            "{kind} member is missing or unsafe (declared as {:?}): {error}",
            reference.declared_name
        ),
    )
}

pub fn graph_identity(
    entry: &Path,
    kind: &str,
    entry_marker: &str,
    digest_domain: &[u8],
    references: &[MediaReference],
) -> io::Result<MediaGraphIdentity> {
    let entry_bytes = read_descriptor(entry, kind, MAX_DESCRIPTOR_BYTES)?;
    let paths = validate_references(entry, kind, references)?;
    let root = entry.parent().unwrap_or_else(|| Path::new("."));
    let entry_name = entry
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(entry_marker)
        .to_string();
    let mut graph_hash = Sha256::new();
    graph_hash.update(digest_domain);
    hash_bytes_into_graph(&mut graph_hash, entry_marker, &entry_bytes)?;
    let mut total_size = entry_bytes.len() as u64;
    let mut files = vec![MediaFileIdentity {
        declared_name: entry_name,
        path: entry.canonicalize()?,
        size: entry_bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&entry_bytes)),
    }];
    let mut seen = HashSet::new();
    seen.insert(files[0].path.clone());
    for (reference, canonical) in references.iter().zip(paths) {
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let mut file =
            crate::path_safety::open_regular_member_no_follow(root, &reference.declared_name)
                .map_err(|error| member_error(kind, reference, error))?;
        let before = file.metadata()?;
        let size = before.len();
        let name = reference.declared_name.as_bytes();
        graph_hash.update((name.len() as u64).to_le_bytes());
        graph_hash.update(name);
        graph_hash.update(size.to_le_bytes());
        let mut file_hash = Sha256::new();
        let mut bytes_read = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        {
            let mut limited = Read::by_ref(&mut file).take(size.saturating_add(1));
            loop {
                let read = limited.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                bytes_read = bytes_read.checked_add(read as u64).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "media member size overflow")
                })?;
                let bytes = &buffer[..read];
                graph_hash.update(bytes);
                file_hash.update(bytes);
            }
        }
        let after = file.metadata()?;
        if bytes_read != size
            || after.len() != before.len()
            || after.modified().ok() != before.modified().ok()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{kind} member changed while it was hashed: {:?}",
                    reference.declared_name
                ),
            ));
        }
        total_size = total_size.checked_add(size).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "media graph size overflow")
        })?;
        files.push(MediaFileIdentity {
            declared_name: reference.declared_name.clone(),
            path: canonical,
            size,
            sha256: hex::encode(file_hash.finalize()),
        });
    }
    if read_descriptor(entry, kind, MAX_DESCRIPTOR_BYTES)? != entry_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{kind} entry file changed while its graph identity was established: {}",
                entry.display()
            ),
        ));
    }
    Ok(MediaGraphIdentity {
        sha256: hex::encode(graph_hash.finalize()),
        size: total_size,
        files,
    })
}

fn hash_bytes_into_graph(
    graph_hash: &mut Sha256,
    declared_name: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let size = u64::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "media member is too large"))?;
    graph_hash.update((declared_name.len() as u64).to_le_bytes());
    graph_hash.update(declared_name.as_bytes());
    graph_hash.update(size.to_le_bytes());
    graph_hash.update(bytes);
    Ok(())
}

#[cfg(test)]
#[path = "media_graph_tests.rs"]
mod tests;
