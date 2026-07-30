use rmcp::model::{CallToolResult, ContentBlock as Content};
use serde_json::{json, Value};

use crate::live::link::LinkError;
use crate::live::tools::ToolOutput;

pub fn json_result(value: Value) -> CallToolResult {
    CallToolResult::structured(value)
}

/// Return a structured tool error when an operation explicitly reports that
/// its requested outcome did not occur. The original diagnostic object is
/// preserved so callers do not lose ownership, recovery, or provenance fields.
pub fn boolean_outcome_result(value: Value, success_field: &str) -> CallToolResult {
    if value.get(success_field).and_then(Value::as_bool) == Some(false) {
        CallToolResult::structured_error(value)
    } else {
        json_result(value)
    }
}

pub fn error_result(code: impl Into<String>, message: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "ok": false,
        "error": {
            "code": code.into(),
            "message": message.to_string(),
        }
    }))
}

pub fn link_error_result(error: LinkError) -> CallToolResult {
    let code = match &error {
        LinkError::Emulator { kind, .. } => kind.as_str(),
        _ => error.kind(),
    };
    error_result(code, &error)
}

pub fn tool_output_result(output: ToolOutput) -> CallToolResult {
    match output {
        ToolOutput::Json(value) => json_result(value),
        ToolOutput::Image {
            png_base64,
            saved_path,
            provenance,
        } => {
            let metadata = json!({
                "kind": "image",
                "mime_type": "image/png",
                "saved_path": saved_path,
                "provenance": provenance,
            });
            let mut result = json_result(metadata);
            result
                .content
                .insert(0, Content::image(png_base64, "image/png"));
            result
        }
    }
}

#[cfg(test)]
#[path = "mcp_result_tests.rs"]
mod tests;
