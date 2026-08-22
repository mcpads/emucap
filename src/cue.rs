//! CUE entry-file parsing and whole-disc identity.

use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const GRAPH_DOMAIN: &[u8] = b"emucap-cue-graph-v1\0";
const MAX_REFERENCED_FILES: usize = 256;
const MAX_CUE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueReference {
    pub declared_name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueFileIdentity {
    pub declared_name: String,
    pub path: PathBuf,
    pub size: u64,
    pub sha1: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueGraphIdentity {
    pub sha1: String,
    pub size: u64,
    pub files: Vec<CueFileIdentity>,
}

pub fn referenced_files(cue: &Path) -> io::Result<Vec<CueReference>> {
    let mut file = crate::path_safety::open_regular_file_no_follow(cue).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "CUE entry file is missing or unsafe: {}: {error}",
                cue.display()
            ),
        )
    })?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_CUE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "CUE entry file exceeds {MAX_CUE_BYTES} bytes: {}",
                cue.display()
            ),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_CUE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CUE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "CUE entry file exceeds {MAX_CUE_BYTES} bytes: {}",
                cue.display()
            ),
        ));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("read CUE entry file {}: {error}", cue.display()),
        )
    })?;
    let base = cue.parent().unwrap_or_else(|| Path::new("."));
    let mut references = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let Some((directive, rest)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        if !directive.eq_ignore_ascii_case("FILE") {
            continue;
        }
        let rest = rest.trim_start();
        let declared_name = if let Some(after_quote) = rest.strip_prefix('"') {
            after_quote.split_once('"').map(|(name, _)| name)
        } else {
            rest.split_whitespace().next()
        }
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CUE FILE directive has no path at {}:{}",
                    cue.display(),
                    line_index + 1
                ),
            )
        })?;
        if !crate::path_safety::is_portable_relative_member(declared_name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CUE FILE path escapes or is not portable at {}:{}: {:?}",
                    cue.display(),
                    line_index + 1,
                    declared_name
                ),
            ));
        }
        references.push(CueReference {
            declared_name: declared_name.to_string(),
            path: base.join(declared_name),
        });
        if references.len() > MAX_REFERENCED_FILES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CUE references more than {MAX_REFERENCED_FILES} files: {}",
                    cue.display()
                ),
            ));
        }
    }
    if references.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("CUE contains no FILE directives: {}", cue.display()),
        ));
    }
    Ok(references)
}

pub fn validate_graph(cue: &Path) -> io::Result<Vec<CueReference>> {
    let mut references = referenced_files(cue)?;
    let base = cue.parent().unwrap_or_else(|| Path::new("."));
    for reference in &mut references {
        reference.path = crate::path_safety::regular_member_path(base, &reference.declared_name)
            .map_err(|error| {
                let kind = if error.kind() == io::ErrorKind::NotFound {
                    io::ErrorKind::NotFound
                } else {
                    io::ErrorKind::InvalidData
                };
                io::Error::new(
                    kind,
                    format!(
                        "CUE referenced file is missing or unsafe: {} (declared as {:?}): {error}",
                        reference.path.display(),
                        reference.declared_name
                    ),
                )
            })?;
    }
    Ok(references)
}

pub fn graph_identity(cue: &Path) -> io::Result<CueGraphIdentity> {
    let references = validate_graph(cue)?;
    let cue_name = cue
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("disc.cue")
        .to_string();
    let mut inputs = vec![CueReference {
        declared_name: cue_name,
        path: cue.to_path_buf(),
    }];
    let mut seen = HashSet::new();
    seen.insert(cue.canonicalize()?);
    for reference in references {
        let canonical = reference.path.canonicalize()?;
        if seen.insert(canonical) {
            inputs.push(reference);
        }
    }

    let mut graph_hasher = Sha1::new();
    graph_hasher.update(GRAPH_DOMAIN);
    let mut total_size = 0_u64;
    let mut files = Vec::with_capacity(inputs.len());
    for input in inputs {
        let canonical = input.path.canonicalize()?;
        let mut file = crate::path_safety::open_regular_file_no_follow(&canonical)?;
        let metadata = file.metadata()?;
        let size = metadata.len();
        total_size = total_size
            .checked_add(size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CUE graph size overflow"))?;
        let name = input.declared_name.as_bytes();
        let name_length = u64::try_from(name.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "CUE file name is too long"))?;
        graph_hasher.update(name_length.to_le_bytes());
        graph_hasher.update(name);
        graph_hasher.update(size.to_le_bytes());

        let mut file_hasher = Sha1::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let bytes = &buffer[..read];
            graph_hasher.update(bytes);
            file_hasher.update(bytes);
        }
        files.push(CueFileIdentity {
            declared_name: input.declared_name,
            path: canonical,
            size,
            sha1: hex::encode(file_hasher.finalize()),
        });
    }

    Ok(CueGraphIdentity {
        sha1: hex::encode(graph_hasher.finalize()),
        size: total_size,
        files,
    })
}

#[cfg(test)]
#[path = "cue_tests.rs"]
mod tests;
