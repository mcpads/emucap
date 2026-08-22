//! Flycast-compatible GDI parsing with a closed referenced-track graph.

use std::io;
use std::path::Path;

use crate::media_graph::{MediaGraphIdentity, MediaReference};

const MAX_GDI_BYTES: u64 = 16 * 1024 - 1;
const GRAPH_SHA256_DOMAIN: &[u8] = b"emucap-gdi-graph-sha256\0";

#[derive(Debug, Clone, PartialEq, Eq)]
struct GdiTrack {
    reference: MediaReference,
    offset: u64,
    sector_size: u64,
}

fn parse_tracks(path: &Path) -> io::Result<Vec<GdiTrack>> {
    let bytes = crate::media_graph::read_descriptor(path, "GDI", MAX_GDI_BYTES)?;
    let text = crate::media_graph::descriptor_text(&bytes, "GDI")?;
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty());
    let Some((first_line_index, first_line)) = lines.next() else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "GDI is empty"));
    };
    let header =
        crate::media_graph::quoted_fields(first_line.trim(), "GDI", first_line_index + 1, 1)?;
    if header.len() != 1 || !header[0].bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GDI track count is malformed",
        ));
    }
    let track_count = header[0]
        .parse::<u32>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "GDI track count is invalid"))?;
    if !(3..=99).contains(&track_count) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GDI track count must be within 3..99",
        ));
    }

    let mut tracks = Vec::with_capacity(track_count as usize);
    for expected_track in 1..=track_count {
        let Some((line_index, line)) = lines.next() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GDI declares {track_count} tracks but ends early"),
            ));
        };
        let fields = crate::media_graph::quoted_fields(line.trim(), "GDI", line_index + 1, 6)?;
        if fields.len() != 6 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GDI track line {} must contain 6 fields", line_index + 1),
            ));
        }
        let parse_u32 = |index: usize, name: &str| -> io::Result<u32> {
            fields[index].parse::<u32>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("GDI {name} is invalid at line {}", line_index + 1),
                )
            })
        };
        let track = parse_u32(0, "track number")?;
        let first_sector = parse_u32(1, "first sector")?;
        let control = parse_u32(2, "control")?;
        let sector_size = parse_u32(3, "sector size")?;
        let offset = fields[5].parse::<i64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GDI offset is invalid at line {}", line_index + 1),
            )
        })?;
        if track != expected_track
            || !matches!(control, 0 | 4)
            || !matches!(sector_size, 2048 | 2352)
            || offset < 0
            || (track == 1 && control != 4)
            || (track == 2 && control != 0)
            || (track == 3 && (control != 4 || first_sector != 45_000))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GDI track semantics are invalid at line {}", line_index + 1),
            ));
        }
        if !crate::path_safety::is_portable_relative_member(&fields[4]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "GDI track path escapes or is not portable at line {}",
                    line_index + 1
                ),
            ));
        }
        tracks.push(GdiTrack {
            reference: MediaReference {
                declared_name: fields[4].clone(),
            },
            offset: offset as u64,
            sector_size: sector_size.into(),
        });
    }
    if lines.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GDI contains more track lines than its declared count",
        ));
    }
    Ok(tracks)
}

pub fn references(path: &Path) -> io::Result<Vec<MediaReference>> {
    Ok(parse_tracks(path)?
        .into_iter()
        .map(|track| track.reference)
        .collect())
}

pub fn validate_graph(path: &Path) -> io::Result<Vec<MediaReference>> {
    let tracks = parse_tracks(path)?;
    let references = tracks
        .iter()
        .map(|track| track.reference.clone())
        .collect::<Vec<_>>();
    crate::media_graph::validate_references(path, "GDI", &references)?;
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    for track in &tracks {
        let size = crate::path_safety::open_regular_member_no_follow(
            root,
            &track.reference.declared_name,
        )?
        .metadata()?
        .len();
        if track
            .offset
            .checked_add(track.sector_size)
            .is_none_or(|end| end > size)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "GDI track offset is outside its file: {:?}",
                    track.reference.declared_name
                ),
            ));
        }
    }
    Ok(references)
}

pub fn graph_identity(path: &Path) -> io::Result<MediaGraphIdentity> {
    let references = validate_graph(path)?;
    let identity = crate::media_graph::graph_identity(
        path,
        "GDI",
        "entry.gdi",
        GRAPH_SHA256_DOMAIN,
        &references,
    )?;
    if validate_graph(path)? != references {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GDI references changed while its graph identity was established",
        ));
    }
    Ok(identity)
}

#[cfg(test)]
#[path = "gdi_tests.rs"]
mod tests;
