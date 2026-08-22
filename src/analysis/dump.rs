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
    #[error("덤프 멤버가 안전한 일반 파일이 아님: {0}")]
    UnsafeMember(PathBuf),
    #[error("덤프 리전 크기가 매니페스트와 다름: {0}")]
    SizeMismatch(String),
}

const MAX_META_BYTES: u64 = 4 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REGIONS: usize = 256;
const MAX_REGION_BYTES: u64 = 1024 * 1024 * 1024;

/// 덤프 디렉토리(regions.json + <name>.bin)를 RegionSet으로 읽는다.
pub fn load(dir: &Path) -> Result<RegionSet, DumpError> {
    let meta_path = dir.join("regions.json");
    if std::fs::symlink_metadata(&meta_path).is_err() {
        return Err(DumpError::MetaNotFound(meta_path));
    }
    let meta = crate::path_safety::read_bounded_regular_member(dir, "regions.json", MAX_META_BYTES)
        .map_err(|_| DumpError::UnsafeMember(meta_path.clone()))?;
    let metas: Vec<RegionMeta> = serde_json::from_slice(&meta)?;
    if metas.len() > MAX_REGIONS {
        return Err(DumpError::SizeMismatch(format!(
            "region count exceeds {MAX_REGIONS}"
        )));
    }
    let mut set = RegionSet::new();
    let mut names = std::collections::BTreeSet::new();
    for m in metas {
        let file_name = format!("{}.bin", m.name);
        let bin = dir.join(&file_name);
        if !crate::path_safety::is_portable_file_name(&file_name, 192)
            || !names.insert(m.name.clone())
        {
            return Err(DumpError::UnsafeMember(bin));
        }
        let canonical = match crate::path_safety::regular_member_path(dir, &file_name) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(DumpError::BinNotFound(bin))
            }
            Err(_) => return Err(DumpError::UnsafeMember(bin)),
        };
        let actual = std::fs::metadata(&canonical)?.len();
        if m.size > MAX_REGION_BYTES || actual != m.size {
            return Err(DumpError::SizeMismatch(format!(
                "{}: manifest={}, file={actual}, maximum={MAX_REGION_BYTES}",
                m.name, m.size
            )));
        }
        let bytes = std::fs::read(&canonical)?;
        set.insert(&m.name, m.base_address, bytes);
    }
    Ok(set)
}

/// 덤프 디렉토리의 `state.json`(레지스터/DMA/PPU 스냅샷)을 읽는다. 없으면 None.
pub fn load_state_map(dir: &Path) -> Result<Option<BTreeMap<String, Value>>, DumpError> {
    let p = dir.join("state.json");
    if std::fs::symlink_metadata(&p).is_err() {
        return Ok(None);
    }
    let bytes = crate::path_safety::read_bounded_regular_member(dir, "state.json", MAX_STATE_BYTES)
        .map_err(|_| DumpError::UnsafeMember(p))?;
    let map: BTreeMap<String, Value> = serde_json::from_slice(&bytes)?;
    Ok(Some(map))
}
