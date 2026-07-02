use serde::{Deserialize, Serialize};

use super::manifest::{ComponentId, RingPolicy, Slice, Trigger};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawManifest {
    pub format_version: u32,
    pub platform: String,
    /// 원본 ROM 파일 경로. SHA-1 계산은 finalize의 책임.
    pub rom_path: String,
    pub adapter: ComponentId,
    pub emulator: ComponentId,
    pub trigger: Trigger,
    pub ring_policy: RingPolicy,
    pub slices: Vec<Slice>,
    pub input_movie: Option<String>,
}

pub fn parse_raw(json: &str) -> Result<RawManifest, serde_json::Error> {
    serde_json::from_str(json)
}
