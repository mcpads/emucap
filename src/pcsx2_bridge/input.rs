use super::*;

pub(super) const PCSX2_INPUT_BUTTONS: &[&str] = &[
    "up", "right", "down", "left", "triangle", "circle", "cross", "square", "select", "start",
    "l1", "l2", "r1", "r2", "l3", "r3",
];

impl<T: PineTransport> Pcsx2Bridge<T> {
    pub(super) fn set_input(&mut self, params: &Value) -> BridgeResult<Value> {
        require_port_zero(params)?;
        let (mask, buttons) = buttons_to_mask(params.get("buttons"))?;
        self.command(MSG_EMUCAP_SET_INPUT, &mask.to_le_bytes())?;
        Ok(json!({
            "buttons": buttons,
            "port": 0,
            "override_engaged": mask != 0,
            "mode": if mask == 0 { "native" } else { "persistent" },
        }))
    }

    pub(super) fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        require_port_zero(params)?;
        let (mask, buttons) = buttons_to_mask(params.get("buttons"))?;
        if mask == 0 {
            return Err(Pcsx2BridgeError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1);
        if !(1..=MAX_INPUT_FRAMES).contains(&frames) {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "press_buttons frames must be in 1..={MAX_INPUT_FRAMES}, got {frames}"
            )));
        }

        let mut body = Vec::with_capacity(8);
        body.extend_from_slice(&mask.to_le_bytes());
        body.extend_from_slice(&(frames as u32).to_le_bytes());
        let payload = self.command(MSG_EMUCAP_PRESS_BUTTONS, &body)?;
        let mut cursor = SliceCursor::new(&payload);
        let terminal = cursor.u32()?;
        let elapsed = cursor.u32()? as u64;
        let state = vm_state_name(cursor.u32()?)?;
        if !cursor.is_empty() {
            return Err(Pcsx2BridgeError::Protocol(
                "press_buttons reply has trailing bytes".into(),
            ));
        }

        let input_override = self.input_override_info()?;
        if input_override
            .get("engaged")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return Err(Pcsx2BridgeError::Protocol(
                "press_buttons returned terminally while its input override remained engaged"
                    .into(),
            ));
        }

        match terminal {
            0 if elapsed == frames => Ok(json!({
                "status": "completed",
                "buttons": buttons,
                "frames": frames,
                "frames_elapsed": elapsed,
                "state": state,
                "port": 0,
                "override_engaged": false,
            })),
            0 => Err(Pcsx2BridgeError::Protocol(format!(
                "press_buttons completed after {elapsed} frames, expected {frames}"
            ))),
            1 if elapsed <= frames => Ok(json!({
                "status": "interrupted",
                "reason": "execution_stopped",
                "buttons": buttons,
                "frames": frames,
                "frames_elapsed": elapsed,
                "state": state,
                "port": 0,
                "override_engaged": false,
            })),
            value => Err(Pcsx2BridgeError::Protocol(format!(
                "invalid press_buttons terminal={value}, elapsed={elapsed}, requested={frames}"
            ))),
        }
    }

    pub(super) fn input_override_info(&mut self) -> BridgeResult<Value> {
        let payload = self.command(MSG_EMUCAP_INPUT_STATUS, &[])?;
        let mut cursor = SliceCursor::new(&payload);
        let engaged = cursor.u32()?;
        let mask = cursor.u32()?;
        let remaining = cursor.u32()?;
        let total = cursor.u32()?;
        if !cursor.is_empty() || engaged > 1 || (mask & !0xffff) != 0 || remaining > total {
            return Err(Pcsx2BridgeError::Protocol(
                "invalid PCSX2 input override status reply".into(),
            ));
        }
        let engaged = engaged != 0;
        Ok(json!({
            "observable": true,
            "authority": "emulator",
            "engaged": engaged,
            "mode": if !engaged {
                "native"
            } else if total == 0 {
                "persistent"
            } else {
                "timed"
            },
            "buttons": mask_to_buttons(mask),
            "remaining_frames": remaining,
            "total_frames": total,
        }))
    }
}

fn require_port_zero(params: &Value) -> BridgeResult<()> {
    let port = optional_num(params, "port")?.unwrap_or(0);
    if port != 0 {
        return Err(Pcsx2BridgeError::BadParams(format!(
            "PCSX2 input supports only port 0, got {port}"
        )));
    }
    Ok(())
}

fn buttons_to_mask(raw: Option<&Value>) -> BridgeResult<(u32, Vec<String>)> {
    let Some(raw) = raw else {
        return Ok((0, Vec::new()));
    };
    let values = raw
        .as_array()
        .ok_or_else(|| Pcsx2BridgeError::BadParams("buttons must be a list".into()))?;
    let mut mask = 0u32;
    let mut buttons = Vec::new();
    for value in values {
        let name = value
            .as_str()
            .map(|value| value.trim().to_ascii_lowercase())
            .ok_or_else(|| {
                Pcsx2BridgeError::BadParams("buttons must be a list of strings".into())
            })?;
        let index = PCSX2_INPUT_BUTTONS
            .iter()
            .position(|candidate| *candidate == name)
            .ok_or_else(|| {
                Pcsx2BridgeError::BadParams(format!(
                    "unsupported PS2 button `{name}`; valid: {}",
                    PCSX2_INPUT_BUTTONS.join(", ")
                ))
            })?;
        let bit = 1u32 << index;
        if mask & bit == 0 {
            mask |= bit;
            buttons.push(name);
        }
    }
    Ok((mask, buttons))
}

fn mask_to_buttons(mask: u32) -> Vec<&'static str> {
    PCSX2_INPUT_BUTTONS
        .iter()
        .enumerate()
        .filter_map(|(index, name)| (mask & (1u32 << index) != 0).then_some(*name))
        .collect()
}

fn vm_state_name(value: u32) -> BridgeResult<&'static str> {
    match value {
        0 => Ok("running"),
        1 => Ok("frozen"),
        2 => Ok("shutdown"),
        _ => Err(Pcsx2BridgeError::Protocol(format!(
            "unknown PCSX2 state value: {value}"
        ))),
    }
}
