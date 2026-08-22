//! CUE entry-file parsing and whole-disc identity.

use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::collections::HashSet;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

// Keep the established SHA-1 domain stable because Neo Geo CD already exposes that digest.
const GRAPH_SHA1_DOMAIN: &[u8] = b"emucap-cue-graph-v1\0";
const GRAPH_SHA256_DOMAIN: &[u8] = b"emucap-cue-graph-sha256\0";
const MAX_REFERENCED_FILES: usize = 255;
const MAX_CUE_BYTES: u64 = 64 * 1024 - 1;
const MAX_CUE_LINE_BYTES: usize = 510;
const MAX_TRACK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_GRAPH_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SBI_BYTES: u64 = 8 * 1024 * 1024;

const CUE_FILE_TYPES: &[&str] = &[
    "BINARY", "MOTOROLA", "OGG", "VORBIS", "WAVE", "WAV", "PCM", "MPC", "MP+",
];
const CUE_TRACK_TYPES: &[&str] = &[
    "AUDIO",
    "CDG",
    "MODE1/2048",
    "MODE1/2352",
    "MODE2",
    "MODE2/2336",
    "MODE2/2048",
    "MODE2/2324",
    "MODE2/2352",
    "MODE2_FORM1",
    "MODE2_FORM2",
    "MODE2_FORM_MIX",
    "MODE2_RAW",
    "CDI/2336",
    "CDI/2352",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueReference {
    pub declared_name: String,
    pub path: PathBuf,
    role: CueMemberRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CueMemberRole {
    Track,
    MednafenSbi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueFileIdentity {
    pub declared_name: String,
    pub path: PathBuf,
    pub size: u64,
    pub sha1: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueGraphIdentity {
    pub sha1: String,
    pub sha256: String,
    pub size: u64,
    pub files: Vec<CueFileIdentity>,
}

fn read_entry(cue: &Path) -> io::Result<Vec<u8>> {
    let mut file = crate::path_safety::open_regular_file_no_follow(cue).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "CUE entry file is missing or unsafe: {}: {error}",
                cue.display()
            ),
        )
    })?;
    let before = file.metadata()?;
    if before.len() > MAX_CUE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "CUE entry file exceeds {MAX_CUE_BYTES} bytes: {}",
                cue.display()
            ),
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
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
    let after = file.metadata()?;
    if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "CUE entry file changed while it was read: {}",
                cue.display()
            ),
        ));
    }
    Ok(bytes)
}

fn tokenize_directive(line: &str, cue: &Path, line_number: usize) -> io::Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut token_started = false;
    for character in line.chars() {
        match character {
            '"' if quoted => {
                quoted = false;
                token_started = true;
            }
            '"' if !token_started => {
                quoted = true;
                token_started = true;
            }
            '"' => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "CUE directive has an unexpected quote at {}:{}",
                        cue.display(),
                        line_number
                    ),
                ));
            }
            character if character.is_whitespace() && !quoted => {
                if token_started {
                    if tokens.len() >= 3 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "CUE directive has too many fields at {}:{}",
                                cue.display(),
                                line_number
                            ),
                        ));
                    }
                    tokens.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            character => {
                current.push(character);
                token_started = true;
            }
        }
    }
    if quoted {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "CUE directive has an unterminated quote at {}:{}",
                cue.display(),
                line_number
            ),
        ));
    }
    if token_started {
        if tokens.len() >= 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CUE directive has too many fields at {}:{}",
                    cue.display(),
                    line_number
                ),
            ));
        }
        tokens.push(current);
    }
    Ok(tokens)
}

fn parse_referenced_files(cue: &Path, bytes: &[u8]) -> io::Result<Vec<CueReference>> {
    let text = crate::media_graph::descriptor_text(bytes, "CUE")?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let base = cue.parent().unwrap_or_else(|| Path::new("."));
    let mut references = Vec::new();
    let mut current_file_has_track = false;
    let mut last_track = None;
    for (line_index, line) in text.lines().enumerate() {
        if line.len() > MAX_CUE_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CUE line exceeds {MAX_CUE_LINE_BYTES} bytes at {}:{}",
                    cue.display(),
                    line_index + 1
                ),
            ));
        }
        let trimmed = line.trim_start();
        let directive = trimmed
            .split_once(char::is_whitespace)
            .map(|(directive, _)| directive)
            .unwrap_or(trimmed);
        if !directive.eq_ignore_ascii_case("FILE") && !directive.eq_ignore_ascii_case("TRACK") {
            continue;
        }
        let line_number = line_index + 1;
        let tokens = tokenize_directive(trimmed, cue, line_number)?;
        if directive.eq_ignore_ascii_case("TRACK") {
            if references.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "CUE TRACK appears before FILE at {}:{line_number}",
                        cue.display()
                    ),
                ));
            }
            if tokens.len() != 3
                || !tokens[1].bytes().all(|byte| byte.is_ascii_digit())
                || !CUE_TRACK_TYPES
                    .iter()
                    .any(|kind| tokens[2].eq_ignore_ascii_case(kind))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "malformed CUE TRACK directive at {}:{line_number}",
                        cue.display()
                    ),
                ));
            }
            let track = tokens[1].parse::<u8>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "invalid CUE track number at {}:{line_number}",
                        cue.display()
                    ),
                )
            })?;
            if !(1..=99).contains(&track)
                || last_track.is_some_and(|previous| track != previous + 1)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "CUE track numbers must be consecutive and within 1..99 at {}:{line_number}",
                        cue.display()
                    ),
                ));
            }
            last_track = Some(track);
            current_file_has_track = true;
            continue;
        }

        if !references.is_empty() && !current_file_has_track {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CUE FILE has no TRACK before the next FILE at {}:{line_number}",
                    cue.display()
                ),
            ));
        }
        if tokens.len() != 3
            || tokens[1].is_empty()
            || !CUE_FILE_TYPES
                .iter()
                .any(|kind| tokens[2].eq_ignore_ascii_case(kind))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "malformed CUE FILE directive at {}:{line_number}",
                    cue.display()
                ),
            ));
        }
        let declared_name = &tokens[1];
        if !crate::path_safety::is_portable_relative_member(declared_name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CUE FILE path escapes or is not portable at {}:{}: {:?}",
                    cue.display(),
                    line_number,
                    declared_name
                ),
            ));
        }
        references.push(CueReference {
            declared_name: declared_name.to_string(),
            path: base.join(declared_name),
            role: CueMemberRole::Track,
        });
        current_file_has_track = false;
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
    if !current_file_has_track {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("CUE FILE has no TRACK: {}", cue.display()),
        ));
    }
    Ok(references)
}

pub fn referenced_files(cue: &Path) -> io::Result<Vec<CueReference>> {
    let bytes = read_entry(cue)?;
    parse_referenced_files(cue, &bytes)
}

pub fn mednafen_sbi_reference(cue: &Path) -> io::Result<CueReference> {
    let stem = cue
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CUE stem is not UTF-8"))?;
    let extension = cue
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CUE extension is not UTF-8"))?;
    let sbi_extension = "sbi"
        .chars()
        .zip(extension.chars())
        .map(|(target, source)| {
            if source.is_ascii_uppercase() {
                target.to_ascii_uppercase()
            } else {
                target
            }
        })
        .collect::<String>();
    let declared_name = format!("{stem}.{sbi_extension}");
    let candidate = cue
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&declared_name);
    if !crate::path_safety::is_portable_relative_member(&declared_name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Mednafen SBI sidecar name is not portable",
        ));
    }
    Ok(CueReference {
        declared_name,
        path: candidate,
        role: CueMemberRole::MednafenSbi,
    })
}

fn append_mednafen_sbi(cue: &Path, references: &mut Vec<CueReference>) -> io::Result<()> {
    let reference = mednafen_sbi_reference(cue)?;
    let candidate = &reference.path;
    match std::fs::symlink_metadata(candidate) {
        Ok(_) => {
            if references.len() >= MAX_REFERENCED_FILES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "CUE graph contains too many media members",
                ));
            }
            references.push(reference);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn checked_member_metadata(
    file: &mut std::fs::File,
    cue: &Path,
    reference: &CueReference,
) -> io::Result<std::fs::Metadata> {
    let metadata = file.metadata()?;
    if metadata.len() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "CUE referenced track is empty at {} (declared as {:?})",
                cue.display(),
                reference.declared_name
            ),
        ));
    }
    if metadata.len() > MAX_TRACK_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "CUE referenced track exceeds {MAX_TRACK_BYTES} bytes at {} (declared as {:?})",
                cue.display(),
                reference.declared_name
            ),
        ));
    }
    if reference.role == CueMemberRole::MednafenSbi {
        if metadata.len() < 4 || metadata.len() > MAX_SBI_BYTES || (metadata.len() - 4) % 14 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Mednafen SBI sidecar size or record length is invalid",
            ));
        }
        let mut header = [0_u8; 4];
        file.read_exact(&mut header)?;
        if &header != b"SBI\0" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Mednafen SBI sidecar has an invalid header",
            ));
        }
        let mut record = [0_u8; 14];
        for _ in 0..(metadata.len() - 4) / 14 {
            file.read_exact(&mut record)?;
            let valid_bcd = |byte: u8| byte >> 4 <= 9 && byte & 0x0f <= 9;
            if !record[..3].iter().copied().all(valid_bcd) || record[3] != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Mednafen SBI sidecar contains an invalid record",
                ));
            }
        }
        file.seek(SeekFrom::Start(0))?;
    }
    Ok(metadata)
}

fn member_error(reference: &CueReference, error: io::Error) -> io::Error {
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
}

fn validate_references(
    cue: &Path,
    mut references: Vec<CueReference>,
) -> io::Result<Vec<CueReference>> {
    let base = cue.parent().unwrap_or_else(|| Path::new("."));
    let canonical_entry = cue.canonicalize()?;
    let mut total_size = cue.metadata()?.len();
    let mut total_name_bytes = 0_usize;
    let mut seen = HashSet::new();
    for reference in &mut references {
        total_name_bytes = total_name_bytes
            .checked_add(reference.declared_name.len())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "CUE member name size overflow")
            })?;
        if total_name_bytes > crate::media_graph::MAX_DECLARED_NAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CUE graph declares too much file-name data",
            ));
        }
        let mut file =
            crate::path_safety::open_regular_member_no_follow(base, &reference.declared_name)
                .map_err(|error| member_error(reference, error))?;
        let metadata = checked_member_metadata(&mut file, cue, reference)?;
        reference.path = crate::path_safety::regular_member_path(base, &reference.declared_name)
            .map_err(|error| member_error(reference, error))?;
        if reference.path == canonical_entry {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CUE entry file cannot also be a track member",
            ));
        }
        if seen.insert(reference.path.clone()) {
            total_size = total_size.checked_add(metadata.len()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "CUE graph size overflow")
            })?;
            if total_size > MAX_GRAPH_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "CUE graph exceeds {MAX_GRAPH_BYTES} bytes: {}",
                        cue.display()
                    ),
                ));
            }
        }
    }
    Ok(references)
}

fn references_for_identity(
    cue: &Path,
    cue_bytes: &[u8],
    include_mednafen_sbi: bool,
) -> io::Result<Vec<CueReference>> {
    let mut references = parse_referenced_files(cue, cue_bytes)?;
    if include_mednafen_sbi {
        append_mednafen_sbi(cue, &mut references)?;
    }
    validate_references(cue, references)
}

pub fn validate_graph(cue: &Path) -> io::Result<Vec<CueReference>> {
    let cue_bytes = read_entry(cue)?;
    references_for_identity(cue, &cue_bytes, false)
}

pub fn validate_mednafen_graph(cue: &Path) -> io::Result<Vec<CueReference>> {
    let cue_bytes = read_entry(cue)?;
    references_for_identity(cue, &cue_bytes, true)
}

pub fn graph_identity(cue: &Path) -> io::Result<CueGraphIdentity> {
    graph_identity_for_loader(cue, false)
}

pub fn mednafen_graph_identity(cue: &Path) -> io::Result<CueGraphIdentity> {
    graph_identity_for_loader(cue, true)
}

fn graph_identity_for_loader(
    cue: &Path,
    include_mednafen_sbi: bool,
) -> io::Result<CueGraphIdentity> {
    let cue_bytes = read_entry(cue)?;
    let references = references_for_identity(cue, &cue_bytes, include_mednafen_sbi)?;
    let cue_name = cue
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("disc.cue")
        .to_string();
    let base = cue.parent().unwrap_or_else(|| Path::new("."));
    let cue_path = cue.canonicalize()?;
    let mut inputs = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(cue_path.clone());
    for mut reference in references {
        let canonical = crate::path_safety::regular_member_path(base, &reference.declared_name)
            .map_err(|error| member_error(&reference, error))?;
        if canonical == cue_path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CUE entry file cannot also be a media member",
            ));
        }
        if seen.insert(canonical.clone()) {
            reference.path = canonical;
            inputs.push(reference);
        }
    }

    let mut graph_sha1 = Sha1::new();
    graph_sha1.update(GRAPH_SHA1_DOMAIN);
    let mut graph_sha256 = Sha256::new();
    graph_sha256.update(GRAPH_SHA256_DOMAIN);
    let cue_size = cue_bytes.len() as u64;
    let cue_name_bytes = cue_name.as_bytes();
    graph_sha1.update((cue_name_bytes.len() as u64).to_le_bytes());
    graph_sha1.update(cue_name_bytes);
    graph_sha1.update(cue_size.to_le_bytes());
    graph_sha1.update(&cue_bytes);
    graph_sha256.update((b"entry.cue".len() as u64).to_le_bytes());
    graph_sha256.update(b"entry.cue");
    graph_sha256.update(cue_size.to_le_bytes());
    graph_sha256.update(&cue_bytes);

    let mut total_size = cue_size;
    let mut files = Vec::with_capacity(inputs.len() + 1);
    files.push(CueFileIdentity {
        declared_name: cue_name,
        path: cue_path,
        size: cue_size,
        sha1: hex::encode(Sha1::digest(&cue_bytes)),
        sha256: hex::encode(Sha256::digest(&cue_bytes)),
    });

    for input in inputs {
        let mut file =
            crate::path_safety::open_regular_member_no_follow(base, &input.declared_name)
                .map_err(|error| member_error(&input, error))?;
        let before = checked_member_metadata(&mut file, cue, &input)?;
        let size = before.len();
        total_size = total_size
            .checked_add(size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CUE graph size overflow"))?;
        if total_size > MAX_GRAPH_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CUE graph exceeds {MAX_GRAPH_BYTES} bytes: {}",
                    cue.display()
                ),
            ));
        }
        let name = input.declared_name.as_bytes();
        let name_length = u64::try_from(name.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "CUE file name is too long"))?
            .to_le_bytes();
        let size_bytes = size.to_le_bytes();
        graph_sha1.update(name_length);
        graph_sha1.update(name);
        graph_sha1.update(size_bytes);
        graph_sha256.update((name.len() as u64).to_le_bytes());
        graph_sha256.update(name);
        graph_sha256.update(size_bytes);

        let mut file_sha1 = Sha1::new();
        let mut file_sha256 = Sha256::new();
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
                    io::Error::new(io::ErrorKind::InvalidData, "CUE member size overflow")
                })?;
                let bytes = &buffer[..read];
                graph_sha1.update(bytes);
                graph_sha256.update(bytes);
                file_sha1.update(bytes);
                file_sha256.update(bytes);
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
                    "CUE referenced track changed while it was hashed: {:?}",
                    input.declared_name
                ),
            ));
        }
        files.push(CueFileIdentity {
            declared_name: input.declared_name,
            path: input.path,
            size,
            sha1: hex::encode(file_sha1.finalize()),
            sha256: hex::encode(file_sha256.finalize()),
        });
    }

    if read_entry(cue)? != cue_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "CUE entry file changed while its graph identity was established: {}",
                cue.display()
            ),
        ));
    }

    Ok(CueGraphIdentity {
        sha1: hex::encode(graph_sha1.finalize()),
        sha256: hex::encode(graph_sha256.finalize()),
        size: total_size,
        files,
    })
}

#[cfg(test)]
#[path = "cue_tests.rs"]
mod tests;
