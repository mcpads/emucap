use super::add;
use crate::args::{
    HoldTouchArgs, HoldUntilArgs, InputArgs, PressArgs, PulseTouchArgs, ReleaseTouchArgs, TouchArgs,
};
use crate::{analysis_surface, invalid_request_result, Emucap};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

fn validate_controller_axes(
    status: &Value,
    port: u64,
    axes: &BTreeMap<String, i64>,
) -> Result<(), String> {
    if axes.is_empty() {
        return Ok(());
    }
    let ports = status
        .pointer("/input_axes/ports")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "controller axes are not advertised by the current runtime; omit axes or call debug(operation=describe) again".to_string()
        })?;
    let matching_ports = ports
        .iter()
        .filter(|entry| entry.get("port").and_then(Value::as_u64) == Some(port))
        .collect::<Vec<_>>();
    if matching_ports.len() != 1 {
        return Err(format!(
            "controller axes are not advertised for port {port}; use an advertised port"
        ));
    }
    let advertised = matching_ports[0]
        .get("axes")
        .and_then(Value::as_object)
        .ok_or_else(|| "the current runtime advertised malformed controller axes".to_string())?;
    for (name, value) in axes {
        let specification = advertised
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("controller axis {name} is not advertised for port {port}"))?;
        let minimum = specification
            .get("minimum")
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("controller axis {name} has no valid minimum"))?;
        let maximum = specification
            .get("maximum")
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("controller axis {name} has no valid maximum"))?;
        if minimum > maximum {
            return Err(format!(
                "controller axis {name} advertises an invalid range {minimum}..{maximum}"
            ));
        }
        if *value < minimum || *value > maximum {
            return Err(format!(
                "controller axis {name} value {value} is outside the advertised range {minimum}..{maximum}"
            ));
        }
    }
    Ok(())
}

pub(super) fn describe(status: &Value, operations: &mut Map<String, Value>) {
    add::<InputArgs>(
        operations,
        status,
        "set_input",
        "Hold a persistent button, key, or advertised controller-axis state; empty buttons and axes release native ownership.",
    );
    add::<HoldTouchArgs>(
        operations,
        status,
        "hold_touch",
        "Hold one touch-screen coordinate until release_touch or generation termination.",
    );
    add::<ReleaseTouchArgs>(
        operations,
        status,
        "release_touch",
        "Release persistent touch-screen ownership without advancing guest time.",
    );
    add::<PulseTouchArgs>(
        operations,
        status,
        "pulse_touch_while_running",
        "Hold one touch-screen coordinate for a real-time frame duration, release it, and leave guest execution running.",
    );
    add::<PressArgs>(
        operations,
        status,
        "pulse_while_running",
        "Hold buttons for a real-time frame duration, release them, and leave guest execution running.",
    );
    add::<HoldUntilArgs>(
        operations,
        status,
        "hold_until",
        "Advance while holding buttons until watched memory changes, then release and return frozen.",
    );
}

pub(crate) async fn execute(
    server: &Emucap,
    status: &Value,
    operation: &str,
    arguments: Option<Map<String, Value>>,
) -> CallToolResult {
    match operation {
        "set_input" => match analysis_surface::parse_arguments::<InputArgs>(operation, arguments) {
            Ok(values) => match validate_controller_axes(status, values.port, &values.axes) {
                Ok(()) => server.set_input(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            },
            Err(error) => invalid_request_result(error),
        },
        "hold_touch" => {
            match analysis_surface::parse_arguments::<HoldTouchArgs>(operation, arguments) {
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
            }
        }
        "release_touch" => {
            match analysis_surface::parse_arguments::<ReleaseTouchArgs>(operation, arguments) {
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
            }
        }
        "pulse_touch_while_running" => {
            match analysis_surface::parse_arguments::<PulseTouchArgs>(operation, arguments) {
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
            }
        }
        "pulse_while_running" => match analysis_surface::parse_arguments(operation, arguments) {
            Ok(values) => server.pulse_while_running(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        "hold_until" => match analysis_surface::parse_arguments(operation, arguments) {
            Ok(values) => server.hold_until(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        _ => invalid_request_result(format!("unknown debug input operation: {operation}")),
    }
}

#[cfg(test)]
#[path = "debug_input_tests.rs"]
mod tests;
