use serde::{Deserialize, Serialize};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::profile::classify_media_extension;
use super::{MediaKind, OpenMsxProfile};

const MAX_FIRMWARE_FILES: usize = 4_096;
const MAX_FIRMWARE_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FIRMWARE_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MEDIA_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirmwareRequirement {
    pub canonical_name: &'static str,
    pub accepted_sha1: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedFirmware {
    pub canonical_name: String,
    pub source: PathBuf,
    pub sha1: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedFirmware {
    pub canonical_name: String,
    pub sha1: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedMedia {
    pub kind: MediaKind,
    pub source_path: PathBuf,
    pub source_sha1: String,
    pub source_size: u64,
    pub mounted_path: PathBuf,
    pub mounted_sha1: String,
    pub source_writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedSession {
    pub system: String,
    pub machine: String,
    pub machine_type: String,
    pub user_data: PathBuf,
    pub firmware_manifest_sha256: Option<String>,
    pub firmware: Vec<StagedFirmware>,
    pub media: PreparedMedia,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSessionPaths {
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub session: PreparedSession,
}

impl PreparedSession {
    pub fn verify(&self) -> io::Result<OpenMsxProfile> {
        let profile = OpenMsxProfile::for_system(&self.system).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown openMSX session system: {}", self.system),
            )
        })?;
        if self.machine != profile.machine() || self.machine_type != profile.machine_type() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX session machine identity does not match its system profile",
            ));
        }
        if !profile.supports(self.media.kind) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX session media kind is not admitted by its profile",
            ));
        }
        if !self.user_data.is_absolute() || !self.user_data.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX session user-data directory is missing or not absolute",
            ));
        }
        if !self.media.source_path.is_absolute() || !self.media.mounted_path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX source and mounted media paths must be absolute",
            ));
        }
        let classified = validate_content_for_profile(profile, &self.media.source_path)?;
        if classified != self.media.kind {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX source media extension disagrees with the session kind",
            ));
        }
        let (source_sha1, source_size) = hash_file(&self.media.source_path, MAX_MEDIA_BYTES)?;
        if source_sha1 != self.media.source_sha1 || source_size != self.media.source_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX source media identity changed after admission",
            ));
        }
        let (mounted_sha1, mounted_size) = hash_file(&self.media.mounted_path, MAX_MEDIA_BYTES)?;
        if mounted_sha1 != self.media.mounted_sha1
            || mounted_sha1 != source_sha1
            || mounted_size != source_size
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX mounted media does not match the admitted source identity",
            ));
        }
        let session_root = self.user_data.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX user-data directory has no session root",
            )
        })?;
        if self.media.kind == MediaKind::Cartridge {
            if self.media.mounted_path != self.media.source_path {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "openMSX cartridge session unexpectedly changed the mounted path",
                ));
            }
        } else if self.media.mounted_path == self.media.source_path
            || !self.media.mounted_path.starts_with(session_root)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX mutable media must use a session-owned working copy",
            ));
        }

        let requirements = firmware_requirements(profile);
        if self.firmware.len() != requirements.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX staged firmware set is incomplete",
            ));
        }
        for (staged, requirement) in self.firmware.iter().zip(&requirements) {
            if staged.canonical_name != requirement.canonical_name
                || !requirement.accepted_sha1.contains(&staged.sha1)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "openMSX staged firmware identity does not match the pinned manifest",
                ));
            }
            let path = self
                .user_data
                .join("systemroms")
                .join(&staged.canonical_name);
            let (sha1, size) = hash_file(&path, MAX_FIRMWARE_FILE_BYTES)?;
            if sha1 != staged.sha1 || size != staged.size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("openMSX staged firmware changed: {}", path.display()),
                ));
            }
        }
        let expected_digest = (!self.firmware.is_empty()).then(|| firmware_digest(&self.firmware));
        if self.firmware_manifest_sha256 != expected_digest {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "openMSX firmware manifest digest is invalid",
            ));
        }
        Ok(profile)
    }
}

fn accepted(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

pub(crate) fn firmware_requirements(profile: OpenMsxProfile) -> Vec<FirmwareRequirement> {
    match profile {
        OpenMsxProfile::CbiosMsx2p => Vec::new(),
        OpenMsxProfile::Msx1 => vec![FirmwareRequirement {
            canonical_name: "vg8020_basic-bios1.rom",
            accepted_sha1: accepted(&["829c00c3114f25b3dae5157c0a238b52a3ac37db"]),
        }],
        OpenMsxProfile::Msx2 => vec![
            FirmwareRequirement {
                canonical_name: "nms8250_basic-bios2.rom",
                accepted_sha1: accepted(&["6103b39f1e38d1aa2d84b1c3219c44f1abb5436e"]),
            },
            FirmwareRequirement {
                canonical_name: "nms8250_msx2sub.rom",
                accepted_sha1: accepted(&["5c1f9c7fb655e43d38e5dd1fcc6b942b2ff68b02"]),
            },
            FirmwareRequirement {
                canonical_name: "nms8250_disk.rom",
                accepted_sha1: accepted(&[
                    "dab3e6f36843392665b71b04178aadd8762c6589",
                    "c3efedda7ab947a06d9345f7b8261076fa7ceeef",
                    "8625c6b633d9cca2875e4dc33404fb98653379d7",
                ]),
            },
        ],
        OpenMsxProfile::Msx2p => vec![
            FirmwareRequirement {
                canonical_name: "fs-a1wsx_basic-bios2p.rom",
                accepted_sha1: accepted(&["f4433752d3bf876bfefb363c749d4d2e08a218b6"]),
            },
            FirmwareRequirement {
                canonical_name: "fs-a1wsx_msx2psub.rom",
                accepted_sha1: accepted(&["fe0254cbfc11405b79e7c86c7769bd6322b04995"]),
            },
            FirmwareRequirement {
                canonical_name: "fs-a1wsx_fmbasic.rom",
                accepted_sha1: accepted(&[
                    "aad42ba4289b33d8eed225d42cea930b7fc5c228",
                    "6354ccc5c100b1c558c9395fa8c00784d2e9b0a3",
                ]),
            },
            FirmwareRequirement {
                canonical_name: "fs-a1wsx_kanjibasic.rom",
                accepted_sha1: accepted(&["dcc3a67732aa01c4f2ee8d1ad886444a4dbafe06"]),
            },
            FirmwareRequirement {
                canonical_name: "fs-a1wsx_kanjifont.rom",
                accepted_sha1: accepted(&["5aff2d9b6efc723bc395b0f96f0adfa83cc54a49"]),
            },
            FirmwareRequirement {
                canonical_name: "fs-a1wsx_disk.rom",
                accepted_sha1: accepted(&["7ed7c55e0359737ac5e68d38cb6903f9e5d7c2b6"]),
            },
            FirmwareRequirement {
                canonical_name: "fs-a1wsx_firmware.rom",
                accepted_sha1: accepted(&["3330d9b6b76e3c4ccb7cf252496ed15d08b95d3f"]),
            },
        ],
        OpenMsxProfile::MsxTurboR => vec![
            FirmwareRequirement {
                canonical_name: "fs-a1gt_firmware.rom",
                accepted_sha1: accepted(&[
                    "e779c338eb91a7dea3ff75f3fde76b8af22c4a3a",
                    "5fa3aa79aeba2c0441f349e78e9a16d9d64422ea",
                ]),
            },
            FirmwareRequirement {
                canonical_name: "fs-a1gt_kanjifont.rom",
                accepted_sha1: accepted(&["5aff2d9b6efc723bc395b0f96f0adfa83cc54a49"]),
            },
        ],
    }
}

pub fn validate_content_for_profile(
    profile: OpenMsxProfile,
    content: &Path,
) -> io::Result<MediaKind> {
    if !content.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MSX content path must be absolute",
        ));
    }
    if !content.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("MSX content not found: {}", content.display()),
        ));
    }
    let extension = content
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "MSX content has no extension")
        })?;
    let kind = classify_media_extension(extension).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported MSX media extension: .{extension}"),
        )
    })?;
    if !profile.supports(kind) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} does not support {} media",
                profile.system(),
                media_name(kind)
            ),
        ));
    }
    Ok(kind)
}

pub(crate) fn validate_firmware_root(root: &Path) -> io::Result<()> {
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "EMUCAP_OPENMSX_FIRMWARE must be an absolute directory",
        ));
    }
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("openMSX firmware directory not found: {}", root.display()),
        ));
    }
    Ok(())
}

fn hash_file(path: &Path, limit: u64) -> io::Result<(String, u64)> {
    let size = fs::metadata(path)?.len();
    if size > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "file exceeds the {limit}-byte identity bound: {}",
                path.display()
            ),
        ));
    }
    let mut file = File::open(path)?;
    let mut digest = Sha1::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut read = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "file crossed the {limit}-byte identity bound: {}",
                    path.display()
                ),
            ));
        }
        digest.update(&buffer[..count]);
    }
    Ok((hex::encode(digest.finalize()), read))
}

fn inventory_paths(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
                if files.len() > MAX_FIRMWARE_FILES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("openMSX firmware inventory exceeds {MAX_FIRMWARE_FILES} files"),
                    ));
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) fn resolve_firmware_inventory(
    root: &Path,
    requirements: &[FirmwareRequirement],
) -> io::Result<Vec<ResolvedFirmware>> {
    validate_firmware_root(root)?;
    let wanted = requirements
        .iter()
        .flat_map(|requirement| requirement.accepted_sha1.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut by_hash = BTreeMap::<String, Vec<(PathBuf, u64)>>::new();
    let mut total = 0_u64;
    for path in inventory_paths(root)? {
        let size = fs::metadata(&path)?.len();
        if size > MAX_FIRMWARE_FILE_BYTES {
            continue;
        }
        total = total.checked_add(size).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "firmware inventory size overflow",
            )
        })?;
        if total > MAX_FIRMWARE_TOTAL_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "openMSX firmware inventory exceeds {MAX_FIRMWARE_TOTAL_BYTES} hashed bytes"
                ),
            ));
        }
        let (sha1, actual_size) = hash_file(&path, MAX_FIRMWARE_FILE_BYTES)?;
        if wanted.contains(&sha1) {
            by_hash.entry(sha1).or_default().push((path, actual_size));
        }
    }

    requirements
        .iter()
        .map(|requirement| {
            let identities = requirement
                .accepted_sha1
                .iter()
                .filter(|sha1| by_hash.contains_key(*sha1))
                .collect::<Vec<_>>();
            if identities.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "required openMSX firmware {} was not found with an accepted SHA-1",
                        requirement.canonical_name
                    ),
                ));
            }
            if identities.len() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "multiple accepted firmware identities exist for {}; select a narrower firmware root",
                        requirement.canonical_name
                    ),
                ));
            }
            let sha1 = (*identities[0]).clone();
            let candidates = &by_hash[&sha1];
            let (source, size) = candidates
                .first()
                .expect("a resolved firmware identity has a source");
            Ok(ResolvedFirmware {
                canonical_name: requirement.canonical_name.to_owned(),
                source: source.clone(),
                sha1,
                size: *size,
            })
        })
        .collect()
}

pub(crate) fn prepare_media(
    kind: MediaKind,
    source: &Path,
    working: &Path,
) -> io::Result<PreparedMedia> {
    let (source_sha1, source_size) = hash_file(source, MAX_MEDIA_BYTES)?;
    if kind == MediaKind::Cartridge {
        return Ok(PreparedMedia {
            kind,
            source_path: source.to_path_buf(),
            source_sha1: source_sha1.clone(),
            source_size,
            mounted_path: source.to_path_buf(),
            mounted_sha1: source_sha1,
            source_writable: false,
        });
    }
    if let Some(parent) = working.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::path_safety::atomic_copy_file(source, working)?;
    let (mounted_sha1, mounted_size) = hash_file(working, MAX_MEDIA_BYTES)?;
    if source_sha1 != mounted_sha1 || source_size != mounted_size {
        return Err(io::Error::other(
            "MSX working media copy did not preserve source identity",
        ));
    }
    Ok(PreparedMedia {
        kind,
        source_path: source.to_path_buf(),
        source_sha1,
        source_size,
        mounted_path: working.to_path_buf(),
        mounted_sha1,
        source_writable: false,
    })
}

fn firmware_digest(files: &[StagedFirmware]) -> String {
    let mut digest = Sha256::new();
    sha2::Digest::update(&mut digest, b"emucap-openmsx-firmware-v1\0");
    for file in files {
        sha2::Digest::update(&mut digest, file.canonical_name.as_bytes());
        sha2::Digest::update(&mut digest, b"\0");
        sha2::Digest::update(&mut digest, file.sha1.as_bytes());
        sha2::Digest::update(&mut digest, b"\0");
        sha2::Digest::update(&mut digest, file.size.to_le_bytes());
    }
    hex::encode(digest.finalize())
}

pub fn prepare_session(
    profile: OpenMsxProfile,
    content: &Path,
    runtime_home: &Path,
    generation_key: &str,
    firmware_root: Option<&Path>,
) -> io::Result<PreparedSessionPaths> {
    let media_kind = validate_content_for_profile(profile, content)?;
    let key = hex::encode(Sha256::digest(generation_key.as_bytes()));
    let generations = runtime_home.join("generations");
    let root = generations.join(&key[..24]);
    let temporary = generations.join(format!(
        ".prepare-{}-{}",
        &key[..24],
        ulid::Ulid::generate().to_string().to_ascii_lowercase()
    ));
    match fs::symlink_metadata(&root) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("openMSX generation already exists: {}", root.display()),
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::create_dir_all(&generations)?;
    let generations_metadata = fs::symlink_metadata(&generations)?;
    if generations_metadata.file_type().is_symlink() || !generations_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "openMSX generations path is not a real directory",
        ));
    }
    fs::create_dir(&temporary)?;

    let result = (|| {
        let final_user_data = root.join("share");
        let temporary_user_data = temporary.join("share");
        fs::create_dir_all(temporary_user_data.join("systemroms"))?;

        let resolved = if profile.uses_real_firmware() {
            let root = firmware_root.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} requires operator-supplied firmware", profile.system()),
                )
            })?;
            resolve_firmware_inventory(root, &firmware_requirements(profile))?
        } else {
            Vec::new()
        };
        let mut staged = Vec::with_capacity(resolved.len());
        for firmware in resolved {
            let destination = temporary_user_data
                .join("systemroms")
                .join(&firmware.canonical_name);
            crate::path_safety::atomic_copy_file(&firmware.source, &destination)?;
            let (sha1, size) = hash_file(&destination, MAX_FIRMWARE_FILE_BYTES)?;
            if sha1 != firmware.sha1 || size != firmware.size {
                return Err(io::Error::other(format!(
                    "staged firmware identity changed for {}",
                    firmware.canonical_name
                )));
            }
            staged.push(StagedFirmware {
                canonical_name: firmware.canonical_name,
                sha1,
                size,
            });
        }

        let media_name = content
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("content.bin");
        let final_media = root.join("media").join(media_name);
        let temporary_media = temporary.join("media").join(media_name);
        let mut media = prepare_media(media_kind, content, &temporary_media)?;
        if media_kind != MediaKind::Cartridge {
            media.mounted_path = final_media;
        }
        let session = PreparedSession {
            system: profile.system().to_owned(),
            machine: profile.machine().to_owned(),
            machine_type: profile.machine_type().to_owned(),
            user_data: final_user_data,
            firmware_manifest_sha256: (!staged.is_empty()).then(|| firmware_digest(&staged)),
            firmware: staged,
            media,
        };
        let manifest = temporary.join("session.json");
        fs::write(&manifest, serde_json::to_vec_pretty(&session)?)?;
        fs::create_dir_all(&generations)?;
        fs::rename(&temporary, &root)?;
        Ok(PreparedSessionPaths {
            root: root.clone(),
            manifest: root.join("session.json"),
            session,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn media_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Cartridge => "cartridge",
        MediaKind::Disk => "disk",
        MediaKind::Cassette => "cassette",
    }
}
