use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Map, Value};

use emucap::live::tools::ToolOutput;

use crate::args::{ClickPointerArgs, DragPointerArgs, MovePointerArgs, RoutedOperationArgs};
use crate::{analysis_surface, invalid_request_result, tool_output_result, Emucap};

const OPERATIONS: &[&str] = &["move_pointer", "click_pointer", "drag_pointer"];

#[cfg(test)]
pub(crate) fn operation_ids() -> &'static [&'static str] {
    OPERATIONS
}

pub(crate) fn supports(operation: &str) -> bool {
    OPERATIONS.contains(&operation)
}

pub(crate) fn advertised(status: &Value, operation: &str) -> bool {
    status.pointer("/contracts/state").and_then(Value::as_str) == Some("validated")
        && status
            .get("methods")
            .and_then(Value::as_array)
            .is_some_and(|methods| methods.iter().any(|method| method == operation))
}

fn validate_pointer_motion(
    status: &Value,
    port: u64,
    dx: i64,
    dy: i64,
    frames: u64,
) -> Result<(), String> {
    let constraints = status
        .pointer("/contracts/constraints")
        .and_then(Value::as_object);
    if constraints
        .and_then(|values| values.get("input.pointer.ports.allowed"))
        .and_then(Value::as_array)
        .is_some_and(|allowed| !allowed.iter().any(|value| value.as_u64() == Some(port)))
    {
        return Err(format!("pointer input port {port} is not advertised"));
    }
    let min = constraints
        .and_then(|values| values.get("input.pointer.delta.min"))
        .and_then(Value::as_i64);
    let max = constraints
        .and_then(|values| values.get("input.pointer.delta.max"))
        .and_then(Value::as_i64);
    if min.is_some_and(|min| dx < min || dy < min) || max.is_some_and(|max| dx > max || dy > max) {
        return Err(format!(
            "relative pointer delta ({dx},{dy}) exceeds the advertised range"
        ));
    }
    if dx == 0 && dy == 0 {
        return Err("relative pointer movement requires a non-zero delta".into());
    }
    if constraints
        .and_then(|values| values.get("input.pointer.move.max_frames"))
        .and_then(Value::as_u64)
        .is_some_and(|max_frames| frames > max_frames)
    {
        return Err(format!(
            "pointer movement frame count {frames} exceeds the advertised limit"
        ));
    }
    Ok(())
}

fn add<T: schemars::JsonSchema>(
    operations: &mut Map<String, Value>,
    status: &Value,
    id: &str,
    description: &str,
) {
    if advertised(status, id) {
        operations.insert(
            id.into(),
            serde_json::json!({
                "description": description,
                "arguments_schema": schemars::schema_for!(T),
            }),
        );
    }
}

pub(crate) fn describe(status: &Value) -> Value {
    let mut operations = Map::new();
    add::<MovePointerArgs>(
        &mut operations,
        status,
        "move_pointer",
        "Queue a signed relative pointer delta, advance an exact frame count, and return frozen; visible cursor updates follow guest polling.",
    );
    add::<ClickPointerArgs>(
        &mut operations,
        status,
        "click_pointer",
        "Click the left, right, or middle pointer button and return frozen with input released.",
    );
    add::<DragPointerArgs>(
        &mut operations,
        status,
        "drag_pointer",
        "Hold a pointer button while queueing relative movement for an exact frame count, then release and return frozen.",
    );

    let available = status.pointer("/contracts/state").and_then(Value::as_str) == Some("validated");
    let has_operations = !operations.is_empty();
    let next_action = if available && has_operations {
        serde_json::json!({
            "tool": "pointer",
            "arguments": {
                "operation": "Select one key from operations.",
                "known_capability_revision": "Reuse capability_revision from this response.",
                "arguments": "Use the selected arguments_schema exactly."
            }
        })
    } else if available {
        serde_json::json!({
            "tool": "status",
            "arguments": {},
            "reason": "This runtime has no pointer operations. Use tap for button or key input."
        })
    } else {
        serde_json::json!({
            "tool": "status",
            "arguments": {},
            "reason": "No validated live capability snapshot is cached."
        })
    };

    serde_json::json!({
        "surface": "pointer",
        "available": available && has_operations,
        "capability_revision": status.get("capability_revision"),
        "operations": operations,
        "next_action": next_action
    })
}

pub(crate) async fn execute(server: &Emucap, arguments: RoutedOperationArgs) -> CallToolResult {
    if arguments.operation == "describe" {
        if arguments
            .arguments
            .as_ref()
            .is_some_and(|values| !values.is_empty())
            || arguments.known_capability_revision.is_some()
        {
            return invalid_request_result(
                "pointer operation=describe accepts no arguments or capability revision",
            );
        }
        return tool_output_result(ToolOutput::Json(describe(
            &server.surface_description_status(),
        )));
    }

    let status = match server.current_surface_status() {
        Ok(status) => status,
        Err(error) => return error,
    };
    if let Err(error) = server.validate_routed_operation(
        &status,
        "pointer",
        &arguments.operation,
        arguments.known_capability_revision.as_deref(),
        supports(&arguments.operation),
        advertised(&status, &arguments.operation),
    ) {
        return error;
    }
    let operation = arguments.operation;
    match operation.as_str() {
        "move_pointer" => {
            match analysis_surface::parse_arguments::<MovePointerArgs>(
                &operation,
                arguments.arguments,
            ) {
                Ok(values) => match validate_pointer_motion(
                    &status,
                    values.port,
                    values.dx,
                    values.dy,
                    values.frames,
                ) {
                    Ok(()) => server.move_pointer(Parameters(values)).await,
                    Err(error) => invalid_request_result(error),
                },
                Err(error) => invalid_request_result(error),
            }
        }
        "click_pointer" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.click_pointer(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        "drag_pointer" => {
            match analysis_surface::parse_arguments::<DragPointerArgs>(
                &operation,
                arguments.arguments,
            ) {
                Ok(values) => match validate_pointer_motion(
                    &status,
                    values.port,
                    values.dx,
                    values.dy,
                    values.move_frames,
                ) {
                    Ok(()) => server.drag_pointer(Parameters(values)).await,
                    Err(error) => invalid_request_result(error),
                },
                Err(error) => invalid_request_result(error),
            }
        }
        _ => invalid_request_result(format!(
            "unknown pointer operation: {operation}; call operation=describe"
        )),
    }
}
