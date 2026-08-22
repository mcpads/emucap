//! cdrdao TOC reference discovery with Mednafen-compatible track boundaries.

use std::io;
use std::path::Path;

use crate::media_graph::{MediaGraphIdentity, MediaReference};

const GRAPH_SHA256_DOMAIN: &[u8] = b"emucap-toc-graph-sha256\0";
const TRACK_TYPES: &[&str] = &[
    "AUDIO",
    "MODE1",
    "MODE1_RAW",
    "MODE2",
    "MODE2_FORM1",
    "MODE2_FORM2",
    "MODE2_RAW",
    "CDI_RAW",
];

pub fn references(path: &Path) -> io::Result<Vec<MediaReference>> {
    let bytes =
        crate::media_graph::read_descriptor(path, "TOC", crate::media_graph::MAX_DESCRIPTOR_BYTES)?;
    let text = crate::media_graph::descriptor_text(&bytes, "TOC")?;
    let mut references = Vec::new();
    let mut track_count = 0_u8;
    let mut current_track_has_file = false;
    for (line_index, source_line) in text.lines().enumerate() {
        let line = source_line
            .split_once("//")
            .map_or(source_line, |(line, _)| line)
            .trim();
        if line.is_empty() {
            continue;
        }
        let directive = line
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if !matches!(
            directive.as_str(),
            "TRACK" | "FILE" | "AUDIOFILE" | "DATAFILE"
        ) {
            continue;
        }
        let fields = crate::media_graph::quoted_fields(line, "TOC", line_index + 1, 5)?;
        if directive == "TRACK" {
            if track_count > 0 && !current_track_has_file {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("TOC track has no file before line {}", line_index + 1),
                ));
            }
            if !(2..=3).contains(&fields.len())
                || !TRACK_TYPES
                    .iter()
                    .any(|kind| fields[1].eq_ignore_ascii_case(kind))
                || (fields.len() == 3
                    && !matches!(fields[2].to_ascii_uppercase().as_str(), "RW" | "RW_RAW"))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "TOC TRACK directive is malformed at line {}",
                        line_index + 1
                    ),
                ));
            }
            track_count = track_count.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "TOC track count overflow")
            })?;
            if track_count > 99 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "TOC contains more than 99 tracks",
                ));
            }
            current_track_has_file = false;
            continue;
        }
        if track_count == 0 || fields.len() < 2 || fields.len() > 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("TOC file directive is malformed at line {}", line_index + 1),
            ));
        }
        if !crate::path_safety::is_portable_relative_member(&fields[1]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "TOC member path escapes or is not portable at line {}",
                    line_index + 1
                ),
            ));
        }
        if references.len() >= crate::media_graph::MAX_GRAPH_MEMBERS - 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "TOC contains too many file directives",
            ));
        }
        references.push(MediaReference {
            declared_name: fields[1].clone(),
        });
        current_track_has_file = true;
    }
    if track_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TOC contains no TRACK",
        ));
    }
    if !current_track_has_file {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TOC final track has no file",
        ));
    }
    Ok(references)
}

pub fn validate_graph(path: &Path) -> io::Result<Vec<MediaReference>> {
    let references = references(path)?;
    crate::media_graph::validate_references(path, "TOC", &references)?;
    Ok(references)
}

pub fn graph_identity(path: &Path) -> io::Result<MediaGraphIdentity> {
    let references = validate_graph(path)?;
    let identity = crate::media_graph::graph_identity(
        path,
        "TOC",
        "entry.toc",
        GRAPH_SHA256_DOMAIN,
        &references,
    )?;
    if validate_graph(path)? != references {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TOC references changed while its graph identity was established",
        ));
    }
    Ok(identity)
}

#[cfg(test)]
#[path = "toc_tests.rs"]
mod tests;
