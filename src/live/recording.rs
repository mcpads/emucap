use std::io;
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};

use super::capture_capsule::{
    CaptureCapsuleError, CaptureCapsuleRepository, CaptureLeaseIdentity, CapturePreparation,
    CaptureProgress, CaptureState, CaptureTerminalSummary, ReconcileOutcome,
};
use super::continuity::{ExecutionState, RuntimeBindingState};
use super::link::{
    AbortRequest, EmulatorLink, LinkError, ProgressCallControl, RequestCancellation,
    WorkingProgress, WorkingProgressPhase,
};
use super::recording_capability::RecordingCapabilityError;
use super::recording_input::RecordingInputError;
use super::recording_member_sink::{MemberSinkOutcome, MemberSinkServer};
use super::recording_progress::ProgressState;
use super::recording_request::{
    canonical_output_root, effective_request, request_digest, runtime_identity,
};
pub use super::recording_request::{RecordWindowRequest, RequestedRecordingLimits};
pub(super) use super::recording_sink::SinkOutcome;
use super::recording_sink::SinkServer;
use super::recording_snapshot::{
    capture_terminal_snapshots, capture_terminal_state, terminalize_snapshot_failure,
    TerminalSnapshotReadout,
};
pub(crate) use super::recording_terminal::terminal_validation;
use super::runtime::{LeaseState, ProcessState, RuntimeStore};
use crate::bundle::publish::{
    verify_published_recording, PublishedRecording, RecordingBundleInput, RecordingStaging,
};
use crate::bundle::recording::{ProducerTerminalReport, RecordingValidationInput};
use crate::bundle::recording_manifest::{
    CleanupFacts, CleanupState, EventOrder, ExecutionOutcome, FinalExecutionState, Integrity,
    LossFacts, OperationOutcome, PublicationOutcome, RecordingCounters, RecordingOrigin,
    RecordingRequest,
};
use crate::event_contracts::EventContractRegistry;

#[cfg(test)]
pub(super) use super::recording_request::default_recording_host_ms;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordWindowResult {
    pub capture_id: String,
    pub operation_outcome: OperationOutcome,
    pub execution_outcome: ExecutionOutcome,
    pub integrity: Integrity,
    pub bundle_path: String,
    pub manifest_sha256: String,
    pub frames: u64,
    pub events: u64,
    pub bytes: u64,
    pub dropped: u64,
    pub final_execution_state: FinalExecutionState,
    pub cleanup: CleanupFacts,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("recording is unavailable: {0}")]
    Unavailable(String),
    #[error("recording request is invalid: {0}")]
    Invalid(String),
    #[error("recording link failed: {0}")]
    Link(#[from] LinkError),
    #[error("recording capsule failed: {0}")]
    Capsule(#[from] CaptureCapsuleError),
    #[error("recording publication failed: {0}")]
    Publish(#[from] crate::bundle::error::PublishError),
    #[error("recording capability failed: {0}")]
    Capability(#[from] RecordingCapabilityError),
    #[error("recording event contract failed: {0}")]
    Contract(#[from] crate::event_contracts::EventContractError),
    #[error("recording input failed: {0}")]
    Input(#[from] RecordingInputError),
    #[error("recording I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("recording terminal response is invalid: {0}")]
    Terminal(String),
    #[error("recording terminal snapshot failed: {0}")]
    Snapshot(String),
    #[error("recording recovery is blocked: {0}")]
    Recovery(String),
}

pub fn record_window(
    link: &mut dyn EmulatorLink,
    store: RuntimeStore,
    request: RecordWindowRequest,
    cancellation: RequestCancellation,
    progress_observer: &mut (dyn FnMut(&WorkingProgress) + Send),
) -> Result<RecordWindowResult, RecordingError> {
    if cancellation.is_cancelled() {
        return Err(RecordingError::Link(LinkError::Cancelled));
    }
    let capability =
        link.capabilities().recording.clone().ok_or_else(|| {
            RecordingError::Unavailable("adapter did not advertise recording".into())
        })?;
    if !link
        .capabilities()
        .methods
        .iter()
        .any(|method| method == "record_window")
    {
        return Err(RecordingError::Unavailable(
            "adapter does not implement record_window".into(),
        ));
    }
    let registry = EventContractRegistry::builtin()?;
    capability.validate(&registry)?;
    let effective = effective_request(
        &capability,
        &link.capabilities().methods,
        &link.capabilities().memory_regions,
        &request,
    )?;
    let identity = link.capabilities().identity.clone();
    let launch_id = identity
        .launch_id
        .clone()
        .ok_or_else(|| RecordingError::Unavailable("live launch_id is missing".into()))?;
    let port = link
        .endpoint_port()
        .ok_or_else(|| RecordingError::Unavailable("managed direct runtime is required".into()))?;
    let current = store
        .read_current(port)?
        .ok_or_else(|| RecordingError::Unavailable("runtime generation is missing".into()))?;
    if current.launch_id != launch_id
        || current.process_state() != ProcessState::Alive
        || identity
            .content
            .as_deref()
            .is_none_or(|content| content != current.content)
    {
        return Err(RecordingError::Unavailable(
            "live adapter and runtime generation are not exactly bound".into(),
        ));
    }
    let lease = link.acquire_control_lease(&launch_id)?;
    if lease.state != LeaseState::Held {
        return Err(RecordingError::Unavailable(
            "the exact runtime control lease is not held".into(),
        ));
    }
    let snapshot = link.continuity();
    if snapshot.runtime_binding.state != RuntimeBindingState::Bound {
        return Err(RecordingError::Unavailable(
            "runtime continuity is not bound to the live generation".into(),
        ));
    }

    reconcile_abandoned_capture(link, store.clone(), port, &launch_id)?;
    if cancellation.is_cancelled() {
        return Err(RecordingError::Link(LinkError::Cancelled));
    }

    let output_root = canonical_output_root(&request.output_root)?;
    let capture_id = format!(
        "capture-{}",
        ulid::Ulid::generate().to_string().to_ascii_lowercase()
    );
    let request_digest = request_digest(&output_root, effective.origin, &effective.request)?;
    let runtime = runtime_identity(&identity, &current, &capability)?;
    let mut staging = Some(RecordingStaging::prepare(&output_root, &capture_id)?);
    let staged_movie_path = if let Some(movie) = &effective.movie {
        match staging
            .as_mut()
            .expect("staging")
            .write_input_movie(&movie.canonical_bytes, &movie.identity)
        {
            Ok(path) => Some(path),
            Err(error) => {
                let _ = staging.take().expect("staging").discard();
                return Err(error.into());
            }
        }
    } else {
        None
    };
    let repository = CaptureCapsuleRepository::new(store.clone(), port, &launch_id);
    if let Err(error) = repository.create(CapturePreparation {
        capture_id: capture_id.clone(),
        request_digest_sha256: request_digest.clone(),
        capability_revision: capability.revision.clone(),
        output_root: output_root.clone(),
        destination_path: staging
            .as_ref()
            .expect("staging")
            .destination_path()
            .to_path_buf(),
        staging_path: staging
            .as_ref()
            .expect("staging")
            .staging_path()
            .to_path_buf(),
        lease: CaptureLeaseIdentity::current(),
    }) {
        if let Some(staging) = staging.take() {
            let _ = staging.quarantine();
        }
        return Err(error.into());
    }
    if let Err(error) = repository.transition(
        &capture_id,
        CaptureState::Prepared,
        CaptureState::Arming,
        None,
    ) {
        terminalize_setup_failure(
            &repository,
            &capture_id,
            CaptureState::Prepared,
            &mut staging,
            format!("capture arming transition failed: {error}"),
        )?;
        return Err(error.into());
    }
    if cancellation.is_cancelled() {
        terminalize_setup_failure(
            &repository,
            &capture_id,
            CaptureState::Arming,
            &mut staging,
            "request was cancelled before adapter dispatch".into(),
        )?;
        return Err(RecordingError::Link(LinkError::Cancelled));
    }

    let limits = &effective.request.limits;
    let writer = match staging.as_mut().expect("staging").open_event_writer(
        limits.max_events,
        limits.max_bytes,
        limits.max_line_bytes,
    ) {
        Ok(writer) => writer,
        Err(error) => {
            terminalize_setup_failure(
                &repository,
                &capture_id,
                CaptureState::Arming,
                &mut staging,
                format!("event sink preparation failed: {error}"),
            )?;
            return Err(error.into());
        }
    };
    let sink = match SinkServer::spawn(
        writer,
        &capture_id,
        limits.max_line_bytes,
        limits.max_host_ms,
    ) {
        Ok(sink) => sink,
        Err(error) => {
            terminalize_setup_failure(
                &repository,
                &capture_id,
                CaptureState::Arming,
                &mut staging,
                format!("event sink listener failed: {error}"),
            )?;
            return Err(error.into());
        }
    };
    let member_sink = if effective.request.initial_snapshots.is_empty() {
        None
    } else {
        match MemberSinkServer::spawn(
            &capture_id,
            &effective.request.initial_snapshots,
            limits.max_host_ms,
        ) {
            Ok(sink) => Some(sink),
            Err(error) => {
                let sink_outcome = sink.cancel_unarmed();
                let reason = sink_outcome.error.map_or_else(
                    || format!("initial snapshot sink listener failed: {error}"),
                    |sink_error| {
                        format!(
                            "initial snapshot sink listener failed: {error}; event {sink_error}"
                        )
                    },
                );
                terminalize_setup_failure(
                    &repository,
                    &capture_id,
                    CaptureState::Arming,
                    &mut staging,
                    reason,
                )?;
                return Err(RecordingError::Io(error));
            }
        }
    };
    if cancellation.is_cancelled() {
        let sink_outcome = sink.cancel_unarmed();
        let member_error = member_sink
            .map(MemberSinkServer::cancel_unarmed)
            .and_then(|outcome| outcome.error);
        let reason = sink_outcome.error.map_or_else(
            || "request was cancelled before adapter dispatch".into(),
            |error| format!("request was cancelled before adapter dispatch; {error}"),
        );
        let reason = member_error.map_or(reason.clone(), |error| format!("{reason}; {error}"));
        terminalize_setup_failure(
            &repository,
            &capture_id,
            CaptureState::Arming,
            &mut staging,
            reason,
        )?;
        return Err(RecordingError::Link(LinkError::Cancelled));
    }
    let mut params = json!({
        "capture_id": capture_id,
        "launch_id": launch_id,
        "request_digest_sha256": request_digest,
        "capability_revision": capability.revision,
        "origin": effective.origin,
        "frames": effective.request.frames,
        "warmup_frames": effective.request.warmup_frames,
        "event_classes": effective.request.event_classes,
        "event_filters": effective.request.event_filters,
        "limits": effective.request.limits,
        "sink": {
            "endpoint": sink.endpoint,
            "token": sink.token,
        },
    });
    if !effective.request.event_arming.is_empty() {
        params["event_arming"] = serde_json::to_value(&effective.request.event_arming)
            .map_err(|error| RecordingError::Invalid(error.to_string()))?;
    }
    if let (Some(movie), Some(path)) = (&effective.movie, &staged_movie_path) {
        let path = path.to_str().ok_or_else(|| {
            RecordingError::Invalid("staged input movie path is not UTF-8".into())
        })?;
        params["input_movie"] = json!({
            "path": path,
            "format": movie.identity.format,
            "port": movie.identity.port,
            "frames": movie.identity.frames,
            "bytes": movie.identity.bytes,
            "sha256": movie.identity.sha256,
        });
    }
    if let Some(stop_on) = &effective.request.stop_on {
        params["stop_on"] = serde_json::to_value(stop_on)
            .map_err(|error| RecordingError::Invalid(error.to_string()))?;
    }
    if let Some(start_on) = &effective.request.start_on {
        params["start_on"] = serde_json::to_value(start_on)
            .map_err(|error| RecordingError::Invalid(error.to_string()))?;
    }
    if !effective.request.initial_snapshots.is_empty() {
        params["initial_snapshots"] = serde_json::to_value(&effective.request.initial_snapshots)
            .map_err(|error| RecordingError::Invalid(error.to_string()))?;
        let member_sink = member_sink.as_ref().expect("initial snapshot sink");
        params["member_sink"] = json!({
            "endpoint": member_sink.endpoint,
            "token": member_sink.token,
        });
    }
    let control = ProgressCallControl {
        cancellation,
        abort: Some(AbortRequest {
            method: "abort_recording".into(),
            params: json!({"capture_id": capture_id, "launch_id": launch_id}),
        }),
        max_host_ms: Some(limits.max_host_ms),
    };
    let require_explicit_frames = effective.request.input_movie.is_some()
        || effective.request.stop_on.is_some()
        || effective.request.event_classes.len() > 1
        || effective.request.warmup_frames > 0
        || effective.request.start_on.is_some();
    let mut progress_state = ProgressState::new(
        &capture_id,
        limits,
        effective
            .request
            .frames
            .saturating_add(effective.request.warmup_frames),
        require_explicit_frames,
    );
    let mut observer = |progress: &WorkingProgress| -> Result<(), LinkError> {
        progress_state.validate(progress)?;
        if progress_state.first {
            repository
                .transition(&capture_id, CaptureState::Arming, CaptureState::Armed, None)
                .map_err(capsule_link_error)?;
            progress_state.first = false;
        }
        if repository
            .read()
            .map_err(capsule_link_error)?
            .is_some_and(|capsule| capsule.state == CaptureState::Armed)
            && (progress.phase == Some(WorkingProgressPhase::Recording)
                || (progress.phase.is_none()
                    && progress
                        .frames
                        .is_some_and(|frames| frames >= effective.request.warmup_frames)))
        {
            repository
                .transition(
                    &capture_id,
                    CaptureState::Armed,
                    CaptureState::Recording,
                    None,
                )
                .map_err(capsule_link_error)?;
        }
        repository
            .update_progress(
                &capture_id,
                CaptureProgress {
                    sequence: progress.sequence,
                    frame: progress.frame,
                    frames: progress.frames,
                    events: progress.events,
                    bytes: progress.bytes,
                    observed_at_unix_ms: super::runtime::now_unix_ms(),
                },
            )
            .map_err(capsule_link_error)?;
        progress_observer(progress);
        Ok(())
    };
    let call_result = link.call_with_progress("record_window", params, &mut observer, &control);
    let prearm_rejection = matches!(call_result, Err(LinkError::Emulator { .. }));
    let sink_outcome = if prearm_rejection {
        sink.cancel_unarmed()
    } else {
        sink.finish(limits.max_host_ms)
    };
    let member_sink_outcome = member_sink.map(|sink| {
        if prearm_rejection {
            sink.cancel_unarmed()
        } else {
            sink.finish(limits.max_host_ms)
        }
    });

    let current_state = repository
        .read()?
        .ok_or_else(|| RecordingError::Terminal("capture capsule disappeared".into()))?
        .state;
    if !matches!(
        current_state,
        CaptureState::Arming | CaptureState::Armed | CaptureState::Recording
    ) {
        return Err(RecordingError::Terminal(format!(
            "capture reached unexpected state {current_state:?} before closing"
        )));
    }
    repository.transition(&capture_id, current_state, CaptureState::Closing, None)?;

    let (mut validation, wire_error) = match call_result {
        Ok(value) => {
            match terminal_validation(
                &capture_id,
                effective.origin,
                &effective.request,
                capability.class_accounting,
                value,
            ) {
                Ok(validation) => (validation, None),
                Err(error) => {
                    let link_error = LinkError::Protocol(error.to_string());
                    (
                        inferred_validation(
                            effective.origin,
                            &effective.request,
                            &sink_outcome,
                            link,
                            &link_error,
                        )?,
                        Some(link_error),
                    )
                }
            }
        }
        Err(error) => (
            inferred_validation(
                effective.origin,
                &effective.request,
                &sink_outcome,
                link,
                &error,
            )?,
            Some(error),
        ),
    };
    apply_sink_outcome(&mut validation, &sink_outcome);
    if let Some(outcome) = member_sink_outcome {
        apply_member_sink_outcome(&mut validation, &outcome);
        for snapshot in outcome.members {
            if let Err(error) = staging
                .as_mut()
                .expect("staging")
                .write_initial_snapshot(&snapshot.request, &snapshot.bytes)
            {
                validation.terminal.operation_outcome = OperationOutcome::Failed;
                validation.terminal.execution_outcome = ExecutionOutcome::AdapterError;
                validation.terminal.claimed_integrity = Integrity::Unverifiable;
                validation.terminal.reason = Some(error.to_string());
                break;
            }
        }
        if let Some(snapshot) = outcome.partial {
            if let Err(error) = staging
                .as_mut()
                .expect("staging")
                .write_initial_snapshot_prefix(&snapshot.request, &snapshot.bytes)
            {
                validation.terminal.reason = Some(error.to_string());
            }
        }
    }
    if effective.request.terminal_snapshots.is_empty() && effective.request.terminal_state.is_none()
    {
        repository.transition(
            &capture_id,
            CaptureState::Closing,
            CaptureState::Finalizing,
            None,
        )?;
    } else {
        repository.transition(
            &capture_id,
            CaptureState::Closing,
            CaptureState::FrozenReadout,
            None,
        )?;
        if wire_error.is_some()
            || validation.f_start == validation.f_end
            || sink_outcome.first_frame.is_none()
            || validation.terminal.operation_outcome != OperationOutcome::Completed
            || validation.terminal.claimed_integrity != Integrity::Complete
            || validation.terminal.final_execution_state != FinalExecutionState::Frozen
        {
            let error = RecordingError::Snapshot(
                "recording did not reach a complete frozen terminal boundary".into(),
            );
            terminalize_snapshot_failure(
                &repository,
                &capture_id,
                &mut staging,
                &mut validation,
                error.to_string(),
            )?;
            return Err(error);
        }
        let readout = TerminalSnapshotReadout {
            store: &store,
            port,
            launch_id: &launch_id,
            capability_revision: &capability.revision,
            final_frame: validation.terminal.final_frame,
            cancellation: &control.cancellation,
        };
        let captured =
            match capture_terminal_snapshots(link, &effective.request.terminal_snapshots, &readout)
            {
                Ok(captured) => captured,
                Err(error) => {
                    terminalize_snapshot_failure(
                        &repository,
                        &capture_id,
                        &mut staging,
                        &mut validation,
                        error.to_string(),
                    )?;
                    return Err(error);
                }
            };
        for snapshot in captured {
            if control.cancellation.is_cancelled() {
                let error = RecordingError::Snapshot(
                    "request was cancelled during frozen snapshot staging".into(),
                );
                terminalize_snapshot_failure(
                    &repository,
                    &capture_id,
                    &mut staging,
                    &mut validation,
                    error.to_string(),
                )?;
                return Err(error);
            }
            if let Err(error) = staging
                .as_mut()
                .expect("staging")
                .write_terminal_snapshot(&snapshot.request, &snapshot.bytes)
            {
                terminalize_snapshot_failure(
                    &repository,
                    &capture_id,
                    &mut staging,
                    &mut validation,
                    error.to_string(),
                )?;
                return Err(RecordingError::Publish(error));
            }
        }
        if let Some(request) = effective.request.terminal_state.as_ref() {
            let captured = match capture_terminal_state(
                link,
                request,
                capability
                    .terminal_state
                    .as_ref()
                    .expect("effective terminal state capability"),
                &readout,
            ) {
                Ok(captured) => captured,
                Err(error) => {
                    terminalize_snapshot_failure(
                        &repository,
                        &capture_id,
                        &mut staging,
                        &mut validation,
                        error.to_string(),
                    )?;
                    return Err(error);
                }
            };
            if let Err(error) = staging
                .as_mut()
                .expect("staging")
                .write_terminal_state(&captured.request, &captured.bytes)
            {
                terminalize_snapshot_failure(
                    &repository,
                    &capture_id,
                    &mut staging,
                    &mut validation,
                    error.to_string(),
                )?;
                return Err(RecordingError::Publish(error));
            }
        }
        repository.transition(
            &capture_id,
            CaptureState::FrozenReadout,
            CaptureState::Finalizing,
            None,
        )?;
    }

    if validation.f_start == validation.f_end || sink_outcome.first_frame.is_none() {
        let terminal = terminal_summary(
            &validation.terminal,
            PublicationOutcome::Failed,
            None,
            None,
            wire_error
                .as_ref()
                .map(ToString::to_string)
                .or(sink_outcome.error),
        );
        repository.transition(
            &capture_id,
            CaptureState::Finalizing,
            CaptureState::PublicationFailed,
            Some(terminal),
        )?;
        if let Some(staging) = staging.take() {
            let _ = staging.quarantine();
        }
        return Err(wire_error.map_or_else(
            || RecordingError::Terminal("recording produced no verifiable frame origin".into()),
            RecordingError::Link,
        ));
    }

    let input = RecordingBundleInput {
        capture_id: capture_id.clone(),
        created_at_unix_ms: super::runtime::now_unix_ms(),
        request_digest_sha256: request_digest,
        runtime,
        event_order: capability.event_order.map(|order| match order {
            super::recording_capability::RecordingEventOrder::GuestEmission => {
                EventOrder::GuestEmission
            }
        }),
        validation,
    };
    let producer_terminal = input.validation.terminal.clone();
    let published = staging
        .take()
        .expect("staging consumed once")
        .publish(&registry, input);
    let published = match published {
        Ok(published) => published,
        Err(error) => {
            let terminal = failed_publication_summary(&producer_terminal, error.to_string());
            repository.transition(
                &capture_id,
                CaptureState::Finalizing,
                CaptureState::PublicationFailed,
                Some(terminal),
            )?;
            return Err(RecordingError::Publish(error));
        }
    };
    finish_published(&repository, &capture_id, published)
}

pub(super) fn apply_sink_outcome(validation: &mut RecordingValidationInput, outcome: &SinkOutcome) {
    if outcome.truncated {
        validation.terminal.operation_outcome = OperationOutcome::Failed;
        validation.terminal.execution_outcome = ExecutionOutcome::AdapterError;
        validation.terminal.claimed_integrity = Integrity::Unverifiable;
        validation.terminal.loss.truncated = true;
        validation.terminal.reason = Some(match validation.terminal.reason.take() {
            Some(reason) => format!("{reason}; host sink received a partial final record"),
            None => "host sink received a partial final record".into(),
        });
    }
    if let Some(error) = outcome.error.as_ref() {
        validation.terminal.operation_outcome = OperationOutcome::Failed;
        validation.terminal.execution_outcome = ExecutionOutcome::AdapterError;
        validation.terminal.claimed_integrity = Integrity::Unverifiable;
        validation.terminal.cleanup.sink = CleanupState::Unverifiable;
        validation.terminal.reason = Some(match validation.terminal.reason.take() {
            Some(reason) => format!("{reason}; {error}"),
            None => error.clone(),
        });
    }
}

fn apply_member_sink_outcome(
    validation: &mut RecordingValidationInput,
    outcome: &MemberSinkOutcome,
) {
    if let Some(error) = outcome.error.as_ref() {
        validation.terminal.operation_outcome = OperationOutcome::Failed;
        validation.terminal.execution_outcome = ExecutionOutcome::AdapterError;
        validation.terminal.claimed_integrity = Integrity::Unverifiable;
        validation.terminal.cleanup.sink = CleanupState::Unverifiable;
        validation.terminal.reason = Some(match validation.terminal.reason.take() {
            Some(reason) => format!("{reason}; {error}"),
            None => error.clone(),
        });
    }
}

pub(crate) fn terminalize_setup_failure(
    repository: &CaptureCapsuleRepository,
    capture_id: &str,
    state: CaptureState,
    staging: &mut Option<RecordingStaging>,
    reason: String,
) -> Result<(), RecordingError> {
    let terminal = CaptureTerminalSummary {
        operation_outcome: OperationOutcome::Failed,
        execution_outcome: ExecutionOutcome::AdapterError,
        integrity: Integrity::Unverifiable,
        publication: PublicationOutcome::Failed,
        final_execution_state: FinalExecutionState::Unknown,
        final_frame: 0,
        counters: RecordingCounters {
            frames: 0,
            events: 0,
            bytes: 0,
            dropped: 0,
        },
        cleanup: CleanupFacts {
            hooks: CleanupState::NotAcquired,
            transient_input: CleanupState::NotAcquired,
            sink: CleanupState::NotAcquired,
        },
        stop_event: None,
        bundle_path: None,
        manifest_sha256: None,
        reason: Some(reason),
    };
    repository.transition(
        capture_id,
        state,
        CaptureState::PublicationFailed,
        Some(terminal),
    )?;
    if let Some(staging) = staging.take() {
        let _ = staging.quarantine();
    }
    Ok(())
}

pub fn reconcile_abandoned_capture(
    link: &mut dyn EmulatorLink,
    store: RuntimeStore,
    port: u16,
    launch_id: &str,
) -> Result<Option<ReconcileOutcome>, RecordingError> {
    let repository = CaptureCapsuleRepository::new(store, port, launch_id);
    let Some(capsule) = repository.read()? else {
        return Ok(None);
    };
    if capsule.state.is_terminal() {
        return Ok(None);
    }

    let lease = link.acquire_control_lease(launch_id)?;
    if lease.state != LeaseState::Held {
        return Err(RecordingError::Recovery(
            "the exact generation lease is not held for capture recovery".into(),
        ));
    }
    let current_lease = CaptureLeaseIdentity::current();
    let registry = EventContractRegistry::builtin()?;
    let destination = Path::new(&capsule.destination_path);
    let verified = if destination.exists() {
        Some(
            verify_published_recording(destination, &registry).map_err(|error| {
                RecordingError::Recovery(format!(
                    "the capture destination exists but is not the exact verified bundle: {error}"
                ))
            })?,
        )
    } else {
        None
    };

    let adapter_terminal = if link.capabilities().identity.launch_id.as_deref() == Some(launch_id) {
        match link.call("status", json!({})) {
            Ok(status) => adapter_terminal_from_status(&status, &capsule.capture_id)?,
            Err(_) => None,
        }
    } else {
        None
    };
    let outcome = repository.reconcile(&current_lease, verified.as_ref(), adapter_terminal)?;
    Ok(Some(outcome))
}

pub(super) fn adapter_terminal_from_status(
    status: &Value,
    capture_id: &str,
) -> Result<Option<CaptureTerminalSummary>, RecordingError> {
    let Some(last) = status
        .get("recording")
        .and_then(|recording| recording.get("last"))
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    if last.get("capture_id").and_then(Value::as_str) != Some(capture_id) {
        return Ok(None);
    }
    let mut terminal = last.clone();
    terminal.remove("capture_id");
    // Publication is Core-owned and therefore absent from the adapter's terminal observation.
    // During abandoned-capsule reconciliation it remains unpublished until an exact bundle is
    // independently verified.
    terminal.insert("publication".into(), json!("failed"));
    serde_json::from_value(Value::Object(terminal))
        .map(Some)
        .map_err(|error| RecordingError::Recovery(format!("adapter terminal is invalid: {error}")))
}

fn inferred_validation(
    origin: RecordingOrigin,
    request: &RecordingRequest,
    sink: &SinkOutcome,
    link: &dyn EmulatorLink,
    error: &LinkError,
) -> Result<RecordingValidationInput, RecordingError> {
    let f_origin = sink.first_frame.unwrap_or(0);
    let f_start = f_origin
        .checked_add(request.warmup_frames)
        .ok_or_else(|| RecordingError::Terminal("inferred warmup scope overflow".into()))?;
    let f_end = f_start
        .checked_add(request.frames)
        .ok_or_else(|| RecordingError::Terminal("inferred frame scope overflow".into()))?;
    let snapshot = link.continuity();
    let terminated = snapshot.execution.state == ExecutionState::Exited;
    let generation_changed = link.capabilities().identity.launch_id.as_deref()
        != snapshot.runtime_binding.current_launch_id.as_deref();
    let rejected_before_arm = matches!(error, LinkError::Emulator { .. })
        && sink.events == 0
        && sink.bytes == 0
        && sink.first_frame.is_none()
        && sink.last_frame.is_none()
        && !sink.truncated
        && sink.error.is_none();
    Ok(RecordingValidationInput {
        request: request.clone(),
        origin,
        f_start,
        f_end,
        observation_start: None,
        terminal: ProducerTerminalReport {
            operation_outcome: if matches!(error, LinkError::Cancelled) {
                OperationOutcome::Aborted
            } else {
                OperationOutcome::Failed
            },
            execution_outcome: if generation_changed {
                ExecutionOutcome::GenerationChanged
            } else if terminated {
                ExecutionOutcome::EmulatorExited
            } else {
                ExecutionOutcome::AdapterError
            },
            claimed_integrity: Integrity::Unverifiable,
            final_execution_state: if terminated {
                FinalExecutionState::Terminated
            } else {
                FinalExecutionState::Unknown
            },
            final_frame: sink
                .last_frame
                .map_or(f_start, |frame| frame.saturating_add(1)),
            f_origin: (request.warmup_frames > 0).then_some(f_origin),
            counters: RecordingCounters {
                frames: sink.events,
                events: sink.events,
                bytes: sink.bytes,
                dropped: if rejected_before_arm {
                    0
                } else {
                    request.frames.saturating_sub(sink.events)
                },
            },
            loss: LossFacts {
                dropped: if rejected_before_arm {
                    0
                } else {
                    request.frames.saturating_sub(sink.events)
                },
                truncated: sink.truncated,
                first_sequence_gap: None,
            },
            cleanup: if rejected_before_arm {
                CleanupFacts {
                    hooks: CleanupState::NotAcquired,
                    transient_input: CleanupState::NotAcquired,
                    sink: CleanupState::NotAcquired,
                }
            } else if terminated {
                CleanupFacts {
                    hooks: CleanupState::GenerationTerminated,
                    transient_input: CleanupState::GenerationTerminated,
                    sink: CleanupState::GenerationTerminated,
                }
            } else {
                CleanupFacts {
                    hooks: CleanupState::Unverifiable,
                    transient_input: CleanupState::Unverifiable,
                    sink: CleanupState::Released,
                }
            },
            stop_event: None,
            reason: Some(error.to_string()),
            event_classes: Vec::new(),
        },
    })
}

pub(super) fn terminal_summary(
    report: &ProducerTerminalReport,
    publication: PublicationOutcome,
    bundle_path: Option<String>,
    manifest_sha256: Option<String>,
    reason: Option<String>,
) -> CaptureTerminalSummary {
    CaptureTerminalSummary {
        operation_outcome: report.operation_outcome,
        execution_outcome: report.execution_outcome,
        integrity: report.claimed_integrity,
        publication,
        final_execution_state: report.final_execution_state,
        final_frame: report.final_frame,
        counters: report.counters.clone(),
        cleanup: report.cleanup.clone(),
        stop_event: report.stop_event.clone(),
        bundle_path,
        manifest_sha256,
        reason: reason.or_else(|| report.reason.clone()),
    }
}

pub(super) fn failed_publication_summary(
    report: &ProducerTerminalReport,
    reason: String,
) -> CaptureTerminalSummary {
    CaptureTerminalSummary {
        operation_outcome: report.operation_outcome,
        execution_outcome: report.execution_outcome,
        integrity: Integrity::Unverifiable,
        publication: PublicationOutcome::Failed,
        final_execution_state: report.final_execution_state,
        final_frame: report.final_frame,
        counters: report.counters.clone(),
        cleanup: report.cleanup.clone(),
        stop_event: report.stop_event.clone(),
        bundle_path: None,
        manifest_sha256: None,
        reason: Some(reason),
    }
}

fn finish_published(
    repository: &CaptureCapsuleRepository,
    capture_id: &str,
    published: PublishedRecording,
) -> Result<RecordWindowResult, RecordingError> {
    let manifest = &published.manifest;
    let terminal = CaptureTerminalSummary {
        operation_outcome: manifest.terminal.operation_outcome,
        execution_outcome: manifest.terminal.execution_outcome,
        integrity: manifest.terminal.integrity,
        publication: PublicationOutcome::Published,
        final_execution_state: manifest.terminal.final_execution_state,
        final_frame: manifest.terminal.final_frame,
        counters: manifest.counters.clone(),
        cleanup: manifest.cleanup.clone(),
        stop_event: manifest.terminal.stop_event.clone(),
        bundle_path: Some(published.bundle_path.display().to_string()),
        manifest_sha256: Some(published.manifest_sha256.clone()),
        reason: manifest.terminal.reason.clone(),
    };
    repository.transition(
        capture_id,
        CaptureState::Finalizing,
        CaptureState::Published,
        Some(terminal),
    )?;
    Ok(RecordWindowResult {
        capture_id: capture_id.into(),
        operation_outcome: manifest.terminal.operation_outcome,
        execution_outcome: manifest.terminal.execution_outcome,
        integrity: manifest.terminal.integrity,
        bundle_path: published.bundle_path.display().to_string(),
        manifest_sha256: published.manifest_sha256,
        frames: manifest.counters.frames,
        events: manifest.counters.events,
        bytes: manifest.counters.bytes,
        dropped: manifest.loss.dropped,
        final_execution_state: manifest.terminal.final_execution_state,
        cleanup: manifest.cleanup.clone(),
    })
}

fn capsule_link_error(error: CaptureCapsuleError) -> LinkError {
    LinkError::Protocol(format!("capture capsule: {error}"))
}
