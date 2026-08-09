use std::time::{Duration, Instant};

use emucap::live::link::{EmulatorLink, LinkError};
use emucap::live::tools::{self, ToolOutput};

use crate::args::ReattachArgs;

const REATTACH_READY_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn make_reattach(
    link: &mut (dyn EmulatorLink + Send),
    args: &ReattachArgs,
) -> serde_json::Value {
    let prepared = match link.reattach_runtime(&args.launch_id) {
        Ok(prepared) => prepared,
        Err(error) => {
            return serde_json::json!({
                "reattached": false,
                "launch_id": args.launch_id,
                "reason": error.to_string(),
                "error_kind": error.kind(),
                "next_action": "Inspect runtime ownership and retry only after the former lease is returned.",
            })
        }
    };
    let deadline = Instant::now() + REATTACH_READY_TIMEOUT;
    let mut last_error: Option<String> = None;
    while Instant::now() < deadline {
        match tools::status(link) {
            Ok(ToolOutput::Json(status))
                if link.capabilities().identity.launch_id.as_deref()
                    == Some(args.launch_id.as_str()) =>
            {
                return serde_json::json!({
                    "reattached": true,
                    "launch_id": args.launch_id,
                    "listener": prepared,
                    "status": status,
                });
            }
            Ok(_) => {
                last_error =
                    Some("adapter status did not identify the requested generation".into());
            }
            Err(LinkError::NotConnected | LinkError::Timeout) => {
                last_error = Some("adapter has not reconnected yet".into());
            }
            Err(error) => {
                return serde_json::json!({
                    "reattached": false,
                    "launch_id": args.launch_id,
                    "listener": prepared,
                    "reason": error.to_string(),
                    "error_kind": error.kind(),
                    "next_action": "Inspect status; do not replace or edit runtime files until the exact generation identity is resolved.",
                })
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    serde_json::json!({
        "reattached": false,
        "launch_id": args.launch_id,
        "listener": prepared,
        "reason": last_error.unwrap_or_else(|| "adapter did not reconnect before the bounded deadline".into()),
        "next_action": "Call status again. The listener and returned lease remain bound to this exact generation; do not launch a duplicate.",
    })
}

#[cfg(test)]
#[path = "reattach_tests.rs"]
mod tests;
