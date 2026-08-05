use serde::{Deserialize, Serialize};

pub const RECORDING_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingManifest {
    pub format_version: u32,
    pub capture_id: String,
    pub capture_kind: CaptureKind,
    pub created_at_unix_ms: u64,
    pub request_digest_sha256: String,
    pub runtime: RuntimeIdentity,
    pub request: RecordingRequest,
    pub scope: EffectiveScope,
    pub terminal: TerminalFacts,
    pub counters: RecordingCounters,
    pub loss: LossFacts,
    pub cleanup: CleanupFacts,
    pub members: Vec<MemberDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    RecordWindow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub system: String,
    pub adapter_id: String,
    pub server_build: String,
    pub adapter_build: String,
    pub emulator_id: String,
    pub emulator_build: String,
    pub emulator_upstream_revision: String,
    pub emulator_patchset_sha256: String,
    pub launch_id: String,
    pub capability_revision: String,
    pub content: ContentIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentIdentity {
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub bytes: u64,
    pub path_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingRequest {
    pub frames: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub warmup_frames: u64,
    pub event_classes: Vec<EventClassIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_arming: Vec<EventClassArming>,
    pub limits: RecordingLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_movie: Option<InputMovieIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_on: Option<EventStopCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_on: Option<EventStartCondition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initial_snapshots: Vec<InitialSnapshotRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_snapshots: Vec<TerminalSnapshotRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_state: Option<TerminalStateRequest>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventClassArming {
    pub id: String,
    pub scope: EventArmingScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventArmingScope {
    Transaction,
    Observation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSnapshotRequest {
    pub label: String,
    pub memory_type: String,
    pub address: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialSnapshotRequest {
    pub label: String,
    pub memory_type: String,
    pub address: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalStateRequest {
    pub profile: String,
    pub contract_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputMovieIdentity {
    pub format: String,
    pub port: u64,
    pub frames: u64,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventStopCondition {
    pub event_class: String,
    pub occurrence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventStartCondition {
    pub event_class: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventClassIdentity {
    pub id: String,
    pub contract_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingLimits {
    pub max_frames: u64,
    pub max_events: u64,
    pub max_bytes: u64,
    pub max_line_bytes: u64,
    pub max_host_ms: u64,
    pub progress_interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveScope {
    pub origin: RecordingOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f_origin: Option<u64>,
    pub f_start: u64,
    pub f_end: u64,
    pub clock_domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_order: Option<EventOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_start: Option<ObservationStartFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationStartFacts {
    pub sequence: u64,
    pub event_class: String,
    pub contract_sha256: String,
    pub frame: u64,
    pub clock_domain: String,
    pub clock_tick: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventOrder {
    GuestEmission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingOrigin {
    NextFrameBoundary,
    ResetRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalFacts {
    pub operation_outcome: OperationOutcome,
    pub execution_outcome: ExecutionOutcome,
    pub integrity: Integrity,
    pub publication: PublicationOutcome,
    pub final_execution_state: FinalExecutionState,
    pub final_frame: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_event: Option<EventStopFacts>,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_classes: Vec<EventClassTerminalFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventClassTerminalFacts {
    pub id: String,
    pub armed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed_interval: Option<FrameInterval>,
    pub observed: u64,
    pub dropped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameInterval {
    pub f_start: u64,
    pub f_end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventStopFacts {
    pub sequence: u64,
    pub event_class: String,
    pub clock_domain: String,
    pub clock_tick: u64,
    pub frame: u64,
    pub occurrence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Completed,
    Interrupted,
    Aborted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    TargetReached,
    EventStop,
    EmulatorExited,
    GenerationChanged,
    AdapterError,
    LossDetected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Integrity {
    Complete,
    Lossy,
    Unverifiable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationOutcome {
    Published,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalExecutionState {
    Frozen,
    Terminated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingCounters {
    pub frames: u64,
    pub events: u64,
    pub bytes: u64,
    pub dropped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LossFacts {
    pub dropped: u64,
    pub truncated: bool,
    pub first_sequence_gap: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupFacts {
    pub hooks: CleanupState,
    pub transient_input: CleanupState,
    pub sink: CleanupState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupState {
    Released,
    NotAcquired,
    GenerationTerminated,
    Unverifiable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberDescriptor {
    pub role: MemberRole,
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub records: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Events,
    InputMovie,
    InitialSnapshot,
    TerminalSnapshot,
    TerminalState,
}
