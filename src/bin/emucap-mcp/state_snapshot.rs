use crate::{
    args::SaveStateArgs, error_result, link_error_result, tool_output_result, SharedLink,
    ToolOutput,
};
use emucap::live::{link::RequestCancellation, runtime::RuntimeStore, snapshot, tools};
use rmcp::{
    model::{CallToolResult, ProgressNotificationParam},
    service::{RequestContext, RoleServer},
};
use std::time::Duration;

struct CancelOnDrop(RequestCancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub async fn save(
    link: SharedLink,
    args: SaveStateArgs,
    context: RequestContext<RoleServer>,
) -> CallToolResult {
    if args.preserve_for_recording && args.snapshot_key.is_some() {
        return error_result(
            "bad_params",
            "snapshot_key and preserve_for_recording are distinct receipt classes",
        );
    }
    let cancellation = RequestCancellation::default();
    let _guard = CancelOnDrop(cancellation.clone());
    let worker_cancel = cancellation.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let mut link = link.lock().unwrap_or_else(|e| e.into_inner());
        if args.preserve_for_recording {
            tools::save_state_for_recording(&mut *link, &args.path)
        } else if snapshot::supported(&*link) {
            let key = args.snapshot_key.as_deref().ok_or_else(|| {
                emucap::live::link::LinkError::Protocol(
                    "snapshot_key is required before instruction snapshot saving".into(),
                )
            })?;
            snapshot::save_with_cancellation(
                &mut *link,
                &args.path,
                key,
                &RuntimeStore::discover(),
                worker_cancel,
            )
        } else {
            tools::save_state_with_key(&mut *link, &args.path, args.snapshot_key.as_deref())
        }
    });
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.tick().await;
    let mut elapsed = 0;
    let mut cancelled = false;
    loop {
        tokio::select! {
            biased;
            result = &mut worker => return match result {
                Ok(Ok(ToolOutput::Json(v))) if v["status"] == "failed" => CallToolResult::structured_error(v),
                Ok(Ok(output)) => tool_output_result(output),
                Ok(Err(e)) => link_error_result(e),
                Err(e) => error_result("snapshot_worker", e),
            },
            _ = context.ct.cancelled(), if !cancelled => { cancellation.cancel(); cancelled = true; },
            _ = ticks.tick(), if !cancelled => {
                elapsed += 1;
                if let Some(token) = context.meta.get_progress_token() {
                    let note = ProgressNotificationParam::new(token, elapsed as f64).with_message("Saving snapshot");
                    if !matches!(tokio::time::timeout(Duration::from_secs(1), context.peer.notify_progress(note)).await, Ok(Ok(()))) {
                        cancellation.cancel(); cancelled = true;
                    }
                }
            }
        }
    }
}
