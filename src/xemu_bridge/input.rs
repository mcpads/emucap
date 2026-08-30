use super::*;

const XBOX_INPUT_BUTTONS: &[&str] = &[
    "a", "b", "x", "y", "white", "black", "start", "back", "up", "down", "left", "right", "l", "r",
    "lstick", "rstick",
];
const XBOX_INPUT_AXES: &[&str] = &[
    "left_x",
    "left_y",
    "right_x",
    "right_y",
    "left_trigger",
    "right_trigger",
];

pub(super) fn xbox_input_buttons_json() -> Value {
    json!({
        "system":"xbox",
        "buttons":XBOX_INPUT_BUTTONS,
        "implemented":true,
        "aliases": {"select":"back", "lt":"l", "rt":"r", "l3":"lstick", "r3":"rstick"},
        "notes":"Original Xbox controller port 0. l/r are full-trigger aliases; white and black remain distinct buttons. set_input replaces the complete guest-visible controller state, and empty buttons plus axes return ownership to native input.",
    })
}

pub(super) fn xbox_input_axes_json() -> Value {
    json!({
        "semantics": "complete_persistent_controller_state",
        "ports": [{
            "port": 0,
            "axes": {
                "left_x": {"minimum":-32768, "neutral":0, "maximum":32767, "negative_direction":"left", "positive_direction":"right"},
                "left_y": {"minimum":-32768, "neutral":0, "maximum":32767, "negative_direction":"down", "positive_direction":"up"},
                "right_x": {"minimum":-32768, "neutral":0, "maximum":32767, "negative_direction":"left", "positive_direction":"right"},
                "right_y": {"minimum":-32768, "neutral":0, "maximum":32767, "negative_direction":"down", "positive_direction":"up"},
                "left_trigger": {"minimum":0, "neutral":0, "maximum":32767},
                "right_trigger": {"minimum":0, "neutral":0, "maximum":32767}
            }
        }],
        "notes": "Axis values are persistent integer controller state. Omitted axes are neutral. l/r button aliases conflict with explicit left_trigger/right_trigger values."
    })
}

fn axis_range(name: &str) -> Option<(i64, i64)> {
    match name {
        "left_x" | "left_y" | "right_x" | "right_y" => Some((i16::MIN.into(), i16::MAX.into())),
        "left_trigger" | "right_trigger" => Some((0, i16::MAX.into())),
        _ => None,
    }
}

fn parse_input(params: &Value) -> XemuResult<InputState> {
    let port = optional_num(params, "port")?.unwrap_or(0);
    if port != 0 {
        return Err(XemuBridgeError::BadParams(format!(
            "Xbox input supports only controller port 0 (got {port})"
        )));
    }
    let values = match params.get("buttons") {
        None => &[][..],
        Some(Value::Array(values)) => values.as_slice(),
        Some(_) => {
            return Err(XemuBridgeError::BadParams(
                "buttons must be a list of strings".into(),
            ))
        }
    };
    let mut input = InputState::default();
    for value in values {
        let raw = value.as_str().ok_or_else(|| {
            XemuBridgeError::BadParams("buttons must be a list of strings".into())
        })?;
        let normalized = raw.trim().to_ascii_lowercase();
        let canonical = match normalized.as_str() {
            "select" => "back",
            "lt" | "l1" | "lb" => "l",
            "rt" | "r1" | "rb" => "r",
            "l3" => "lstick",
            "r3" => "rstick",
            other => other,
        };
        let bit = match canonical {
            "a" => Some(0),
            "b" => Some(1),
            "x" => Some(2),
            "y" => Some(3),
            "left" => Some(4),
            "up" => Some(5),
            "right" => Some(6),
            "down" => Some(7),
            "back" => Some(8),
            "start" => Some(9),
            "white" => Some(10),
            "black" => Some(11),
            "lstick" => Some(12),
            "rstick" => Some(13),
            "l" => {
                input.axes.insert("left_trigger".into(), i16::MAX);
                None
            }
            "r" => {
                input.axes.insert("right_trigger".into(), i16::MAX);
                None
            }
            other => {
                return Err(XemuBridgeError::BadParams(format!(
                    "unsupported Xbox button: {other}; valid: {}",
                    XBOX_INPUT_BUTTONS.join(", ")
                )))
            }
        };
        if let Some(bit) = bit {
            input.mask |= 1 << bit;
        }
        if !input.buttons.iter().any(|name| name == canonical) {
            input.buttons.push(canonical.into());
        }
    }
    if let Some(axis_values) = params.get("axes") {
        let axis_values = axis_values.as_object().ok_or_else(|| {
            XemuBridgeError::BadParams("axes must be an object of integer values".into())
        })?;
        for (name, raw_value) in axis_values {
            let (minimum, maximum) = axis_range(name).ok_or_else(|| {
                XemuBridgeError::BadParams(format!(
                    "unsupported Xbox controller axis: {name}; valid: {}",
                    XBOX_INPUT_AXES.join(", ")
                ))
            })?;
            let value = raw_value.as_i64().ok_or_else(|| {
                XemuBridgeError::BadParams(format!(
                    "Xbox controller axis {name} must be an integer"
                ))
            })?;
            if value < minimum || value > maximum {
                return Err(XemuBridgeError::BadParams(format!(
                    "Xbox controller axis {name} value {value} is outside {minimum}..{maximum}"
                )));
            }
            if input.axes.contains_key(name) {
                return Err(XemuBridgeError::BadParams(format!(
                    "Xbox controller axis {name} conflicts with its full-trigger button alias"
                )));
            }
            input.axes.insert(name.clone(), value as i16);
        }
    }
    Ok(input)
}

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn send_input(&mut self, engaged: bool, input: &InputState) -> XemuResult<()> {
        self.qmp.execute(
            "xemu-emucap-set-input",
            Some(json!({
                "engaged":engaged, "buttons":input.mask,
                "ltrigger":input.axes.get("left_trigger").copied().unwrap_or(0),
                "rtrigger":input.axes.get("right_trigger").copied().unwrap_or(0),
                "lstick-x":input.axes.get("left_x").copied().unwrap_or(0),
                "lstick-y":input.axes.get("left_y").copied().unwrap_or(0),
                "rstick-x":input.axes.get("right_x").copied().unwrap_or(0),
                "rstick-y":input.axes.get("right_y").copied().unwrap_or(0),
            })),
        )?;
        let observed = self.extension_status()?["input-engaged"]
            .as_bool()
            .unwrap_or(!engaged);
        if observed != engaged {
            return Err(XemuBridgeError::Emulator(format!(
                "xemu input ownership mismatch: requested engaged={engaged}, observed {observed}"
            )));
        }
        Ok(())
    }

    pub(super) fn set_input(&mut self, params: &Value) -> XemuResult<Value> {
        let input = parse_input(params)?;
        let engaged = !input.buttons.is_empty() || !input.axes.is_empty();
        self.send_input(engaged, &input)?;
        self.held_input = engaged.then_some(input.clone());
        Ok(json!({
            "buttons":input.buttons, "axes":input.axes, "port":0, "override_engaged":engaged,
            "ownership":if engaged {"emucap"} else {"native"}
        }))
    }

    pub(super) fn release_input_override(&mut self) -> XemuResult<()> {
        self.send_input(false, &InputState::default())?;
        self.held_input = None;
        Ok(())
    }
}
