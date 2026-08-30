use super::*;

use std::fs;
use std::path::{Component, Path};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) const MAX_STATE_CONTAINER_BYTES: u64 = 128 * 1024;
const EEPROM_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StateFileIdentity {
    pub(super) path: String,
    pub(super) size: u64,
    pub(super) sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaFileObservation {
    size: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::xemu_bridge) struct MediaIdentityCache {
    path: PathBuf,
    observation: MediaFileObservation,
    identity: StateFileIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StateStorage {
    pub(super) snapshot_tag: String,
    pub(super) hdd_path: String,
    pub(super) hdd_node: String,
    pub(super) eeprom_path: String,
    pub(super) eeprom_sha256: String,
    pub(super) eeprom_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateControllerTopology {
    port: u8,
    binding: String,
    semantics: String,
}

impl Default for StateControllerTopology {
    fn default() -> Self {
        Self {
            port: 0,
            binding: "managed-keyboard-xid".into(),
            semantics: "complete-persistent-state".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct XemuStateContainer {
    pub(super) format: String,
    pub(super) launch_id: String,
    pub(super) server_build: String,
    pub(super) host_build: XemuHostBuildIdentity,
    pub(super) machine_inputs: XemuMachineIdentity,
    pub(super) storage: StateStorage,
    pub(super) media: Option<StateFileIdentity>,
    controller: StateControllerTopology,
}

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn require_complete_machine_identity(&self) -> XemuResult<()> {
        if self.machine_identity.mcpx_sha256.is_none()
            || self.machine_identity.flash_sha256.is_none()
            || self.machine_identity.hdd_template_sha256.is_none()
            || self.machine_identity.eeprom_initial_sha256.is_none()
        {
            return Err(XemuBridgeError::BadState(
                "Xbox state requires a complete managed machine identity".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn state_path(&self, params: &Value) -> XemuResult<PathBuf> {
        let path = PathBuf::from(required_str(params, "path")?);
        if !path.is_absolute() {
            return Err(XemuBridgeError::BadParams(
                "Xbox state path must be absolute".into(),
            ));
        }
        if path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(XemuBridgeError::BadParams(
                "Xbox state path must not contain dot or parent components".into(),
            ));
        }
        for protected in [
            Some(self.state_environment.hdd.as_path()),
            Some(self.state_environment.eeprom.as_path()),
            self.current_disc.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if path == protected {
                return Err(XemuBridgeError::BadParams(format!(
                    "Xbox state path must not replace managed machine or media file {}",
                    protected.display()
                )));
            }
            if let (Ok(left), Ok(right)) = (fs::canonicalize(&path), fs::canonicalize(protected)) {
                if left == right {
                    return Err(XemuBridgeError::BadParams(format!(
                        "Xbox state path aliases managed machine or media file {}",
                        protected.display()
                    )));
                }
            }
        }
        Ok(path)
    }

    pub(super) fn state_container(
        &self,
        snapshot_tag: String,
        layout: &BlockLayout,
        media: Option<StateFileIdentity>,
        eeprom: &[u8],
    ) -> XemuResult<XemuStateContainer> {
        Ok(XemuStateContainer {
            format: STATE_FORMAT.into(),
            launch_id: self
                .env
                .launch_id
                .clone()
                .ok_or_else(|| XemuBridgeError::BadState("managed launch_id is missing".into()))?,
            server_build: self.env.build.clone().ok_or_else(|| {
                XemuBridgeError::BadState("managed server build is missing".into())
            })?,
            host_build: self.state_environment.host_build.clone(),
            machine_inputs: self.machine_identity.clone(),
            storage: StateStorage {
                snapshot_tag,
                hdd_path: self.state_environment.hdd.display().to_string(),
                hdd_node: layout.hdd_node.clone(),
                eeprom_path: self.state_environment.eeprom.display().to_string(),
                eeprom_sha256: hex::encode(Sha256::digest(eeprom)),
                eeprom_hex: hex::encode(eeprom),
            },
            media,
            controller: StateControllerTopology::default(),
        })
    }

    pub(super) fn validate_container(
        &mut self,
        container: &XemuStateContainer,
        layout: &BlockLayout,
    ) -> XemuResult<Vec<u8>> {
        if container.format != STATE_FORMAT {
            return Err(XemuBridgeError::BadParams(format!(
                "unsupported Xbox state format: {}",
                container.format
            )));
        }
        if Some(container.launch_id.as_str()) != self.env.launch_id.as_deref() {
            return Err(XemuBridgeError::BadState(
                "Xbox state belongs to a different managed generation".into(),
            ));
        }
        if Some(container.server_build.as_str()) != self.env.build.as_deref()
            || container.host_build != self.state_environment.host_build
            || container.machine_inputs != self.machine_identity
        {
            return Err(XemuBridgeError::BadState(
                "Xbox state build or machine identity does not match this generation".into(),
            ));
        }
        if container.storage.hdd_path != self.state_environment.hdd.display().to_string()
            || container.storage.hdd_node != layout.hdd_node
            || container.storage.eeprom_path != self.state_environment.eeprom.display().to_string()
            || container.controller != StateControllerTopology::default()
        {
            return Err(XemuBridgeError::BadState(
                "Xbox state storage or controller topology does not match this generation".into(),
            ));
        }
        if !crate::path_safety::is_hyphenated_ascii_id(&container.storage.snapshot_tag, 96) {
            return Err(XemuBridgeError::BadParams(
                "Xbox state contains an invalid internal snapshot identifier".into(),
            ));
        }
        let eeprom = hex::decode(&container.storage.eeprom_hex).map_err(|_| {
            XemuBridgeError::BadParams("Xbox state contains invalid EEPROM bytes".into())
        })?;
        if eeprom.len() != EEPROM_BYTES
            || hex::encode(Sha256::digest(&eeprom)) != container.storage.eeprom_sha256
        {
            return Err(XemuBridgeError::BadParams(
                "Xbox state EEPROM length or digest is invalid".into(),
            ));
        }
        if self.current_media_identity()? != container.media {
            return Err(XemuBridgeError::BadState(
                "Xbox state requires the exact disc that was mounted when it was saved; change_media while frozen before retrying"
                    .into(),
            ));
        }
        Ok(eeprom)
    }

    pub(super) fn read_replaced_container(
        &self,
        path: &Path,
        layout: &BlockLayout,
    ) -> XemuResult<Option<(XemuStateContainer, Vec<u8>)>> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
            Ok(_) => {
                let bytes = crate::path_safety::read_bounded_regular_file_no_follow(
                    path,
                    MAX_STATE_CONTAINER_BYTES,
                )?;
                let container: XemuStateContainer =
                    serde_json::from_slice(&bytes).map_err(|_| {
                        XemuBridgeError::BadParams(format!(
                            "refusing to overwrite a file that is not an Xbox state container: {}",
                            path.display()
                        ))
                    })?;
                if container.format != STATE_FORMAT
                    || !crate::path_safety::is_hyphenated_ascii_id(
                        &container.storage.snapshot_tag,
                        96,
                    )
                {
                    return Err(XemuBridgeError::BadParams(format!(
                        "refusing to overwrite an invalid Xbox state container: {}",
                        path.display()
                    )));
                }
                if container.launch_id == self.env.launch_id.as_deref().unwrap_or_default()
                    && (container.storage.hdd_path
                        != self.state_environment.hdd.display().to_string()
                        || container.storage.hdd_node != layout.hdd_node)
                {
                    return Err(XemuBridgeError::BadState(
                        "existing Xbox state claims this generation with a different HDD binding"
                            .into(),
                    ));
                }
                Ok(Some((container, bytes)))
            }
        }
    }

    pub(super) fn read_eeprom(&self) -> XemuResult<Vec<u8>> {
        let bytes = crate::path_safety::read_bounded_regular_file_no_follow(
            &self.state_environment.eeprom,
            EEPROM_BYTES as u64,
        )?;
        if bytes.len() != EEPROM_BYTES {
            return Err(XemuBridgeError::BadState(format!(
                "managed xemu EEPROM must be {EEPROM_BYTES} bytes, got {}",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    pub(super) fn current_media_identity(&mut self) -> XemuResult<Option<StateFileIdentity>> {
        let Some(path) = self.current_disc.clone() else {
            self.state_media_identity_cache = None;
            return Ok(None);
        };
        let before = observe_media_file(&path)?;
        if let Some(cached) = self.state_media_identity_cache.as_ref() {
            if cached.path == path && cached.observation == before {
                return Ok(Some(cached.identity.clone()));
            }
        }
        let identity = crate::launch::xemu::identity_for_regular_file(&path)?;
        let after = observe_media_file(&path)?;
        if before != after {
            return Err(XemuBridgeError::BadState(format!(
                "Xbox disc changed while establishing its state identity: {}",
                path.display()
            )));
        }
        let identity = StateFileIdentity {
            path: path.display().to_string(),
            size: identity.size,
            sha256: identity.sha256,
        };
        self.state_media_identity_cache = Some(MediaIdentityCache {
            path,
            observation: after,
            identity: identity.clone(),
        });
        Ok(Some(identity))
    }
}

fn observe_media_file(path: &Path) -> XemuResult<MediaFileObservation> {
    let file = crate::path_safety::open_regular_file_no_follow(path)?;
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(MediaFileObservation {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            device: metadata.dev(),
            inode: metadata.ino(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(MediaFileObservation {
            size: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}
