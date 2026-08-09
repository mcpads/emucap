use std::path::PathBuf;

use emucap::bundle::recording_manifest::{
    EventClassFilter, EventFilterTerm, EventStartCondition, EventStopCondition,
    InitialSnapshotRequest, RecordingOrigin, TerminalSnapshotRequest,
};
use emucap::live::link::{RequestCancellation, WorkingProgress};
use emucap::live::recording::{self, RecordWindowRequest, RequestedRecordingLimits};
use emucap::live::runtime::RuntimeStore;
use rmcp::model::{CallToolResult, ProgressNotificationParam, ProgressToken};
use rmcp::service::{RequestContext, RoleServer};

use crate::args::{RecordWindowArgs, RecordWindowFilterTermArgs, RecordWindowOriginArgs};
use crate::{error_result, tool_output_result, SharedLink, ToolOutput};

#[cfg(test)]
#[path = "recording_tests.rs"]
mod tests;

struct CancelOnDrop(RequestCancellation);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn progress_notification(
    token: ProgressToken,
    progress: &WorkingProgress,
    total_frames: u64,
    completed_frames: u64,
) -> ProgressNotificationParam {
    ProgressNotificationParam::new(token, completed_frames as f64)
        .with_total(total_frames as f64)
        .with_message(format!(
            "capture {}{}: {completed_frames} frames, frame {}, {} events, {} bytes",
            progress.capture_id,
            progress
                .phase
                .map(|phase| format!(" ({phase:?})").to_ascii_lowercase())
                .unwrap_or_default(),
            progress.frame,
            progress.events,
            progress.bytes
        ))
}

fn next_progress_value(
    progress: &WorkingProgress,
    total_frames: u64,
    last_notified: Option<u64>,
) -> Option<u64> {
    let completed_frames = progress.frames.unwrap_or(progress.events).min(total_frames);
    last_notified
        .is_none_or(|previous| completed_frames > previous)
        .then_some(completed_frames)
}

pub(crate) async fn run_record_window(
    link: SharedLink,
    args: RecordWindowArgs,
    context: RequestContext<RoleServer>,
) -> CallToolResult {
    let request = RecordWindowRequest {
        output_root: PathBuf::from(args.output_root),
        frames: args.frames,
        warmup_frames: args.warmup_frames,
        event_classes: args.event_classes,
        event_filters: args
            .event_filters
            .into_iter()
            .map(|filter| EventClassFilter {
                event_class: filter.event_class,
                terms: filter
                    .terms
                    .into_iter()
                    .map(|term| match term {
                        RecordWindowFilterTermArgs::U64Range {
                            path,
                            start,
                            length,
                        } => EventFilterTerm::U64Range {
                            path,
                            start: start.get(),
                            length: length.get(),
                        },
                    })
                    .collect(),
            })
            .collect(),
        origin: args.origin.map(|origin| match origin {
            RecordWindowOriginArgs::NextFrameBoundary => RecordingOrigin::NextFrameBoundary,
            RecordWindowOriginArgs::ResetRelease => RecordingOrigin::ResetRelease,
        }),
        input_path: args.input_path.map(PathBuf::from),
        stop_on: args.stop_on.map(|stop| EventStopCondition {
            event_class: stop.event_class,
            occurrence: stop.occurrence,
        }),
        start_on: args.start_on.map(|start| EventStartCondition {
            event_class: start.event_class,
        }),
        initial_snapshots: args
            .initial_snapshots
            .into_iter()
            .map(|snapshot| InitialSnapshotRequest {
                label: snapshot.label,
                memory_type: snapshot.memory_type,
                address: snapshot.address.get(),
                length: snapshot.length.get(),
            })
            .collect(),
        terminal_snapshots: args
            .terminal_snapshots
            .into_iter()
            .map(|snapshot| TerminalSnapshotRequest {
                label: snapshot.label,
                memory_type: snapshot.memory_type,
                address: snapshot.address.get(),
                length: snapshot.length.get(),
            })
            .collect(),
        terminal_state_profile: args.terminal_state_profile,
        require_repeatable: args.require_repeatable,
        limits: args.limits.map(|limits| RequestedRecordingLimits {
            max_events: limits.max_events,
            max_bytes: limits.max_bytes,
            max_host_ms: limits.max_host_ms,
        }),
    };
    let total_frames = request.frames.saturating_add(request.warmup_frames);
    let cancellation = RequestCancellation::default();
    // If the MCP request future itself disappears, the blocking worker must still observe an
    // exact request-scoped cancellation instead of continuing as an orphaned temporal operation.
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let worker_cancellation = cancellation.clone();
    let (progress_tx, mut progress_rx) = tokio::sync::watch::channel(None::<WorkingProgress>);
    let worker_link = link;
    let mut worker = tokio::task::spawn_blocking(move || {
        let mut link = worker_link
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        recording::record_window(
            &mut *link,
            RuntimeStore::discover(),
            request,
            worker_cancellation,
            &mut |progress| {
                progress_tx.send_replace(Some(progress.clone()));
            },
        )
    });

    let progress_token = context.meta.get_progress_token();
    let mut cancellation_observed = false;
    let mut progress_open = true;
    let mut last_progress_notified = None;
    loop {
        tokio::select! {
            result = &mut worker => {
                return match result {
                    Ok(Ok(result)) => match serde_json::to_value(result) {
                        Ok(value) => tool_output_result(ToolOutput::Json(value)),
                        Err(error) => error_result("recording_result", error),
                    },
                    Ok(Err(error)) => error_result("recording_failed", error),
                    Err(error) => error_result("recording_worker", error),
                };
            }
            changed = progress_rx.changed(), if progress_open => {
                if changed.is_err() {
                    progress_open = false;
                    continue;
                }
                // Mark the watched value as observed even when this request has no
                // MCP progress token. Otherwise `changed()` remains immediately ready
                // and can spin while the blocking recording worker is still active.
                let Some(progress) = progress_rx.borrow_and_update().clone() else { continue };
                if cancellation_observed || context.ct.is_cancelled() {
                    cancellation.cancel();
                    cancellation_observed = true;
                    continue;
                }
                let Some(token) = progress_token.clone() else { continue };
                let Some(value) = next_progress_value(
                    &progress,
                    total_frames,
                    last_progress_notified,
                ) else { continue };
                let notification = progress_notification(token, &progress, total_frames, value);
                // On stdio, notifications/cancelled is the request terminal signal. Prefer it over
                // an equally-ready progress write and never emit another request-scoped message.
                tokio::select! {
                    biased;
                    _ = context.ct.cancelled() => {
                        cancellation.cancel();
                        cancellation_observed = true;
                    }
                    result = context.peer.notify_progress(notification) => {
                        if result.is_err() {
                            cancellation.cancel();
                            cancellation_observed = true;
                        } else {
                            last_progress_notified = Some(value);
                        }
                    }
                }
            }
            _ = context.ct.cancelled(), if !cancellation_observed => {
                cancellation.cancel();
                cancellation_observed = true;
            }
        }
    }
}
