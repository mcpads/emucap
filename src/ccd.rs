//! Mednafen-compatible CloneCD graph discovery and validation.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use crate::media_graph::{MediaGraphIdentity, MediaReference};

const GRAPH_SHA256_DOMAIN: &[u8] = b"emucap-ccd-graph-sha256\0";
const MAX_CCD_SECTIONS: usize = 256;
const MAX_CCD_KEYS: usize = 4096;

fn companion_names(path: &Path) -> io::Result<(String, String)> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "CCD file stem is not portable UTF-8",
            )
        })?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CCD extension is not UTF-8"))?;
    if extension.len() != 3 || !extension.eq_ignore_ascii_case("ccd") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CloneCD entry must use a .ccd extension",
        ));
    }
    let mapped = |lower: &str| {
        lower
            .chars()
            .zip(extension.chars())
            .map(|(target, source)| {
                if source.is_ascii_uppercase() {
                    target.to_ascii_uppercase()
                } else {
                    target
                }
            })
            .collect::<String>()
    };
    Ok((
        format!("{stem}.{}", mapped("img")),
        format!("{stem}.{}", mapped("sub")),
    ))
}

fn parse_sections(path: &Path) -> io::Result<HashMap<String, HashMap<String, String>>> {
    let bytes =
        crate::media_graph::read_descriptor(path, "CCD", crate::media_graph::MAX_DESCRIPTOR_BYTES)?;
    let text = crate::media_graph::descriptor_text(&bytes, "CCD")?;
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = None;
    let mut key_count = 0_usize;
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            if section.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("CCD section is empty at line {}", line_index + 1),
                ));
            }
            let section = section.to_ascii_uppercase();
            if sections.contains_key(&section) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("CCD repeats section {section:?}"),
                ));
            }
            if sections.len() >= MAX_CCD_SECTIONS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "CCD contains too many sections",
                ));
            }
            sections.insert(section.clone(), HashMap::new());
            current = Some(section);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("CCD key/value is malformed at line {}", line_index + 1),
            ));
        };
        if value.contains('=') || key.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("CCD key/value is malformed at line {}", line_index + 1),
            ));
        }
        let section = current.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CCD value appears before a section at line {}",
                    line_index + 1
                ),
            )
        })?;
        key_count = key_count
            .checked_add(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CCD key count overflow"))?;
        if key_count > MAX_CCD_KEYS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CCD contains too many keys",
            ));
        }
        if sections
            .get_mut(section)
            .expect("current CCD section exists")
            .insert(key.trim().to_ascii_uppercase(), value.trim().to_string())
            .is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("CCD repeats a key at line {}", line_index + 1),
            ));
        }
    }
    Ok(sections)
}

fn required_value<'a>(section: &'a HashMap<String, String>, key: &str) -> io::Result<&'a str> {
    section
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("CCD is missing {key}")))
}

fn required_u32(section: &HashMap<String, String>, key: &str) -> io::Result<u32> {
    let value = required_value(section, key)?;
    let parsed = value
        .strip_prefix("0x")
        .map_or_else(|| value.parse::<u32>(), |hex| u32::from_str_radix(hex, 16));
    parsed.map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("CCD {key} is invalid")))
}

fn required_i32(section: &HashMap<String, String>, key: &str) -> io::Result<i32> {
    let value = required_value(section, key)?;
    let parsed = value
        .strip_prefix("0x")
        .map_or_else(|| value.parse::<i32>(), |hex| i32::from_str_radix(hex, 16));
    parsed.map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("CCD {key} is invalid")))
}

pub fn references(path: &Path) -> io::Result<Vec<MediaReference>> {
    let sections = parse_sections(path)?;
    let disc = sections
        .get("DISC")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CCD is missing [Disc]"))?;
    let entries = required_u32(disc, "TOCENTRIES")?;
    if !(3..=102).contains(&entries)
        || required_u32(disc, "SESSIONS")? != 1
        || required_u32(disc, "DATATRACKSSCRAMBLED")? != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CCD disc semantics are unsupported or malformed",
        ));
    }
    let present_entries = sections
        .keys()
        .filter_map(|name| name.strip_prefix("ENTRY "))
        .filter_map(|number| number.parse::<u32>().ok())
        .collect::<HashSet<_>>();
    if (0..entries).any(|entry| !present_entries.contains(&entry)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CCD does not contain every declared TOC entry",
        ));
    }
    for entry in 0..entries {
        let section = sections
            .get(&format!("ENTRY {entry}"))
            .expect("declared CCD entry section was checked above");
        let point = required_u32(section, "POINT")?;
        if required_u32(section, "SESSION")? != 1
            || required_u32(section, "ADR")? > u8::MAX.into()
            || required_u32(section, "CONTROL")? > u8::MAX.into()
            || required_u32(section, "PMIN")? > u8::MAX.into()
            || required_u32(section, "PSEC")? > u8::MAX.into()
            || !matches!(point, 1..=99 | 0xa0..=0xa2)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("CCD TOC entry {entry} is unsupported or malformed"),
            ));
        }
        required_i32(section, "PLBA")?;
    }
    let (image, subchannel) = companion_names(path)?;
    Ok(vec![
        MediaReference {
            declared_name: image,
        },
        MediaReference {
            declared_name: subchannel,
        },
    ])
}

pub fn validate_graph(path: &Path) -> io::Result<Vec<MediaReference>> {
    let references = references(path)?;
    crate::media_graph::validate_references(path, "CCD", &references)?;
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let image_size =
        crate::path_safety::open_regular_member_no_follow(root, &references[0].declared_name)?
            .metadata()?
            .len();
    if image_size > 0x7fff_ffff || image_size % 2352 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CCD IMG size must be at most 2 GiB and divisible by 2352",
        ));
    }
    let expected_subchannel = (image_size / 2352)
        .checked_mul(96)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CCD SUB size overflow"))?;
    if crate::path_safety::open_regular_member_no_follow(root, &references[1].declared_name)?
        .metadata()?
        .len()
        != expected_subchannel
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CCD SUB size does not match the IMG sector count",
        ));
    }
    Ok(references)
}

pub fn graph_identity(path: &Path) -> io::Result<MediaGraphIdentity> {
    let references = validate_graph(path)?;
    let identity = crate::media_graph::graph_identity(
        path,
        "CCD",
        "entry.ccd",
        GRAPH_SHA256_DOMAIN,
        &references,
    )?;
    if validate_graph(path)? != references {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CCD graph changed while its identity was established",
        ));
    }
    Ok(identity)
}

#[cfg(test)]
#[path = "ccd_tests.rs"]
mod tests;
