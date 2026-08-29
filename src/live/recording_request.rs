use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use super::link::{EmulatorIdentity, MemoryRegion};
use super::recording::RecordingError;
use super::recording_capability::{
    RecordingCapability, RecordingCapabilityOrigin, RecordingEventFilterKind,
};
use super::recording_input::{acquire_recording_movie, AcquiredRecordingMovie};
use super::recording_state::AcquiredRecordingState;
use super::runtime::CurrentManifest;
use crate::bundle::recording_manifest::{
    ContentIdentity, EventArmingScope, EventClassArming, EventClassFilter, EventFilterTerm,
    EventStartCondition, EventStopCondition, InitialSnapshotRequest, RecordingOrigin,
    RecordingRequest, RuntimeIdentity, TerminalSnapshotRequest, TerminalStateRequest,
    MAX_EVENT_FILTER_TERMS,
};

const DEFAULT_RECORDING_HOST_MS_MIN: u64 = 30_000;
const DEFAULT_RECORDING_HOST_MS_PER_FRAME: u64 = 50;

pub(super) fn default_recording_host_ms(advertised_max: u64, frames: u64) -> u64 {
    frames
        .saturating_mul(DEFAULT_RECORDING_HOST_MS_PER_FRAME)
        .max(DEFAULT_RECORDING_HOST_MS_MIN)
        .min(advertised_max)
}

#[derive(Debug, Clone)]
pub struct RecordWindowRequest {
    pub output_root: PathBuf,
    pub frames: u64,
    pub warmup_frames: u64,
    pub event_classes: Vec<String>,
    pub event_filters: Vec<EventClassFilter>,
    pub event_arming_overrides: Vec<EventClassArming>,
    pub origin: Option<RecordingOrigin>,
    pub input_path: Option<PathBuf>,
    pub initial_state: Option<RecordingStateInput>,
    pub stop_on: Option<EventStopCondition>,
    pub start_on: Option<EventStartCondition>,
    pub initial_snapshots: Vec<InitialSnapshotRequest>,
    pub terminal_snapshots: Vec<TerminalSnapshotRequest>,
    pub terminal_state_profile: Option<String>,
    pub require_repeatable: bool,
    pub limits: Option<RequestedRecordingLimits>,
}

#[derive(Debug, Clone)]
pub struct RecordingStateInput {
    pub snapshot_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestedRecordingLimits {
    pub max_events: Option<u64>,
    pub max_bytes: Option<u64>,
    pub max_host_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct RequestDigest<'a> {
    output_root: &'a str,
    origin: RecordingOrigin,
    request: &'a RecordingRequest,
}

#[derive(Debug)]
pub(super) struct EffectiveRequest {
    pub(super) request: RecordingRequest,
    pub(super) origin: RecordingOrigin,
    pub(super) movie: Option<AcquiredRecordingMovie>,
    pub(super) state: Option<AcquiredRecordingState>,
}

pub(super) fn effective_request(
    capability: &RecordingCapability,
    methods: &[String],
    memory_regions: &[MemoryRegion],
    request: &RecordWindowRequest,
    acquired_state: Option<AcquiredRecordingState>,
) -> Result<EffectiveRequest, RecordingError> {
    let total_frames = request
        .warmup_frames
        .checked_add(request.frames)
        .ok_or_else(|| RecordingError::Invalid("recording frame count overflow".into()))?;
    if request.frames == 0 || total_frames > capability.limits.max_frames {
        return Err(RecordingError::Invalid(format!(
            "warmup_frames + frames must be in 1..={}",
            capability.limits.max_frames
        )));
    }
    let origin = request.origin.unwrap_or(RecordingOrigin::NextFrameBoundary);
    if origin == RecordingOrigin::StateLoad
        && (request.warmup_frames > 0
            || request.start_on.is_some()
            || !request.initial_snapshots.is_empty())
    {
        return Err(RecordingError::Invalid(
            "state_load currently starts at its advertised frame alignment and cannot be combined with warmup_frames, start_on, or initial_snapshots".into(),
        ));
    }
    if request.warmup_frames > 0 {
        let warmup = capability.warmup.as_ref().ok_or_else(|| {
            RecordingError::Unavailable(
                "the current runtime does not advertise recording warmup".into(),
            )
        })?;
        if request.warmup_frames > warmup.max_frames {
            return Err(RecordingError::Invalid(format!(
                "warmup_frames exceeds {}",
                warmup.max_frames
            )));
        }
    }
    let capability_origin = match origin {
        RecordingOrigin::NextFrameBoundary => RecordingCapabilityOrigin::NextFrameBoundary,
        RecordingOrigin::ResetRelease => RecordingCapabilityOrigin::ResetRelease,
        RecordingOrigin::StateLoad => RecordingCapabilityOrigin::StateLoad,
    };
    if request.require_repeatable
        && capability
            .repeatability
            .as_ref()
            .is_none_or(|repeatability| !repeatability.origins.contains(&capability_origin))
    {
        return Err(RecordingError::Unavailable(format!(
            "the current runtime does not advertise repeatable recording for origin {origin:?}"
        )));
    }
    if request.require_repeatable
        && capability
            .repeatability
            .as_ref()
            .is_some_and(|repeatability| repeatability.requires_input_movie)
        && request.input_path.is_none()
    {
        return Err(RecordingError::Invalid(
            "the selected repeatable profile requires an explicit input movie; use an all-empty movie when no buttons should be pressed".into(),
        ));
    }
    if !capability.origins.contains(&capability_origin) {
        return Err(RecordingError::Unavailable(format!(
            "the current runtime does not advertise origin {origin:?}"
        )));
    }
    let state = match (origin, request.initial_state.as_ref(), acquired_state) {
        (RecordingOrigin::StateLoad, Some(input), Some(state)) => {
            let state_load = capability.state_load.as_ref().ok_or_else(|| {
                RecordingError::Unavailable(
                    "the current runtime does not advertise state-backed recording".into(),
                )
            })?;
            if state_load.requires_input_movie && request.input_path.is_none() {
                return Err(RecordingError::Invalid(
                    "state-backed recording requires an explicit input movie; use an all-empty movie when no buttons should be pressed".into(),
                ));
            }
            if input.snapshot_id != state.receipt.snapshot_id {
                return Err(RecordingError::Invalid(
                    "resolved snapshot does not match initial_state.snapshot_id".into(),
                ));
            }
            Some(state)
        }
        (RecordingOrigin::StateLoad, None, _) => {
            return Err(RecordingError::Invalid(
                "state_load origin requires initial_state".into(),
            ));
        }
        (RecordingOrigin::StateLoad, Some(_), None) => {
            return Err(RecordingError::Unavailable(
                "producer-managed initial state was not resolved".into(),
            ));
        }
        (_, Some(_), _) => {
            return Err(RecordingError::Invalid(
                "initial_state requires the state_load origin".into(),
            ));
        }
        (_, None, Some(_)) => {
            return Err(RecordingError::Invalid(
                "unexpected resolved state without initial_state".into(),
            ));
        }
        (_, None, None) => None,
    };
    let event_classes = capability.identities(&request.event_classes)?;
    let event_filters = validate_event_filters(capability, &event_classes, &request.event_filters)?;
    validate_start_request(capability, &event_classes, request)?;
    validate_initial_snapshot_request(capability, memory_regions, request)?;
    let event_arming = resolve_event_arming(capability, &event_classes, request)?;
    validate_terminal_snapshot_request(capability, methods, memory_regions, request)?;
    let terminal_state = validate_terminal_state_request(capability, methods, request)?;
    if !event_classes
        .iter()
        .any(|identity| identity.id == "frame_boundary")
    {
        return Err(RecordingError::Invalid(
            "frame_boundary must be selected for a recording window".into(),
        ));
    }
    if let Some(stop_on) = &request.stop_on {
        if stop_on.occurrence == 0 {
            return Err(RecordingError::Invalid(
                "stop_on.occurrence must be positive".into(),
            ));
        }
        let event = capability
            .event_classes
            .iter()
            .find(|event| event.id == stop_on.event_class)
            .ok_or_else(|| {
                RecordingError::Unavailable(format!(
                    "stop event class {} is not advertised",
                    stop_on.event_class
                ))
            })?;
        if !event.stoppable
            || !event_classes
                .iter()
                .any(|identity| identity.id == stop_on.event_class)
        {
            return Err(RecordingError::Invalid(
                "stop event class must be selected, exact, and stoppable".into(),
            ));
        }
        if stop_on.event_class == "frame_completed" && stop_on.occurrence > request.frames {
            return Err(RecordingError::Invalid(
                "frame_completed occurrence cannot exceed the frame bound".into(),
            ));
        }
    }
    let mut limits = capability.limits.clone();
    if request
        .limits
        .as_ref()
        .and_then(|limits| limits.max_host_ms)
        .is_none()
    {
        // Host time bounds cleanup only; it never schedules guest input or progress.
        limits.max_host_ms = default_recording_host_ms(limits.max_host_ms, total_frames);
    }
    if let Some(requested) = &request.limits {
        narrow_limit("max_events", &mut limits.max_events, requested.max_events)?;
        narrow_limit("max_bytes", &mut limits.max_bytes, requested.max_bytes)?;
        narrow_limit(
            "max_host_ms",
            &mut limits.max_host_ms,
            requested.max_host_ms,
        )?;
    }
    let required_events = event_classes
        .iter()
        .filter(|identity| matches!(identity.id.as_str(), "frame_boundary" | "frame_completed"))
        .try_fold(0_u64, |total, identity| {
            let class_frames = event_arming
                .iter()
                .find(|arming| arming.id == identity.id)
                .map_or(request.frames, |arming| match arming.scope {
                    EventArmingScope::Transaction => total_frames,
                    EventArmingScope::Observation => request.frames,
                });
            total.checked_add(class_frames)
        })
        .ok_or_else(|| RecordingError::Invalid("required event count overflow".into()))?;
    if limits.max_events < required_events || limits.max_bytes < limits.max_line_bytes {
        return Err(RecordingError::Invalid(
            "requested limits cannot contain the required frame stream".into(),
        ));
    }
    if limits.max_host_ms <= limits.progress_interval_ms {
        return Err(RecordingError::Invalid(format!(
            "max_host_ms must exceed the adapter progress interval of {} ms",
            limits.progress_interval_ms
        )));
    }
    limits.max_frames = total_frames;
    let movie = match &request.input_path {
        Some(path) => {
            let capability = capability.input_movie.as_ref().ok_or_else(|| {
                RecordingError::Unavailable(
                    "the current runtime does not advertise input movies".into(),
                )
            })?;
            if total_frames > capability.max_frames {
                return Err(RecordingError::Invalid(format!(
                    "input movie frame count exceeds {}",
                    capability.max_frames
                )));
            }
            Some(acquire_recording_movie(path, total_frames, capability)?)
        }
        None => None,
    };
    Ok(EffectiveRequest {
        request: RecordingRequest {
            frames: request.frames,
            warmup_frames: request.warmup_frames,
            event_classes,
            event_filters,
            event_arming,
            limits,
            input_movie: movie.as_ref().map(|movie| movie.identity.clone()),
            initial_state: state.as_ref().map(|state| state.receipt.clone()),
            stop_on: request.stop_on.clone(),
            start_on: request.start_on.clone(),
            initial_snapshots: request.initial_snapshots.clone(),
            terminal_snapshots: request.terminal_snapshots.clone(),
            terminal_state,
        },
        origin,
        movie,
        state,
    })
}

fn resolve_event_arming(
    capability: &RecordingCapability,
    selected: &[crate::bundle::recording_manifest::EventClassIdentity],
    request: &RecordWindowRequest,
) -> Result<Vec<EventClassArming>, RecordingError> {
    if request.warmup_frames == 0 && request.start_on.is_none() {
        if !request.event_arming_overrides.is_empty() {
            return Err(RecordingError::Invalid(
                "event_arming_overrides require warmup_frames".into(),
            ));
        }
        return Ok(Vec::new());
    }
    if request.start_on.is_some() && !request.event_arming_overrides.is_empty() {
        return Err(RecordingError::Invalid(
            "event_arming_overrides cannot be combined with start_on".into(),
        ));
    }
    let warmup = capability.warmup.as_ref().ok_or_else(|| {
        RecordingError::Unavailable(
            "the current runtime does not advertise recording warmup".into(),
        )
    })?;
    let selected_ids = selected
        .iter()
        .map(|identity| identity.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut overrides = std::collections::BTreeMap::new();
    for requested in &request.event_arming_overrides {
        if !selected_ids.contains(requested.id.as_str()) {
            return Err(RecordingError::Invalid(format!(
                "event arming override {} must select a recorded class",
                requested.id
            )));
        }
        if overrides
            .insert(requested.id.as_str(), requested.scope)
            .is_some()
        {
            return Err(RecordingError::Invalid(format!(
                "duplicate event arming override {}",
                requested.id
            )));
        }
        let advertised = warmup
            .selectable_event_scopes
            .iter()
            .find(|entry| entry.id == requested.id)
            .ok_or_else(|| {
                RecordingError::Unavailable(format!(
                    "event class {} does not advertise selectable warmup scopes",
                    requested.id
                ))
            })?;
        if !advertised.scopes.contains(&requested.scope) {
            return Err(RecordingError::Unavailable(format!(
                "event class {} does not advertise scope {:?}",
                requested.id, requested.scope
            )));
        }
    }
    Ok(selected
        .iter()
        .map(|identity| EventClassArming {
            id: identity.id.clone(),
            scope: overrides
                .get(identity.id.as_str())
                .copied()
                .unwrap_or_else(|| {
                    if warmup.transaction_event_classes.contains(&identity.id) {
                        EventArmingScope::Transaction
                    } else {
                        EventArmingScope::Observation
                    }
                }),
        })
        .collect())
}

fn validate_event_filters(
    capability: &RecordingCapability,
    selected: &[crate::bundle::recording_manifest::EventClassIdentity],
    requested: &[EventClassFilter],
) -> Result<Vec<EventClassFilter>, RecordingError> {
    let mut classes = BTreeSet::new();
    let mut filters = Vec::with_capacity(requested.len());
    for filter in requested {
        if !classes.insert(filter.event_class.as_str()) {
            return Err(RecordingError::Invalid(format!(
                "duplicate event filter for {}",
                filter.event_class
            )));
        }
        if !selected
            .iter()
            .any(|identity| identity.id == filter.event_class)
        {
            return Err(RecordingError::Invalid(format!(
                "filtered event class {} must be selected",
                filter.event_class
            )));
        }
        if filter.terms.is_empty() || filter.terms.len() > MAX_EVENT_FILTER_TERMS {
            return Err(RecordingError::Invalid(format!(
                "event filter for {} must contain 1..={} terms",
                filter.event_class, MAX_EVENT_FILTER_TERMS
            )));
        }
        let event = capability
            .event_classes
            .iter()
            .find(|event| event.id == filter.event_class)
            .expect("selected event class was resolved from this capability");
        let mut paths = BTreeSet::new();
        let mut terms = filter.terms.clone();
        for term in &terms {
            if !paths.insert(term.path()) {
                return Err(RecordingError::Invalid(format!(
                    "duplicate event filter path {} for {}",
                    term.path(),
                    filter.event_class
                )));
            }
            match term {
                EventFilterTerm::U64Range {
                    path,
                    start,
                    length,
                } => {
                    let field = event
                        .filterable_fields
                        .iter()
                        .find(|field| {
                            field.path == *path && field.kind == RecordingEventFilterKind::U64Range
                        })
                        .ok_or_else(|| {
                            RecordingError::Unavailable(format!(
                                "event class {} does not advertise u64_range filtering for {}",
                                filter.event_class, path
                            ))
                        })?;
                    let end = start.checked_add(*length).ok_or_else(|| {
                        RecordingError::Invalid(format!(
                            "event filter range for {}.{} overflows",
                            filter.event_class, path
                        ))
                    })?;
                    if *length == 0 || *start < field.min || end - 1 > field.max {
                        return Err(RecordingError::Invalid(format!(
                            "event filter range for {}.{} is outside {}..={}",
                            filter.event_class, path, field.min, field.max
                        )));
                    }
                }
            }
        }
        terms.sort_by(|left, right| left.path().cmp(right.path()));
        filters.push(EventClassFilter {
            event_class: filter.event_class.clone(),
            terms,
        });
    }
    filters.sort_by(|left, right| left.event_class.cmp(&right.event_class));
    Ok(filters)
}

fn validate_start_request(
    capability: &RecordingCapability,
    event_classes: &[crate::bundle::recording_manifest::EventClassIdentity],
    request: &RecordWindowRequest,
) -> Result<(), RecordingError> {
    let Some(start) = &request.start_on else {
        if !request.initial_snapshots.is_empty() {
            return Err(RecordingError::Invalid(
                "initial snapshots require an event-aligned start_on condition".into(),
            ));
        }
        return Ok(());
    };
    if capability.event_order
        != Some(super::recording_capability::RecordingEventOrder::GuestEmission)
    {
        return Err(RecordingError::Unavailable(
            "event-aligned start requires guest_emission order".into(),
        ));
    }
    let event = capability
        .event_classes
        .iter()
        .find(|event| event.id == start.event_class)
        .ok_or_else(|| {
            RecordingError::Unavailable(format!(
                "start event class {} is not advertised",
                start.event_class
            ))
        })?;
    if !event.exact
        || !event.startable
        || !event_classes
            .iter()
            .any(|identity| identity.id == start.event_class)
    {
        return Err(RecordingError::Invalid(
            "start event class must be selected, exact, and startable".into(),
        ));
    }
    Ok(())
}

fn validate_initial_snapshot_request(
    capability: &RecordingCapability,
    memory_regions: &[MemoryRegion],
    request: &RecordWindowRequest,
) -> Result<(), RecordingError> {
    if request.initial_snapshots.is_empty() {
        return Ok(());
    }
    let bounds = capability.initial_snapshots.as_ref().ok_or_else(|| {
        RecordingError::Unavailable(
            "the current runtime does not advertise event-aligned initial snapshots".into(),
        )
    })?;
    validate_snapshot_ranges(
        "initial snapshot",
        &request.initial_snapshots,
        memory_regions,
        &bounds.memory_types,
        bounds.max_members,
        bounds.max_member_bytes,
        bounds.max_total_bytes,
    )
}

trait SnapshotRange {
    fn label(&self) -> &str;
    fn memory_type(&self) -> &str;
    fn address(&self) -> u64;
    fn length(&self) -> u64;
}

impl SnapshotRange for InitialSnapshotRequest {
    fn label(&self) -> &str {
        &self.label
    }
    fn memory_type(&self) -> &str {
        &self.memory_type
    }
    fn address(&self) -> u64 {
        self.address
    }
    fn length(&self) -> u64 {
        self.length
    }
}

impl SnapshotRange for TerminalSnapshotRequest {
    fn label(&self) -> &str {
        &self.label
    }
    fn memory_type(&self) -> &str {
        &self.memory_type
    }
    fn address(&self) -> u64 {
        self.address
    }
    fn length(&self) -> u64 {
        self.length
    }
}

fn validate_snapshot_ranges<T: SnapshotRange>(
    kind: &str,
    snapshots: &[T],
    memory_regions: &[MemoryRegion],
    allowed_memory_types: &[String],
    max_members: u64,
    max_member_bytes: u64,
    max_total_bytes: u64,
) -> Result<(), RecordingError> {
    if u64::try_from(snapshots.len()).unwrap_or(u64::MAX) > max_members {
        return Err(RecordingError::Invalid(format!(
            "{kind} count exceeds {max_members}"
        )));
    }
    let mut labels = BTreeSet::new();
    let mut total = 0u64;
    for snapshot in snapshots {
        if !crate::path_safety::is_hyphenated_ascii_id(snapshot.label(), 64)
            || !labels.insert(snapshot.label())
        {
            return Err(RecordingError::Invalid(format!(
                "{kind} labels must be unique safe identifiers of at most 64 bytes"
            )));
        }
        if snapshot.length() == 0 || snapshot.length() > max_member_bytes {
            return Err(RecordingError::Invalid(format!(
                "{kind} {} length must be in 1..={max_member_bytes}",
                snapshot.label()
            )));
        }
        if !allowed_memory_types.is_empty()
            && !allowed_memory_types
                .iter()
                .any(|memory_type| memory_type == snapshot.memory_type())
        {
            return Err(RecordingError::Unavailable(format!(
                "{kind} {} memory_type is not callback-safe",
                snapshot.label()
            )));
        }
        let region = memory_regions
            .iter()
            .find(|region| region.memory_type == snapshot.memory_type())
            .ok_or_else(|| {
                RecordingError::Invalid(format!(
                    "{kind} {} memory_type is absent from status.memory_regions",
                    snapshot.label()
                ))
            })?;
        if snapshot.address() > region.size
            || snapshot.length() > region.size.saturating_sub(snapshot.address())
        {
            return Err(RecordingError::Invalid(format!(
                "{kind} {} exceeds the exact live region size",
                snapshot.label()
            )));
        }
        total = total
            .checked_add(snapshot.length())
            .ok_or_else(|| RecordingError::Invalid(format!("{kind} total length overflow")))?;
    }
    if total > max_total_bytes {
        return Err(RecordingError::Invalid(format!(
            "{kind} bytes exceed {max_total_bytes}"
        )));
    }
    Ok(())
}

fn validate_terminal_state_request(
    capability: &RecordingCapability,
    methods: &[String],
    request: &RecordWindowRequest,
) -> Result<Option<TerminalStateRequest>, RecordingError> {
    let Some(requested_profile) = request.terminal_state_profile.as_deref() else {
        return Ok(None);
    };
    if !methods.iter().any(|method| method == "get_state") {
        return Err(RecordingError::Unavailable(
            "terminal state capture requires live get_state".into(),
        ));
    }
    let capability = capability.terminal_state.as_ref().ok_or_else(|| {
        RecordingError::Unavailable(
            "the current runtime does not advertise terminal state profiles".into(),
        )
    })?;
    let profile = capability
        .profiles
        .iter()
        .find(|profile| profile.id == requested_profile)
        .ok_or_else(|| {
            RecordingError::Unavailable(format!(
                "terminal state profile {requested_profile} is not advertised"
            ))
        })?;
    Ok(Some(TerminalStateRequest {
        profile: profile.id.clone(),
        contract_sha256: profile.contract_sha256.clone(),
    }))
}

fn validate_terminal_snapshot_request(
    capability: &RecordingCapability,
    methods: &[String],
    memory_regions: &[MemoryRegion],
    request: &RecordWindowRequest,
) -> Result<(), RecordingError> {
    if request.terminal_snapshots.is_empty() {
        return Ok(());
    }
    let bounds = capability.terminal_snapshots.as_ref().ok_or_else(|| {
        RecordingError::Unavailable(
            "the current runtime does not advertise terminal snapshots".into(),
        )
    })?;
    if !methods.iter().any(|method| method == "read_memory") {
        return Err(RecordingError::Unavailable(
            "terminal snapshots require live read_memory".into(),
        ));
    }
    validate_snapshot_ranges(
        "terminal snapshot",
        &request.terminal_snapshots,
        memory_regions,
        &[],
        bounds.max_members,
        bounds.max_member_bytes,
        bounds.max_total_bytes,
    )
}

fn narrow_limit(
    name: &str,
    effective: &mut u64,
    requested: Option<u64>,
) -> Result<(), RecordingError> {
    if let Some(requested) = requested {
        if requested == 0 || requested > *effective {
            return Err(RecordingError::Invalid(format!(
                "{name} may narrow but not expand the advertised limit"
            )));
        }
        *effective = requested;
    }
    Ok(())
}

pub(super) fn canonical_output_root(path: &Path) -> Result<PathBuf, RecordingError> {
    if !path.is_absolute() {
        return Err(RecordingError::Invalid(
            "output_root must be absolute".into(),
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RecordingError::Invalid(
            "output_root must be a real existing directory".into(),
        ));
    }
    Ok(fs::canonicalize(path)?)
}

pub(super) fn request_digest(
    root: &Path,
    origin: RecordingOrigin,
    request: &RecordingRequest,
) -> Result<String, RecordingError> {
    let root = root
        .to_str()
        .ok_or_else(|| RecordingError::Invalid("output_root must be UTF-8".into()))?;
    let bytes = serde_json::to_vec(&RequestDigest {
        output_root: root,
        origin,
        request,
    })
    .map_err(|error| RecordingError::Invalid(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub(super) fn runtime_identity(
    identity: &EmulatorIdentity,
    current: &CurrentManifest,
    capability: &RecordingCapability,
) -> Result<RuntimeIdentity, RecordingError> {
    let content_path = Path::new(&current.content);
    let metadata = fs::symlink_metadata(content_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RecordingError::Unavailable(
            "recording content must be a regular file".into(),
        ));
    }
    let (sha1, sha256) = hash_content(content_path)?;
    let host = identity
        .host_build
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| RecordingError::Unavailable("emulator host identity is missing".into()))?;
    let field = |name: &str| {
        host.get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(String::from)
            .ok_or_else(|| {
                RecordingError::Unavailable(format!("emulator host identity lacks {name}"))
            })
    };
    let upstream = field("commit")?;
    let patchset = field("patchset_sha256")?;
    let binary = field("binary_sha256")?;
    Ok(RuntimeIdentity {
        system: current.system.clone(),
        adapter_id: identity
            .adapter
            .clone()
            .ok_or_else(|| RecordingError::Unavailable("adapter identity is missing".into()))?,
        server_build: crate::build_identity::BUILD_HASH.into(),
        adapter_build: identity
            .build
            .clone()
            .ok_or_else(|| RecordingError::Unavailable("adapter build is missing".into()))?,
        emulator_id: host
            .get("upstream")
            .and_then(Value::as_str)
            .unwrap_or("emulator-host")
            .to_string(),
        emulator_build: binary,
        emulator_upstream_revision: upstream,
        emulator_patchset_sha256: patchset,
        launch_id: current.launch_id.clone(),
        capability_revision: capability.revision.clone(),
        content: ContentIdentity {
            sha1: Some(sha1),
            sha256: Some(sha256),
            bytes: metadata.len(),
            path_hint: content_path
                .file_name()
                .and_then(|value| value.to_str())
                .map(String::from),
        },
    })
}

fn hash_content(path: &Path) -> io::Result<(String, String)> {
    let mut file = crate::path_safety::open_regular_file_no_follow(path)?;
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sha1.update(&buffer[..read]);
        sha256.update(&buffer[..read]);
    }
    Ok((hex::encode(sha1.finalize()), hex::encode(sha256.finalize())))
}
