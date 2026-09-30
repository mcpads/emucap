//! MCP admission and reply verification for batched memory reads and host pacing.
//!
//! Both operations are forwarded to the adapter as one request after the common checks. Pacing
//! never waits behind another Control operation: a held link reports busy instead of applying a
//! late policy change after that operation finishes.
#[cfg(test)]
use std::sync::TryLockError;

use emucap::live::link::{EmulatorLink, LinkError};
use emucap::live::memory_batch::{self, BatchRange};
use emucap::live::pacing::{self, SpeedRequest};
use emucap::live::tools::ToolOutput;
use emucap::mcp_result::{error_result, link_error_result, tool_output_result};
use rmcp::model::CallToolResult;

use crate::args::{ExecutionSpeedArgs, ExecutionSpeedMode, ReadMemoryBatchArgs};
use crate::invalid_request_result;
#[cfg(test)]
use crate::SharedLink;

fn unsupported(method: &str) -> CallToolResult {
    error_result(
        "unsupported",
        format!("{method} is not advertised by the connected adapter; read full status methods"),
    )
}

pub(crate) fn read_memory_batch(
    link: &mut dyn EmulatorLink,
    args: ReadMemoryBatchArgs,
) -> CallToolResult {
    let Some(capability) = link.capabilities().features.memory_batch.clone() else {
        return unsupported(memory_batch::METHOD);
    };
    let ranges: Vec<BatchRange> = args
        .ranges
        .into_iter()
        .map(|range| BatchRange {
            memory_type: range.memory_type,
            address: range.address.get(),
            length: range.length.get(),
        })
        .collect();
    if let Err(error) = capability.admit(&ranges) {
        return invalid_request_result(error);
    }
    let generation = link.capabilities().identity.launch_id.clone();
    let reply = link
        .call(memory_batch::METHOD, serde_json::json!({"ranges": ranges}))
        .and_then(|reply| {
            memory_batch::verify_reply(&ranges, &reply, generation.as_deref()).map(|()| reply)
        });
    match reply {
        Ok(value) => tool_output_result(ToolOutput::Json(value)),
        Err(error) => link_error_result(error),
    }
}

#[cfg(test)]
pub(crate) fn execution_speed(shared: &SharedLink, args: ExecutionSpeedArgs) -> CallToolResult {
    let mut link = match shared.try_lock() {
        Ok(link) => link,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => {
            return error_result(
                "busy",
                "active_operation: another Control operation is in progress; finish or cancel it, then change pacing",
            )
        }
    };
    execution_speed_locked(&mut *link, args)
}

pub(crate) fn execution_speed_locked(
    link: &mut dyn EmulatorLink,
    args: ExecutionSpeedArgs,
) -> CallToolResult {
    let mode = args.mode.map(|mode| match mode {
        ExecutionSpeedMode::Limited => "limited",
        ExecutionSpeedMode::Unlimited => "unlimited",
    });
    let request = match pacing::parse_request(mode, args.percent) {
        Ok(request) => request,
        Err(error) => return invalid_request_result(error),
    };
    let Some(capability) = link.capabilities().features.execution_speed.clone() else {
        return unsupported(pacing::METHOD);
    };
    if let Err(error) = capability.admit(request) {
        return invalid_request_result(error);
    }
    let reply = link
        .call(pacing::METHOD, pacing::request_params(request))
        .and_then(|reply| {
            let verified = match request {
                SpeedRequest::Query => capability.verify_policy(&reply),
                _ => capability.verify_change(request, &reply),
            };
            verified.map(|()| reply).map_err(|error| {
                LinkError::Protocol(format!("invalid execution_speed reply: {error}"))
            })
        });
    match reply {
        Ok(value) => tool_output_result(ToolOutput::Json(value)),
        Err(error) => link_error_result(error),
    }
}

#[cfg(test)]
#[path = "observation_speed_tests.rs"]
mod tests;
