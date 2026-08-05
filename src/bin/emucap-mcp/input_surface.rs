use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Map, Value};

use emucap::live::tools::ToolOutput;

use crate::args::{
    HoldTouchArgs, HoldUntilArgs, InputArgs, PressArgs, PulseTouchArgs, ReleaseTouchArgs,
    RoutedOperationArgs, TouchArgs,
};
use crate::{analysis_surface, invalid_request_result, tool_output_result, Emucap};

const OPERATIONS: &[&str] = &[
    "set_input",
    "hold_touch",
    "release_touch",
    "pulse_touch_while_running",
    "pulse_while_running",
    "hold_until",
];

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
    add::<InputArgs>(
        &mut operations,
        status,
        "set_input",
        "Hold a persistent button or key override; an empty button list releases native ownership.",
    );
    add::<HoldTouchArgs>(
        &mut operations,
        status,
        "hold_touch",
        "Hold one touch-screen coordinate until release_touch or generation termination.",
    );
    add::<ReleaseTouchArgs>(
        &mut operations,
        status,
        "release_touch",
        "Release persistent touch-screen ownership without advancing guest time.",
    );
    add::<PulseTouchArgs>(
        &mut operations,
        status,
        "pulse_touch_while_running",
        "Hold one touch-screen coordinate for a real-time frame duration, release it, and leave guest execution running.",
    );
    add::<PressArgs>(
        &mut operations,
        status,
        "pulse_while_running",
        "Hold buttons for a real-time frame duration, release them, and leave guest execution running.",
    );
    add::<HoldUntilArgs>(
        &mut operations,
        status,
        "hold_until",
        "Advance while holding buttons until watched memory changes, then release and return frozen.",
    );

    let available = status.pointer("/contracts/state").and_then(Value::as_str) == Some("validated");
    let next_action = if available {
        serde_json::json!({
            "tool": "input_control",
            "arguments": {
                "operation": "Select one key from operations.",
                "known_capability_revision": "Reuse capability_revision from this response.",
                "arguments": "Use the selected arguments_schema exactly."
            }
        })
    } else {
        serde_json::json!({
            "tool": "status",
            "arguments": {},
            "reason": "No validated live capability snapshot is cached."
        })
    };

    serde_json::json!({
        "surface": "input_control",
        "available": available,
        "capability_revision": status.get("capability_revision"),
        "input_buttons": status.get("input_buttons"),
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
                "input_control operation=describe accepts no arguments or capability revision",
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
        "input_control",
        &arguments.operation,
        arguments.known_capability_revision.as_deref(),
        supports(&arguments.operation),
        advertised(&status, &arguments.operation),
    ) {
        return error;
    }
    let operation = arguments.operation;
    match operation.as_str() {
        "set_input" => match analysis_surface::parse_arguments(&operation, arguments.arguments) {
            Ok(values) => server.set_input(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        "hold_touch" => match analysis_surface::parse_arguments::<HoldTouchArgs>(
            &operation,
            arguments.arguments,
        ) {
            Ok(values) => {
                server
                    .touch(Parameters(TouchArgs {
                        port: values.port,
                        x: Some(values.x),
                        y: Some(values.y),
                        frames: None,
                        release: false,
                    }))
                    .await
            }
            Err(error) => invalid_request_result(error),
        },
        "release_touch" => match analysis_surface::parse_arguments::<ReleaseTouchArgs>(
            &operation,
            arguments.arguments,
        ) {
            Ok(values) => {
                server
                    .touch(Parameters(TouchArgs {
                        port: values.port,
                        x: None,
                        y: None,
                        frames: None,
                        release: true,
                    }))
                    .await
            }
            Err(error) => invalid_request_result(error),
        },
        "pulse_touch_while_running" => match analysis_surface::parse_arguments::<PulseTouchArgs>(
            &operation,
            arguments.arguments,
        ) {
            Ok(values) => {
                server
                    .touch(Parameters(TouchArgs {
                        port: values.port,
                        x: Some(values.x),
                        y: Some(values.y),
                        frames: Some(values.frames),
                        release: false,
                    }))
                    .await
            }
            Err(error) => invalid_request_result(error),
        },
        "pulse_while_running" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.pulse_while_running(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        "hold_until" => match analysis_surface::parse_arguments(&operation, arguments.arguments) {
            Ok(values) => server.hold_until(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        _ => invalid_request_result(format!(
            "unknown input_control operation: {operation}; call operation=describe"
        )),
    }
}
