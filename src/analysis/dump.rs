use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::diff::RegionSet;

/// `regions.json`의 한 항목. 포맷 단일 진실 원천.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionMeta {
    pub name: String,
    pub memory_type: String,
    pub base_address: u64,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("regions.json 없음: {0}")]
    MetaNotFound(PathBuf),
    #[error("regions.json 파싱 실패: {0}")]
    MetaParse(#[from] serde_json::Error),
    #[error("입출력 오류: {0}")]
    Io(#[from] std::io::Error),
    #[error("리전 바이트 파일 없음: {0}")]
    BinNotFound(PathBuf),
}

/// 덤프 디렉토리(regions.json + <name>.bin)를 RegionSet으로 읽는다.
pub fn load(dir: &Path) -> Result<RegionSet, DumpError> {
    let meta_path = dir.join("regions.json");
    if !meta_path.exists() {
        return Err(DumpError::MetaNotFound(meta_path));
    }
    let metas: Vec<RegionMeta> = serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?;
    let mut set = RegionSet::new();
    for m in metas {
        let bin = dir.join(format!("{}.bin", m.name));
        if !bin.exists() {
            return Err(DumpError::BinNotFound(bin));
        }
        let bytes = std::fs::read(&bin)?;
        set.insert(&m.name, m.base_address, bytes);
    }
    Ok(set)
}

/// 덤프 디렉토리의 `state.json`(레지스터/DMA/PPU 스냅샷)을 읽는다. 없으면 None.
pub fn load_state_map(dir: &Path) -> Result<Option<BTreeMap<String, Value>>, DumpError> {
    let p = dir.join("state.json");
    if !p.exists() {
        return Ok(None);
    }
    let map: BTreeMap<String, Value> = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
    Ok(Some(map))
}
