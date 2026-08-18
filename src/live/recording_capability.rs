use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bundle::recording_manifest::{EventArmingScope, EventClassIdentity, RecordingLimits};
use crate::event_contracts::{EventContractError, EventContractRegistry};
pub use crate::input_movie::INPUT_MOVIE_FORMAT;
use crate::live::temporal::{MAX_SYNC_ADVANCE_COUNT, MAX_SYNC_OPERATION_MS};

/// Recording shares the finite synchronous guest-advance envelope. Adapters still advertise a
/// conservative per-runtime subset, and callers must explicitly request every captured frame.
pub const CORE_MAX_RECORDING_FRAMES: u64 = MAX_SYNC_ADVANCE_COUNT;
pub const CORE_MAX_RECORDING_EVENTS: u64 = 100_000;
pub const CORE_MAX_RECORDING_BYTES: u64 = 64 * 1024 * 1024;
pub const CORE_MAX_RECORDING_LINE_BYTES: u64 = 64 * 1024;
pub const CORE_MAX_RECORDING_HOST_MS: u64 = MAX_SYNC_OPERATION_MS;
pub const CORE_MAX_INPUT_MOVIE_BYTES: u64 = 1024 * 1024;
pub const CORE_MAX_INPUT_BUTTONS_PER_FRAME: u64 = 64;
pub const CORE_MAX_TERMINAL_SNAPSHOT_MEMBERS: u64 = 8;
pub const CORE_MAX_TERMINAL_SNAPSHOT_MEMBER_BYTES: u64 = 128 * 1024;
pub const CORE_MAX_TERMINAL_SNAPSHOT_TOTAL_BYTES: u64 = 1024 * 1024;
pub const CORE_MAX_INITIAL_SNAPSHOT_MEMBERS: u64 = 8;
pub const CORE_MAX_INITIAL_SNAPSHOT_MEMBER_BYTES: u64 = 128 * 1024;
pub const CORE_MAX_INITIAL_SNAPSHOT_TOTAL_BYTES: u64 = 1024 * 1024;
pub const CORE_MAX_INITIAL_SNAPSHOT_CALLBACK_MS: u64 = 500;
pub const CORE_MAX_TERMINAL_STATE_BYTES: u64 = 128 * 1024;
pub const INITIAL_RECORDING_CAPABILITY_REVISION: &str =
    "9320c892dcc55522769a9c5dcc70c5e5ba3b0f9f1f43cada9c4550bb54d894fc";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingCapability {
    pub revision: String,
    pub origins: Vec<RecordingCapabilityOrigin>,
    pub units: Vec<RecordingCapabilityUnit>,
    pub default_event_classes: Vec<String>,
    pub event_classes: Vec<RecordingEventCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_order: Option<RecordingEventOrder>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub class_accounting: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_movie: Option<RecordingInputMovieCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_snapshots: Option<RecordingInitialSnapshotCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_snapshots: Option<RecordingTerminalSnapshotCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_state: Option<RecordingTerminalStateCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup: Option<RecordingWarmupCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeatability: Option<RecordingRepeatabilityCapability>,
    pub limits: RecordingLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingRepeatabilityCapability {
    pub profile: String,
    pub conditions_sha256: String,
    pub origins: Vec<RecordingCapabilityOrigin>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub requires_input_movie: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingCapabilityOrigin {
    NextFrameBoundary,
    ResetRelease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingCapabilityUnit {
    Frames,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingEventOrder {
    GuestEmission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingEventCapability {
    pub id: String,
    pub contract_sha256: String,
    pub clock_domains: Vec<String>,
    pub exact: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stoppable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub startable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filterable_fields: Vec<RecordingEventFilterField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingEventFilterKind {
    U64Range,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingEventFilterField {
    pub path: String,
    pub kind: RecordingEventFilterKind,
    pub min: u64,
    pub max: u64,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingInputMovieCapability {
    pub format: String,
    pub port: u64,
    pub max_frames: u64,
    pub max_bytes: u64,
    pub max_buttons_per_frame: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingTerminalSnapshotCapability {
    pub max_members: u64,
    pub max_member_bytes: u64,
    pub max_total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingInitialSnapshotPosition {
    EventAnchor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingInitialSnapshotCapability {
    pub memory_types: Vec<String>,
    pub start_positions: Vec<RecordingInitialSnapshotPosition>,
    pub max_members: u64,
    pub max_member_bytes: u64,
    pub max_total_bytes: u64,
    pub max_callback_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingTerminalStateCapability {
    pub max_bytes: u64,
    pub profiles: Vec<RecordingTerminalStateProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingTerminalStateProfile {
    pub id: String,
    pub contract_sha256: String,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingWarmupCapability {
    pub max_frames: u64,
    pub transaction_event_classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selectable_event_scopes: Vec<RecordingEventScopeCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingEventScopeCapability {
    pub id: String,
    pub scopes: Vec<EventArmingScope>,
}

#[derive(Serialize)]
struct RecordingCapabilityRevision<'a> {
    origins: &'a [RecordingCapabilityOrigin],
    units: &'a [RecordingCapabilityUnit],
    default_event_classes: &'a [String],
    event_classes: &'a [RecordingEventCapability],
    #[serde(skip_serializing_if = "Option::is_none")]
    event_order: &'a Option<RecordingEventOrder>,
    #[serde(skip_serializing_if = "is_false")]
    class_accounting: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_movie: &'a Option<RecordingInputMovieCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_snapshots: &'a Option<RecordingInitialSnapshotCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_snapshots: &'a Option<RecordingTerminalSnapshotCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_state: &'a Option<RecordingTerminalStateCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warmup: &'a Option<RecordingWarmupCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeatability: &'a Option<RecordingRepeatabilityCapability>,
    limits: &'a RecordingLimits,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingCapabilityError {
    #[error("recording capability JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("recording event contract is invalid: {0}")]
    Contract(#[from] EventContractError),
    #[error("recording capability is invalid: {0}")]
    Invalid(String),
}

impl RecordingCapability {
    pub fn computed_revision(&self) -> Result<String, RecordingCapabilityError> {
        let material = RecordingCapabilityRevision {
            origins: &self.origins,
            units: &self.units,
            default_event_classes: &self.default_event_classes,
            event_classes: &self.event_classes,
            event_order: &self.event_order,
            class_accounting: self.class_accounting,
            input_movie: &self.input_movie,
            initial_snapshots: &self.initial_snapshots,
            terminal_snapshots: &self.terminal_snapshots,
            terminal_state: &self.terminal_state,
            warmup: &self.warmup,
            repeatability: &self.repeatability,
            limits: &self.limits,
        };
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&material)?)))
    }

    pub fn from_hello(
        value: Option<&serde_json::Value>,
        registry: &EventContractRegistry,
    ) -> Result<Option<Self>, RecordingCapabilityError> {
        let Some(value) = value else {
            return Ok(None);
        };
        let capability: Self = serde_json::from_value(value.clone())?;
        capability.validate(registry)?;
        Ok(Some(capability))
    }

    pub fn validate(
        &self,
        registry: &EventContractRegistry,
    ) -> Result<(), RecordingCapabilityError> {
        if self.revision.len() != 64 || !self.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(RecordingCapabilityError::Invalid(
                "revision must be a SHA-256".into(),
            ));
        }
        if !self
            .revision
            .eq_ignore_ascii_case(&self.computed_revision()?)
        {
            return Err(RecordingCapabilityError::Invalid(
                "revision does not cover the advertised recording capability".into(),
            ));
        }
        if let Some(repeatability) = &self.repeatability {
            if repeatability.profile.is_empty()
                || repeatability.profile.len() > 96
                || !repeatability
                    .profile
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            {
                return Err(RecordingCapabilityError::Invalid(
                    "repeatability profile must be a safe non-empty identifier".into(),
                ));
            }
            if repeatability.conditions_sha256.len() != 64
                || !repeatability
                    .conditions_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(RecordingCapabilityError::Invalid(
                    "repeatability conditions_sha256 must be a SHA-256".into(),
                ));
            }
            if repeatability.origins.is_empty()
                || repeatability.origins.iter().collect::<BTreeSet<_>>().len()
                    != repeatability.origins.len()
                || repeatability
                    .origins
                    .iter()
                    .any(|origin| !self.origins.contains(origin))
            {
                return Err(RecordingCapabilityError::Invalid(
                    "repeatability origins must be a non-empty subset of recording origins".into(),
                ));
            }
            if repeatability.requires_input_movie && self.input_movie.is_none() {
                return Err(RecordingCapabilityError::Invalid(
                    "repeatability requires an advertised input movie capability".into(),
                ));
            }
        }
        if self.origins.first() != Some(&RecordingCapabilityOrigin::NextFrameBoundary)
            || self.origins.len() > 2
            || self.origins.iter().collect::<BTreeSet<_>>().len() != self.origins.len()
        {
            return Err(RecordingCapabilityError::Invalid(
                "origins must start with one next_frame_boundary and contain no duplicates".into(),
            ));
        }
        if self.units != [RecordingCapabilityUnit::Frames] {
            return Err(RecordingCapabilityError::Invalid(
                "the initial profile requires only frame units".into(),
            ));
        }
        if self.event_classes.is_empty() || self.default_event_classes.is_empty() {
            return Err(RecordingCapabilityError::Invalid(
                "event classes and defaults must not be empty".into(),
            ));
        }

        let mut ids = BTreeSet::new();
        for event in &self.event_classes {
            if !ids.insert(event.id.as_str()) {
                return Err(RecordingCapabilityError::Invalid(format!(
                    "duplicate event class {}",
                    event.id
                )));
            }
            let identity = EventClassIdentity {
                id: event.id.clone(),
                contract_sha256: event.contract_sha256.clone(),
            };
            let contract = registry.validate_identity(&identity)?;
            if !event.exact || event.clock_domains != [contract.clock_domain.clone()] {
                return Err(RecordingCapabilityError::Invalid(format!(
                    "event class {} does not exactly match its registered clock",
                    event.id
                )));
            }
            let mut filter_paths = BTreeSet::new();
            for field in &event.filterable_fields {
                let registered = contract
                    .payload_fields
                    .iter()
                    .find(|registered| registered.path == field.path);
                let valid = registered.is_some_and(|registered| {
                    registered.value_type == crate::event_contracts::PayloadValueType::U64
                        && field.kind == RecordingEventFilterKind::U64Range
                        && field.min == registered.min.unwrap_or(0)
                        && field.max == registered.max.unwrap_or(u64::MAX)
                });
                if !filter_paths.insert(field.path.as_str()) || !valid {
                    return Err(RecordingCapabilityError::Invalid(format!(
                        "event class {} advertises an invalid filterable field {}",
                        event.id, field.path
                    )));
                }
            }
        }
        if self.event_classes.iter().any(|event| event.startable)
            && (self.event_order != Some(RecordingEventOrder::GuestEmission)
                || self.warmup.as_ref().is_none_or(|warmup| {
                    !warmup
                        .transaction_event_classes
                        .iter()
                        .any(|id| id == "frame_boundary")
                }))
        {
            return Err(RecordingCapabilityError::Invalid(
                "startable event classes require guest_emission order and a transaction frame boundary"
                    .into(),
            ));
        }
        let mut defaults = BTreeSet::new();
        for id in &self.default_event_classes {
            if !ids.contains(id.as_str()) || !defaults.insert(id.as_str()) {
                return Err(RecordingCapabilityError::Invalid(format!(
                    "invalid default event class {id}"
                )));
            }
        }
        if !ids.contains("frame_boundary") || !defaults.contains("frame_boundary") {
            return Err(RecordingCapabilityError::Invalid(
                "the initial profile requires frame_boundary".into(),
            ));
        }

        if let Some(movie) = &self.input_movie {
            if movie.format != INPUT_MOVIE_FORMAT
                || movie.port != 0
                || movie.max_frames == 0
                || movie.max_frames > self.limits.max_frames
                || movie.max_bytes == 0
                || movie.max_bytes > CORE_MAX_INPUT_MOVIE_BYTES
                || movie.max_buttons_per_frame == 0
                || movie.max_buttons_per_frame > CORE_MAX_INPUT_BUTTONS_PER_FRAME
            {
                return Err(RecordingCapabilityError::Invalid(
                    "input movie format, port, or bounds are invalid".into(),
                ));
            }
        }

        if let Some(snapshots) = &self.terminal_snapshots {
            if snapshots.max_members == 0
                || snapshots.max_members > CORE_MAX_TERMINAL_SNAPSHOT_MEMBERS
                || snapshots.max_member_bytes == 0
                || snapshots.max_member_bytes > CORE_MAX_TERMINAL_SNAPSHOT_MEMBER_BYTES
                || snapshots.max_total_bytes == 0
                || snapshots.max_total_bytes > CORE_MAX_TERMINAL_SNAPSHOT_TOTAL_BYTES
                || snapshots.max_total_bytes < snapshots.max_member_bytes
            {
                return Err(RecordingCapabilityError::Invalid(
                    "terminal snapshot bounds are outside the Core limits".into(),
                ));
            }
        }

        if let Some(snapshots) = &self.initial_snapshots {
            let mut memory_types = BTreeSet::new();
            if !self.event_classes.iter().any(|event| event.startable)
                || snapshots.start_positions != [RecordingInitialSnapshotPosition::EventAnchor]
                || snapshots.memory_types.is_empty()
                || snapshots.memory_types.iter().any(|memory_type| {
                    memory_type.is_empty()
                        || memory_type.len() > 128
                        || !memory_types.insert(memory_type.as_str())
                })
                || snapshots.max_members == 0
                || snapshots.max_members > CORE_MAX_INITIAL_SNAPSHOT_MEMBERS
                || snapshots.max_member_bytes == 0
                || snapshots.max_member_bytes > CORE_MAX_INITIAL_SNAPSHOT_MEMBER_BYTES
                || snapshots.max_total_bytes == 0
                || snapshots.max_total_bytes > CORE_MAX_INITIAL_SNAPSHOT_TOTAL_BYTES
                || snapshots.max_total_bytes < snapshots.max_member_bytes
                || snapshots.max_callback_ms == 0
                || snapshots.max_callback_ms > CORE_MAX_INITIAL_SNAPSHOT_CALLBACK_MS
            {
                return Err(RecordingCapabilityError::Invalid(
                    "initial snapshot positions, memory types, or bounds are invalid".into(),
                ));
            }
        }

        if let Some(state) = &self.terminal_state {
            if state.max_bytes == 0
                || state.max_bytes > CORE_MAX_TERMINAL_STATE_BYTES
                || state.profiles.is_empty()
            {
                return Err(RecordingCapabilityError::Invalid(
                    "terminal state bounds or profiles are invalid".into(),
                ));
            }
            let mut profile_ids = BTreeSet::new();
            for profile in &state.profiles {
                let groups = profile
                    .groups
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                if !safe_profile_token(&profile.id)
                    || !profile_ids.insert(profile.id.as_str())
                    || groups.is_empty()
                    || groups.len() != profile.groups.len()
                    || !groups.iter().all(|group| safe_profile_token(group))
                    || profile.contract_sha256 != terminal_state_contract_sha256(&profile.groups)?
                {
                    return Err(RecordingCapabilityError::Invalid(
                        "terminal state profile identity, groups, or contract is invalid".into(),
                    ));
                }
            }
        }

        if let Some(warmup) = &self.warmup {
            let transaction_classes: BTreeSet<_> = warmup
                .transaction_event_classes
                .iter()
                .map(String::as_str)
                .collect();
            let mut selectable_classes = BTreeSet::new();
            let selectable_scopes_are_valid = warmup.selectable_event_scopes.iter().all(|entry| {
                let scopes = entry.scopes.iter().copied().collect::<BTreeSet<_>>();
                let default_scope = if transaction_classes.contains(entry.id.as_str()) {
                    EventArmingScope::Transaction
                } else {
                    EventArmingScope::Observation
                };
                ids.contains(entry.id.as_str())
                    && selectable_classes.insert(entry.id.as_str())
                    && scopes.len() == entry.scopes.len()
                    && scopes.len() > 1
                    && scopes.contains(&default_scope)
            });
            if warmup.max_frames == 0
                || warmup.max_frames > self.limits.max_frames
                || transaction_classes.len() != warmup.transaction_event_classes.len()
                || !transaction_classes.contains("frame_boundary")
                || !transaction_classes.iter().all(|id| ids.contains(id))
                || !selectable_scopes_are_valid
                || !self.class_accounting
            {
                return Err(RecordingCapabilityError::Invalid(
                    "warmup bounds, event scopes, or class accounting are invalid".into(),
                ));
            }
        }

        let limits = &self.limits;
        if limits.max_frames == 0 || limits.max_frames > CORE_MAX_RECORDING_FRAMES {
            return Err(RecordingCapabilityError::Invalid(
                "max_frames is outside the Core bound".into(),
            ));
        }
        if limits.max_events < limits.max_frames || limits.max_events > CORE_MAX_RECORDING_EVENTS {
            return Err(RecordingCapabilityError::Invalid(
                "max_events is outside the Core bound".into(),
            ));
        }
        if limits.max_bytes == 0 || limits.max_bytes > CORE_MAX_RECORDING_BYTES {
            return Err(RecordingCapabilityError::Invalid(
                "max_bytes is outside the Core bound".into(),
            ));
        }
        if limits.max_line_bytes == 0
            || limits.max_line_bytes > CORE_MAX_RECORDING_LINE_BYTES
            || limits.max_line_bytes > limits.max_bytes
        {
            return Err(RecordingCapabilityError::Invalid(
                "max_line_bytes is outside the Core bound".into(),
            ));
        }
        if limits.max_host_ms == 0 || limits.max_host_ms > CORE_MAX_RECORDING_HOST_MS {
            return Err(RecordingCapabilityError::Invalid(
                "max_host_ms is outside the Core bound".into(),
            ));
        }
        if limits.progress_interval_ms < 10 || limits.progress_interval_ms >= limits.max_host_ms {
            return Err(RecordingCapabilityError::Invalid(
                "progress_interval_ms must be in 10..max_host_ms".into(),
            ));
        }
        Ok(())
    }

    pub fn identities(
        &self,
        requested: &[String],
    ) -> Result<Vec<EventClassIdentity>, RecordingCapabilityError> {
        let requested: Vec<&str> = if requested.is_empty() {
            self.default_event_classes
                .iter()
                .map(String::as_str)
                .collect()
        } else {
            requested.iter().map(String::as_str).collect()
        };
        let mut result = Vec::with_capacity(requested.len());
        let mut seen = BTreeSet::new();
        for id in requested {
            if !seen.insert(id) {
                return Err(RecordingCapabilityError::Invalid(format!(
                    "duplicate requested event class {id}"
                )));
            }
            let capability = self
                .event_classes
                .iter()
                .find(|event| event.id == id)
                .ok_or_else(|| {
                    RecordingCapabilityError::Invalid(format!("event class {id} is not advertised"))
                })?;
            result.push(EventClassIdentity {
                id: capability.id.clone(),
                contract_sha256: capability.contract_sha256.clone(),
            });
        }
        Ok(result)
    }
}

fn safe_profile_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn terminal_state_contract_sha256(
    groups: &[String],
) -> Result<String, RecordingCapabilityError> {
    #[derive(Serialize)]
    struct Contract<'a> {
        representation: &'static str,
        groups: &'a [String],
    }
    let contract = Contract {
        representation: "emucap.grouped_flat_state_map",
        groups,
    };
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&contract)?)))
}
