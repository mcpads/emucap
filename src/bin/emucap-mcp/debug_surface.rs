use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::{RequestContext, RoleServer};
use serde_json::{Map, Value};

use emucap::live::tools::ToolOutput;

use crate::args::{
    BreakOnResetArgs, EmptyArgs, FindPatternArgs, GetTraceArgs, PathArgs, ProbeArgs,
    RecordWindowArgs, ResolveTileArgs, RoutedOperationArgs, SetLayerEnableArgs, SetTraceArgs,
    WatchRegisterArgs,
};
use crate::{analysis_surface, invalid_request_result, recording, tool_output_result, Emucap};

const OPERATIONS: &[&str] = &[
    "dismiss_failure",
    "find_pattern",
    "dump_memory",
    "probe",
    "get_video_state",
    "resolve_tile",
    "set_layer_enable",
    "record_window",
    "watch_register",
    "set_trace",
    "get_trace",
    "call_stack",
    "break_on_reset",
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
    add::<EmptyArgs>(
        &mut operations,
        status,
        "dismiss_failure",
        "Dismiss preserved failure evidence only after it has been inspected.",
    );
    add::<FindPatternArgs>(
        &mut operations,
        status,
        "find_pattern",
        "Search an advertised memory region without transferring it in full.",
    );
    add::<PathArgs>(
        &mut operations,
        status,
        "dump_memory",
        "Write the backend's standard memory dump set to a local path.",
    );
    add::<ProbeArgs>(
        &mut operations,
        status,
        "probe",
        "Atomically restore, advance, and read one memory value.",
    );
    add::<EmptyArgs>(
        &mut operations,
        status,
        "get_video_state",
        "Read device-specific decoded video state.",
    );
    add::<ResolveTileArgs>(
        &mut operations,
        status,
        "resolve_tile",
        "Resolve an advertised video coordinate to backing character data.",
    );
    add::<SetLayerEnableArgs>(
        &mut operations,
        status,
        "set_layer_enable",
        "Read or change the persistent video-layer mask.",
    );
    add::<RecordWindowArgs>(
        &mut operations,
        status,
        "record_window",
        "Capture a negotiated bounded guest-time window and return frozen with a validated bundle.",
    );
    add::<WatchRegisterArgs>(
        &mut operations,
        status,
        "watch_register",
        "Arm a bounded persistent register-range watch.",
    );
    add::<SetTraceArgs>(
        &mut operations,
        status,
        "set_trace",
        "Enable or disable the backend's persistent execution trace.",
    );
    add::<GetTraceArgs>(
        &mut operations,
        status,
        "get_trace",
        "Read a bounded recent execution trace.",
    );
    add::<EmptyArgs>(
        &mut operations,
        status,
        "call_stack",
        "Read the current call chain at its advertised authority level.",
    );
    add::<BreakOnResetArgs>(
        &mut operations,
        status,
        "break_on_reset",
        "Arm or disarm persistent reset-handler observation.",
    );

    let available = status.pointer("/contracts/state").and_then(Value::as_str) == Some("validated");
    let next_action = if available {
        serde_json::json!({
            "tool": "debug",
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
        "surface": "debug",
        "available": available,
        "capability_revision": status.get("capability_revision"),
        "operations": operations,
        "next_action": next_action
    })
}

pub(crate) async fn execute(
    server: &Emucap,
    arguments: RoutedOperationArgs,
    context: RequestContext<RoleServer>,
) -> CallToolResult {
    if arguments.operation == "describe" {
        if arguments
            .arguments
            .as_ref()
            .is_some_and(|values| !values.is_empty())
            || arguments.known_capability_revision.is_some()
        {
            return invalid_request_result(
                "debug operation=describe accepts no arguments or capability revision",
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
        "debug",
        &arguments.operation,
        arguments.known_capability_revision.as_deref(),
        supports(&arguments.operation),
        advertised(&status, &arguments.operation),
    ) {
        return error;
    }
    let operation = arguments.operation;
    match operation.as_str() {
        "dismiss_failure" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.dismiss_failure(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        "find_pattern" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.find_pattern(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        "dump_memory" => match analysis_surface::parse_arguments(&operation, arguments.arguments) {
            Ok(values) => server.dump_memory(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        "probe" => match analysis_surface::parse_arguments(&operation, arguments.arguments) {
            Ok(values) => server.probe(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        "get_video_state" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.get_video_state(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        "resolve_tile" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.resolve_tile(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        "set_layer_enable" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.set_layer_enable(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        "record_window" => match analysis_surface::parse_arguments(&operation, arguments.arguments)
        {
            Ok(values) => {
                recording::run_record_window(Arc::clone(&server.link), values, context).await
            }
            Err(error) => invalid_request_result(error),
        },
        "watch_register" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.watch_register(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        "set_trace" => match analysis_surface::parse_arguments(&operation, arguments.arguments) {
            Ok(values) => server.set_trace(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        "get_trace" => match analysis_surface::parse_arguments(&operation, arguments.arguments) {
            Ok(values) => server.get_trace(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        "call_stack" => match analysis_surface::parse_arguments(&operation, arguments.arguments) {
            Ok(values) => server.call_stack(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        "break_on_reset" => {
            match analysis_surface::parse_arguments(&operation, arguments.arguments) {
                Ok(values) => server.break_on_reset(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            }
        }
        _ => invalid_request_result(format!(
            "unknown debug operation: {operation}; call operation=describe"
        )),
    }
}
