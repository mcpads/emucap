use std::path::{Path, PathBuf};

pub(super) fn ext_lower(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
}

fn read_prefix(path: &Path, max: usize) -> Option<Vec<u8>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0; max];
    let length = file.read(&mut buffer).ok()?;
    buffer.truncate(length);
    Some(buffer)
}

pub(super) fn read_iso9660_system_cnf(path: &Path) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    const SECTOR: u64 = 2048;
    let mut file = std::fs::File::open(path).ok()?;
    let mut descriptor = [0u8; SECTOR as usize];
    file.seek(SeekFrom::Start(16 * SECTOR)).ok()?;
    file.read_exact(&mut descriptor).ok()?;
    if descriptor[0] != 1 || descriptor.get(1..6) != Some(b"CD001") {
        return None;
    }

    let root = descriptor.get(156..)?;
    let record_length = *root.first()? as usize;
    let root = root.get(..record_length)?;
    let root_lba = u32::from_le_bytes(root.get(2..6)?.try_into().ok()?) as u64;
    let root_length = u32::from_le_bytes(root.get(10..14)?.try_into().ok()?) as usize;
    let root_length = root_length.min(1024 * 1024);
    let mut directory = vec![0u8; root_length];
    file.seek(SeekFrom::Start(root_lba.checked_mul(SECTOR)?))
        .ok()?;
    file.read_exact(&mut directory).ok()?;

    let mut offset = 0usize;
    while offset < directory.len() {
        let length = directory[offset] as usize;
        if length == 0 {
            offset = ((offset / SECTOR as usize) + 1) * SECTOR as usize;
            continue;
        }
        let record = directory.get(offset..offset.checked_add(length)?)?;
        let identifier_length = *record.get(32)? as usize;
        let identifier = record.get(33..33usize.checked_add(identifier_length)?)?;
        let name = String::from_utf8_lossy(identifier).to_ascii_uppercase();
        if name.trim_end_matches(";1") == "SYSTEM.CNF" {
            let lba = u32::from_le_bytes(record.get(2..6)?.try_into().ok()?) as u64;
            let length = u32::from_le_bytes(record.get(10..14)?.try_into().ok()?) as usize;
            let mut contents = vec![0u8; length.min(64 * 1024)];
            file.seek(SeekFrom::Start(lba.checked_mul(SECTOR)?)).ok()?;
            file.read_exact(&mut contents).ok()?;
            return Some(contents);
        }
        offset = offset.checked_add(length)?;
    }
    None
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle.iter())
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
    })
}

fn cue_file_refs(path: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if !trimmed.to_ascii_uppercase().starts_with("FILE ") {
                return None;
            }
            let rest = trimmed.get(5..)?.trim_start();
            let file_name = if let Some(after_quote) = rest.strip_prefix('"') {
                after_quote.split('"').next()
            } else {
                rest.split_whitespace().next()
            }?;
            Some(base.join(file_name))
        })
        .collect()
}

pub(super) fn content_markers(path: Option<&str>) -> serde_json::Value {
    let Some(path) = path else {
        return serde_json::json!({"available": false});
    };
    let path_ref = Path::new(path);
    let exists = path_ref.exists();
    let mut markers = Vec::new();
    let mut scanned_files = Vec::new();
    let mut candidates = Vec::new();

    if ext_lower(path).as_deref() == Some("cue") {
        candidates.extend(cue_file_refs(path_ref));
    } else {
        candidates.push(path_ref.to_path_buf());
    }

    for candidate in candidates.into_iter().take(4) {
        if let Some(bytes) = read_prefix(&candidate, 1024 * 1024) {
            scanned_files.push(candidate.display().to_string());
            if contains_ascii_case_insensitive(&bytes, b"PSP GAME") {
                markers.push("psp_game_marker");
            }
            if contains_ascii_case_insensitive(&bytes, b"SEGA SEGASATURN") {
                markers.push("sega_saturn_header");
            }
            if contains_ascii_case_insensitive(&bytes, b"PLAYSTATION") {
                markers.push("playstation_marker");
            }
            if contains_ascii_case_insensitive(&bytes, b"PC Engine") {
                markers.push("pc_engine_marker");
            }
            if contains_ascii_case_insensitive(&bytes, b"SEGA MEGA DRIVE")
                || contains_ascii_case_insensitive(&bytes, b"SEGA GENESIS")
            {
                markers.push("sega_megadrive_header");
            }
            if bytes.get(0x1c..0x20) == Some(&[0xc2, 0x33, 0x9f, 0x3d]) {
                markers.push("gamecube_disc_magic");
            }
            if bytes.get(0x18..0x1c) == Some(&[0x5d, 0x1c, 0x9e, 0xa3]) {
                markers.push("wii_disc_magic");
            }
        }
        if ext_lower(&candidate.to_string_lossy()).as_deref() == Some("iso") {
            if let Some(system_cnf) = read_iso9660_system_cnf(&candidate) {
                if contains_ascii_case_insensitive(&system_cnf, b"BOOT2") {
                    markers.push("ps2_system_cnf");
                } else if contains_ascii_case_insensitive(&system_cnf, b"BOOT") {
                    markers.push("psx_system_cnf");
                }
            }
        }
    }

    markers.sort_unstable();
    markers.dedup();
    serde_json::json!({
        "available": exists,
        "scanned_files": scanned_files,
        "markers": markers,
    })
}
