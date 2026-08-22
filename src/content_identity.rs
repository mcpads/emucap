//! Exact byte identity for content made from more than one file.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io;
use std::path::Path;

const MAX_PERSISTED_IDENTITY_BYTES: usize = 96 * 1024;
const APPROVAL_BINDING_DOMAIN: &[u8] = b"emucap-indirect-media-approval\0";

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IndirectMediaApproval {
    /// Opaque binding for the selected entry path on this host.
    pub entry_binding: String,
    /// Adapter whose loader rules produced the declaration.
    pub adapter: String,
    /// Exact normalized cumulative set returned by launch_plan.
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndirectMediaMemberRole {
    Descriptor,
    Track,
    Companion,
    OptionalSidecar,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IndirectMediaMember {
    pub declared_name: String,
    pub role: IndirectMediaMemberRole,
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndirectMediaAdmission {
    NotRequired,
    Review {
        approval: IndirectMediaApproval,
        members: Vec<IndirectMediaMember>,
        newly_declared: Vec<String>,
    },
    Approved {
        approval: IndirectMediaApproval,
        members: Vec<IndirectMediaMember>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentIdentityScope {
    CueGraph,
    GdiGraph,
    CcdGraph,
    TocGraph,
    M3uGraph,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContentHashAlgorithm {
    Sha256,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContentMemberIdentity {
    pub declared_name: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContentIdentity {
    pub scope: ContentIdentityScope,
    pub algorithm: ContentHashAlgorithm,
    pub digest: String,
    pub size: u64,
    pub members: Vec<ContentMemberIdentity>,
}

impl ContentIdentity {
    pub fn tracking_id(&self) -> String {
        match self.algorithm {
            ContentHashAlgorithm::Sha256 => format!("sha256-{}", self.digest),
        }
    }

    pub fn summary_value(&self) -> serde_json::Value {
        serde_json::json!({
            "scope": self.scope,
            "algorithm": self.algorithm,
            "digest": self.digest,
            "size": self.size,
            "member_count": self.members.len(),
        })
    }
}

fn composite_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|value| matches!(value.as_str(), "cue" | "gdi" | "ccd" | "toc" | "m3u"))
}

fn entry_binding(path: &Path, adapter: &str) -> io::Result<String> {
    crate::path_safety::open_regular_file_no_follow(path)?;
    let canonical = path.canonicalize()?;
    let mut hash = Sha256::new();
    hash.update(APPROVAL_BINDING_DOMAIN);
    hash.update((adapter.len() as u64).to_le_bytes());
    hash.update(adapter.as_bytes());
    let bytes = path_bytes(&canonical);
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
    Ok(format!("sha256:{}", hex::encode(hash.finalize())))
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn add_declaration(
    declarations: &mut BTreeMap<String, IndirectMediaMember>,
    declared_name: String,
    role: IndirectMediaMemberRole,
    optional: bool,
) -> io::Result<()> {
    if !crate::path_safety::is_portable_relative_member(&declared_name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("indirect media member path is not portable: {declared_name:?}"),
        ));
    }
    declarations
        .entry(declared_name.clone())
        .and_modify(|member| {
            member.optional &= optional;
            if role == IndirectMediaMemberRole::Descriptor {
                member.role = IndirectMediaMemberRole::Descriptor;
            }
        })
        .or_insert(IndirectMediaMember {
            declared_name,
            role,
            optional,
        });
    if declarations.len() > crate::media_graph::MAX_GRAPH_MEMBERS - 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "indirect media declaration contains too many members",
        ));
    }
    let name_bytes = declarations
        .keys()
        .try_fold(0_usize, |total, name| total.checked_add(name.len()))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "indirect media member name size overflow",
            )
        })?;
    if name_bytes > crate::media_graph::MAX_DECLARED_NAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "indirect media declaration contains too much file-name data",
        ));
    }
    Ok(())
}

fn portable_join(parent_entry: &str, child: &str) -> io::Result<String> {
    if !crate::path_safety::is_portable_relative_member(child) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "indirect media member path is not portable",
        ));
    }
    let parent = parent_entry
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let joined = if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}/{child}")
    };
    if !crate::path_safety::is_portable_relative_member(&joined) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "nested indirect media member path is not portable",
        ));
    }
    Ok(joined)
}

fn add_cue_declarations(
    declarations: &mut BTreeMap<String, IndirectMediaMember>,
    descriptor_name: &str,
    descriptor_path: &Path,
    include_mednafen_sbi: bool,
) -> io::Result<()> {
    for reference in crate::cue::referenced_files(descriptor_path)? {
        add_declaration(
            declarations,
            portable_join(descriptor_name, &reference.declared_name)?,
            IndirectMediaMemberRole::Track,
            false,
        )?;
    }
    if include_mednafen_sbi {
        let reference = crate::cue::mednafen_sbi_reference(descriptor_path)?;
        add_declaration(
            declarations,
            portable_join(descriptor_name, &reference.declared_name)?,
            IndirectMediaMemberRole::OptionalSidecar,
            true,
        )?;
    }
    Ok(())
}

fn add_media_declarations(
    declarations: &mut BTreeMap<String, IndirectMediaMember>,
    descriptor_name: &str,
    references: Vec<crate::media_graph::MediaReference>,
    role: IndirectMediaMemberRole,
) -> io::Result<()> {
    for reference in references {
        add_declaration(
            declarations,
            portable_join(descriptor_name, &reference.declared_name)?,
            role.clone(),
            false,
        )?;
    }
    Ok(())
}

struct PlaylistDeclaration<'a> {
    root: &'a Path,
    adapter: &'a str,
    approved: &'a BTreeSet<String>,
    declarations: BTreeMap<String, IndirectMediaMember>,
    active: HashSet<String>,
    disc_count: usize,
}

impl PlaylistDeclaration<'_> {
    fn collect(&mut self, descriptor_name: &str, path: &Path, depth: usize) -> io::Result<()> {
        if depth > 9 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "M3U recursion exceeds 9",
            ));
        }
        let active_name = if descriptor_name.is_empty() {
            "<entry>".to_string()
        } else {
            descriptor_name.to_string()
        };
        if !self.active.insert(active_name.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("M3U playlist cycle detected at {active_name:?}"),
            ));
        }
        let result = self.collect_entries(descriptor_name, path, depth);
        self.active.remove(&active_name);
        result
    }

    fn collect_entries(
        &mut self,
        descriptor_name: &str,
        path: &Path,
        depth: usize,
    ) -> io::Result<()> {
        for child in crate::m3u::direct_entries(path)? {
            let nested = portable_join(descriptor_name, &child)?;
            let extension = Path::new(&nested)
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase);
            if !matches!(extension.as_deref(), Some("m3u" | "cue" | "ccd" | "toc")) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("M3U member must be CUE, CCD, TOC, or M3U: {nested:?}"),
                ));
            }
            add_declaration(
                &mut self.declarations,
                nested.clone(),
                IndirectMediaMemberRole::Descriptor,
                false,
            )?;
            if !self.approved.contains(&nested) {
                continue;
            }
            let nested_path = crate::path_safety::regular_member_path(self.root, &nested)?;
            match extension.as_deref() {
                Some("m3u") => self.collect(&nested, &nested_path, depth + 1)?,
                Some("cue") => {
                    self.disc_count += 1;
                    add_cue_declarations(
                        &mut self.declarations,
                        &nested,
                        &nested_path,
                        self.adapter == "mednafen",
                    )?;
                }
                Some("ccd") => {
                    self.disc_count += 1;
                    add_media_declarations(
                        &mut self.declarations,
                        &nested,
                        crate::ccd::references(&nested_path)?,
                        IndirectMediaMemberRole::Companion,
                    )?;
                }
                Some("toc") => {
                    self.disc_count += 1;
                    add_media_declarations(
                        &mut self.declarations,
                        &nested,
                        crate::toc::references(&nested_path)?,
                        IndirectMediaMemberRole::Track,
                    )?;
                }
                _ => unreachable!("M3U extension was checked above"),
            }
            if self.disc_count > 25 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "M3U contains more than 25 discs",
                ));
            }
        }
        Ok(())
    }
}

fn discover_indirect_members(
    path: &Path,
    adapter: &str,
    approved: &BTreeSet<String>,
) -> io::Result<Vec<IndirectMediaMember>> {
    let extension = composite_extension(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "content is not descriptor media",
        )
    })?;
    let mut declarations = BTreeMap::new();
    match extension.as_str() {
        "cue" => add_cue_declarations(&mut declarations, "", path, adapter == "mednafen")?,
        "gdi" => add_media_declarations(
            &mut declarations,
            "",
            crate::gdi::references(path)?,
            IndirectMediaMemberRole::Track,
        )?,
        "ccd" => add_media_declarations(
            &mut declarations,
            "",
            crate::ccd::references(path)?,
            IndirectMediaMemberRole::Companion,
        )?,
        "toc" => add_media_declarations(
            &mut declarations,
            "",
            crate::toc::references(path)?,
            IndirectMediaMemberRole::Track,
        )?,
        "m3u" => {
            let root = path.parent().unwrap_or_else(|| Path::new("."));
            let mut collector = PlaylistDeclaration {
                root,
                adapter,
                approved,
                declarations,
                active: HashSet::new(),
                disc_count: 0,
            };
            collector.collect("", path, 0)?;
            declarations = collector.declarations;
        }
        _ => unreachable!("composite extension was checked above"),
    }
    Ok(declarations.into_values().collect())
}

/// Discover only the member names authorized by the selected entry and earlier review frontiers.
pub fn inspect_indirect_media(
    path: &Path,
    adapter: &str,
    approval: Option<&IndirectMediaApproval>,
) -> io::Result<IndirectMediaAdmission> {
    if composite_extension(path).is_none() {
        return Ok(IndirectMediaAdmission::NotRequired);
    }
    let binding = entry_binding(path, adapter)?;
    let approved = match approval {
        Some(approval) => {
            if approval.entry_binding != binding || approval.adapter != adapter {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "indirect media approval belongs to another entry or adapter",
                ));
            }
            let approval_name_bytes = approval
                .members
                .iter()
                .try_fold(0_usize, |total, name| total.checked_add(name.len()))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "indirect media approval name size overflow",
                    )
                })?;
            if approval.members.len() > crate::media_graph::MAX_GRAPH_MEMBERS - 1
                || approval_name_bytes > crate::media_graph::MAX_DECLARED_NAME_BYTES
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "indirect media approval exceeds its member bounds",
                ));
            }
            let set = approval.members.iter().cloned().collect::<BTreeSet<_>>();
            if set.len() != approval.members.len()
                || approval
                    .members
                    .iter()
                    .any(|name| !crate::path_safety::is_portable_relative_member(name))
                || approval.members.iter().ne(set.iter())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "indirect media approval members are not the exact normalized set",
                ));
            }
            set
        }
        None => BTreeSet::new(),
    };
    let members = discover_indirect_members(path, adapter, &approved)?;
    let discovered = members
        .iter()
        .map(|member| member.declared_name.clone())
        .collect::<BTreeSet<_>>();
    let stale = approved.difference(&discovered).next().is_some();
    if stale {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "indirect media declaration changed after approval",
        ));
    }
    let newly_declared = discovered
        .difference(&approved)
        .cloned()
        .collect::<Vec<_>>();
    let next_approval = IndirectMediaApproval {
        entry_binding: binding,
        adapter: adapter.to_string(),
        members: discovered.into_iter().collect(),
    };
    if !newly_declared.is_empty() || approval.is_none() {
        return Ok(IndirectMediaAdmission::Review {
            approval: next_approval,
            members,
            newly_declared,
        });
    }
    Ok(IndirectMediaAdmission::Approved {
        approval: next_approval,
        members,
    })
}

pub fn validate_approved_composite_content_for_adapter(
    path: &Path,
    adapter: &str,
    approval: Option<&IndirectMediaApproval>,
) -> io::Result<bool> {
    match inspect_indirect_media(path, adapter, approval)? {
        IndirectMediaAdmission::NotRequired => Ok(false),
        IndirectMediaAdmission::Review { .. } => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "indirect media members require review before validation",
        )),
        IndirectMediaAdmission::Approved { .. } => {
            validate_composite_content_for_adapter(path, Some(adapter))
        }
    }
}

pub fn identify_approved_composite_content_for_adapter(
    path: &Path,
    adapter: &str,
    approval: Option<&IndirectMediaApproval>,
) -> io::Result<Option<ContentIdentity>> {
    match inspect_indirect_media(path, adapter, approval)? {
        IndirectMediaAdmission::NotRequired => Ok(None),
        IndirectMediaAdmission::Review { .. } => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "indirect media members require review before hashing",
        )),
        IndirectMediaAdmission::Approved { .. } => {
            identify_composite_content_for_adapter(path, Some(adapter))
        }
    }
}

/// Hash a supported multi-file entry graph. `None` means the content is not a composite format;
/// malformed or unsafe supported graphs fail instead of degrading to entry-file identity.
pub fn identify_composite_content(path: &Path) -> io::Result<Option<ContentIdentity>> {
    identify_composite_content_for_adapter(path, None)
}

/// Hash the files consumed by the selected adapter. Loader-specific implicit files are included
/// only when that adapter actually opens them.
pub fn identify_composite_content_for_adapter(
    path: &Path,
    adapter: Option<&str>,
) -> io::Result<Option<ContentIdentity>> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    let identity = match extension.as_deref() {
        Some("cue") => {
            let graph = if adapter == Some("mednafen") {
                crate::cue::mednafen_graph_identity(path)?
            } else {
                crate::cue::graph_identity(path)?
            };
            ContentIdentity {
                scope: ContentIdentityScope::CueGraph,
                algorithm: ContentHashAlgorithm::Sha256,
                digest: graph.sha256,
                size: graph.size,
                members: graph
                    .files
                    .into_iter()
                    .map(|file| ContentMemberIdentity {
                        declared_name: file.declared_name,
                        size: file.size,
                        digest: file.sha256,
                    })
                    .collect(),
            }
        }
        Some("gdi") => from_media_graph(
            ContentIdentityScope::GdiGraph,
            crate::gdi::graph_identity(path)?,
        ),
        Some("ccd") => from_media_graph(
            ContentIdentityScope::CcdGraph,
            crate::ccd::graph_identity(path)?,
        ),
        Some("toc") => from_media_graph(
            ContentIdentityScope::TocGraph,
            crate::toc::graph_identity(path)?,
        ),
        Some("m3u") => from_media_graph(
            ContentIdentityScope::M3uGraph,
            crate::m3u::graph_identity(path)?,
        ),
        _ => return Ok(None),
    };
    let encoded = serde_json::to_vec(&identity)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if encoded.len() > MAX_PERSISTED_IDENTITY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "composite content identity is too large to persist safely",
        ));
    }
    Ok(Some(identity))
}

fn from_media_graph(
    scope: ContentIdentityScope,
    graph: crate::media_graph::MediaGraphIdentity,
) -> ContentIdentity {
    ContentIdentity {
        scope,
        algorithm: ContentHashAlgorithm::Sha256,
        digest: graph.sha256,
        size: graph.size,
        members: graph
            .files
            .into_iter()
            .map(|file| ContentMemberIdentity {
                declared_name: file.declared_name,
                size: file.size,
                digest: file.sha256,
            })
            .collect(),
    }
}

/// Validate a supported descriptor graph without hashing all media bytes. `false` means the entry
/// is a single-file format whose identity remains adapter-owned.
pub fn validate_composite_content(path: &Path) -> io::Result<bool> {
    validate_composite_content_for_adapter(path, None)
}

pub fn validate_composite_content_for_adapter(
    path: &Path,
    adapter: Option<&str>,
) -> io::Result<bool> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("cue") if adapter == Some("mednafen") => {
            crate::cue::validate_mednafen_graph(path).map(|_| true)
        }
        Some("cue") => crate::cue::validate_graph(path).map(|_| true),
        Some("gdi") => crate::gdi::validate_graph(path).map(|_| true),
        Some("ccd") => crate::ccd::validate_graph(path).map(|_| true),
        Some("toc") => crate::toc::validate_graph(path).map(|_| true),
        Some("m3u") => crate::m3u::validate_graph(path).map(|_| true),
        _ => Ok(false),
    }
}

#[cfg(test)]
#[path = "content_identity_tests.rs"]
mod tests;
