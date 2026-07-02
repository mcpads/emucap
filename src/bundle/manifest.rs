use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub platform: String,
    pub rom: RomId,
    pub adapter: ComponentId,
    pub emulator: ComponentId,
    pub trigger: Trigger,
    pub ring_policy: RingPolicy,
    pub slices: Vec<Slice>,
    pub input_movie: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RomId {
    pub sha1: String,
    pub path_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentId {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub at_unix_ms: u64,
    pub at_frame: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Retrospective,
    /// 녹화 창용 예약 변형 — 포맷 변경 없이 처리만 추가하면 된다.
    RecordWindow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RingPolicy {
    pub interval_frames: u32,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slice {
    pub frame: u64,
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Artifact {
    Savestate {
        path: String,
    },
    Screenshot {
        path: String,
    },
    /// 세이브스테이트에서 뽑은 WRAM/VRAM/CRAM이 여기로 들어온다.
    MemoryRegion {
        name: String,
        path: String,
    },
}
