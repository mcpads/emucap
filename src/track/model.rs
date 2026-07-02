use serde::{Deserialize, Serialize};

pub const RUN_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Done,
    Aborted,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Machine,
    Judgment,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproStatus {
    Clean,
    ReplayableWithInterventions,
    SavestateOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rom {
    pub sha1: String,
    pub platform: String,
    pub title: Option<String>,
    pub first_seen: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gate {
    pub id: String,
    pub name: String,
    pub kind: GateKind,
    pub passed: Option<bool>,
    pub evidence_ref: Option<String>,
    pub detail: Option<String>,
    pub case_ref: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub id: String,
    pub key: String,
    pub value: f64,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub sha256: String,
    pub meta: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intervention {
    pub id: String,
    pub seq: u64,
    pub at_frame: Option<u64>,
    pub at_event: Option<String>,
    pub frozen_context: bool,
    pub op: String,
    pub args: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub format_version: u32,
    pub id: String,
    pub rom_sha1: String,
    pub goal: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub status: RunStatus,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub agent: Option<String>,
    pub session: Option<String>,
    pub connection_ref: Option<String>,
    pub repro_base: Option<String>,
    pub repro_movie_ref: Option<String>,
    pub repro_status: Option<ReproStatus>,
    #[serde(default)]
    pub gates: Vec<Gate>,
    #[serde(default)]
    pub metrics: Vec<Metric>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub interventions: Vec<Intervention>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub rom_sha1: String,
    pub run_id: Option<String>,
    pub claim: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub promoted: bool,
    pub created_at: String,
}
