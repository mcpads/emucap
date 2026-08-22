//! Bounded Mednafen M3U graph discovery.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::media_graph::{MediaGraphIdentity, MediaReference};

const GRAPH_SHA256_DOMAIN: &[u8] = b"emucap-m3u-graph-sha256\0";
const MAX_RECURSION_DEPTH: usize = 9;
const MAX_DISCS: usize = 25;

/// Parse one selected or already approved playlist without opening anything it names.
pub fn direct_entries(path: &Path) -> io::Result<Vec<String>> {
    let bytes =
        crate::media_graph::read_descriptor(path, "M3U", crate::media_graph::MAX_DESCRIPTOR_BYTES)?;
    let text = crate::media_graph::descriptor_text(&bytes, "M3U")?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut entries = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !crate::path_safety::is_portable_relative_member(line) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "M3U path escapes or is not portable at line {}",
                    line_index + 1
                ),
            ));
        }
        if entries.len() >= crate::media_graph::MAX_GRAPH_MEMBERS - 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "M3U contains too many entries",
            ));
        }
        entries.push(line.to_string());
    }
    if entries.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("M3U contains no disc entries: {}", path.display()),
        ));
    }
    Ok(entries)
}

struct Collector<'a> {
    root: &'a Path,
    active_playlists: HashSet<PathBuf>,
    references: Vec<MediaReference>,
    disc_count: usize,
}

impl Collector<'_> {
    fn add_reference(&mut self, declared_name: String) -> io::Result<()> {
        if self.references.len() >= crate::media_graph::MAX_GRAPH_MEMBERS - 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "M3U media graph contains too many files",
            ));
        }
        self.references.push(MediaReference { declared_name });
        Ok(())
    }

    fn collect_nested_playlist(&mut self, relative_playlist: &str, depth: usize) -> io::Result<()> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("M3U recursion exceeds {MAX_RECURSION_DEPTH}"),
            ));
        }
        let playlist = crate::path_safety::regular_member_path(self.root, relative_playlist)?;
        if !self.active_playlists.insert(playlist.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("M3U playlist cycle detected at {}", playlist.display()),
            ));
        }
        let result = self.collect_playlist_contents(&playlist, Path::new(relative_playlist), depth);
        self.active_playlists.remove(&playlist);
        result
    }

    fn collect_root(&mut self, entry: &Path) -> io::Result<()> {
        let canonical = entry.canonicalize()?;
        self.active_playlists.insert(canonical.clone());
        let result = self.collect_playlist_contents(entry, Path::new(""), 0);
        self.active_playlists.remove(&canonical);
        result
    }

    fn collect_playlist_contents(
        &mut self,
        playlist: &Path,
        relative_playlist: &Path,
        depth: usize,
    ) -> io::Result<()> {
        let bytes = crate::media_graph::read_descriptor(
            playlist,
            "M3U",
            crate::media_graph::MAX_DESCRIPTOR_BYTES,
        )?;
        let text = crate::media_graph::descriptor_text(&bytes, "M3U")?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let relative_parent = relative_playlist.parent().unwrap_or_else(|| Path::new(""));
        let mut found_entry = false;
        for (line_index, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            found_entry = true;
            if !crate::path_safety::is_portable_relative_member(line) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "M3U path escapes or is not portable at line {}",
                        line_index + 1
                    ),
                ));
            }
            let nested = relative_parent.join(line);
            let nested = nested.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "M3U member path is not UTF-8")
            })?;
            if !crate::path_safety::is_portable_relative_member(nested) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("M3U nested path is not portable at line {}", line_index + 1),
                ));
            }
            let nested_path = crate::path_safety::regular_member_path(self.root, nested)?;
            let extension = nested_path
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase);
            self.add_reference(nested.to_string())?;
            match extension.as_deref() {
                Some("m3u") => self.collect_nested_playlist(nested, depth + 1)?,
                Some("cue") => {
                    self.disc_count += 1;
                    self.add_descriptor_members(
                        nested,
                        crate::cue::validate_mednafen_graph(&nested_path)?,
                    )?;
                }
                Some("ccd") => {
                    self.disc_count += 1;
                    let direct = crate::ccd::validate_graph(&nested_path)?;
                    self.add_media_references(nested, direct)?;
                }
                Some("toc") => {
                    self.disc_count += 1;
                    let direct = crate::toc::validate_graph(&nested_path)?;
                    self.add_media_references(nested, direct)?;
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("M3U member must be CUE, CCD, TOC, or M3U: {nested:?}"),
                    ));
                }
            }
            if self.disc_count > MAX_DISCS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("M3U contains more than {MAX_DISCS} discs"),
                ));
            }
        }
        if !found_entry {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("M3U contains no disc entries: {}", playlist.display()),
            ));
        }
        Ok(())
    }

    fn add_descriptor_members(
        &mut self,
        descriptor: &str,
        direct: Vec<crate::cue::CueReference>,
    ) -> io::Result<()> {
        let parent = Path::new(descriptor)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        for reference in direct {
            let nested = parent.join(reference.declared_name);
            let nested = nested.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "M3U CUE member is not UTF-8")
            })?;
            self.add_reference(nested.to_string())?;
        }
        Ok(())
    }

    fn add_media_references(
        &mut self,
        descriptor: &str,
        direct: Vec<MediaReference>,
    ) -> io::Result<()> {
        let parent = Path::new(descriptor)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        for reference in direct {
            let nested = parent.join(reference.declared_name);
            let nested = nested.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "M3U media member is not UTF-8")
            })?;
            self.add_reference(nested.to_string())?;
        }
        Ok(())
    }
}

pub fn references(path: &Path) -> io::Result<Vec<MediaReference>> {
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let root_metadata = std::fs::symlink_metadata(root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "M3U entry directory must be a real directory",
        ));
    }
    let mut collector = Collector {
        root,
        active_playlists: HashSet::new(),
        references: Vec::new(),
        disc_count: 0,
    };
    collector.collect_root(path)?;
    Ok(collector.references)
}

pub fn validate_graph(path: &Path) -> io::Result<Vec<MediaReference>> {
    let references = references(path)?;
    crate::media_graph::validate_references(path, "M3U", &references)?;
    Ok(references)
}

pub fn graph_identity(path: &Path) -> io::Result<MediaGraphIdentity> {
    let references = validate_graph(path)?;
    let identity = crate::media_graph::graph_identity(
        path,
        "M3U",
        "entry.m3u",
        GRAPH_SHA256_DOMAIN,
        &references,
    )?;
    if validate_graph(path)? != references {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "M3U graph changed while its identity was established",
        ));
    }
    Ok(identity)
}

#[cfg(test)]
#[path = "m3u_tests.rs"]
mod tests;
