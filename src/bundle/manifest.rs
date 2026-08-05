use serde::{Deserialize, Serialize};

pub use super::legacy_manifest::{
    Artifact, ComponentId, LegacyManifest, RingPolicy, RomId, Slice, Trigger, TriggerKind,
    LEGACY_FORMAT_VERSION,
};
pub use super::recording_manifest::{RecordingManifest, RECORDING_FORMAT_VERSION};

/// Kept as a source-compatible name for the format-1 finalize and tracking APIs.
pub type Manifest = LegacyManifest;
pub const FORMAT_VERSION: u32 = LEGACY_FORMAT_VERSION;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum BundleManifest {
    Legacy(Box<LegacyManifest>),
    Recording(Box<RecordingManifest>),
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestDecodeError {
    #[error("manifest JSON parse failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported format_version: {0}")]
    UnsupportedFormatVersion(u32),
}

#[derive(Deserialize)]
struct VersionProbe {
    format_version: u32,
}

pub fn parse_manifest(json: &str) -> Result<BundleManifest, ManifestDecodeError> {
    let version = serde_json::from_str::<VersionProbe>(json)?.format_version;
    match version {
        LEGACY_FORMAT_VERSION => Ok(BundleManifest::Legacy(Box::new(serde_json::from_str(
            json,
        )?))),
        RECORDING_FORMAT_VERSION => Ok(BundleManifest::Recording(Box::new(serde_json::from_str(
            json,
        )?))),
        other => Err(ManifestDecodeError::UnsupportedFormatVersion(other)),
    }
}

impl<'de> Deserialize<'de> for BundleManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        parse_manifest(&value.to_string()).map_err(serde::de::Error::custom)
    }
}
