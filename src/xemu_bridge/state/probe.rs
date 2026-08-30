use super::*;

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(in crate::xemu_bridge) fn probe(&mut self, params: &Value) -> XemuResult<Value> {
        let state = required_str(params, "state")?.to_string();
        let requested_frames = optional_num(params, "frame")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(0);
        if requested_frames > crate::live::temporal::MAX_SYNC_ADVANCE_COUNT {
            return Err(XemuBridgeError::BadParams(format!(
                "probe frame count must be in 0..={}, got {requested_frames}",
                crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
            )));
        }

        let length_u64 = required_num(params, "length")?;
        let length = usize::try_from(length_u64).map_err(|_| {
            XemuBridgeError::BadParams("probe memory length does not fit this host".into())
        })?;
        if length == 0 || length > MAX_MEMORY_TRANSFER {
            return Err(XemuBridgeError::BadParams(format!(
                "probe read length must be in 1..={MAX_MEMORY_TRANSFER:#x}, got {length:#x}"
            )));
        }
        // Validate the complete observation range before load_state can mutate the VM.
        region_address(params, length_u64)?;

        let loaded = self.load_state(&json!({"path":state}))?;
        let start_frame = loaded["frame"].as_u64().ok_or_else(|| {
            XemuBridgeError::Emulator("Xbox state load returned no frame boundary".into())
        })?;
        let advance = if requested_frames == 0 {
            json!({
                "status":"completed", "count":0, "requested":0,
                "start_frame":start_frame, "end_frame":start_frame,
                "clock":"nv2a_display_update_accepted_while_running", "state":"frozen"
            })
        } else {
            self.step_frames(&json!({"count":requested_frames}))?
        };
        let observed = self.read_memory(params)?;
        let completed_frames = advance["count"].as_u64().ok_or_else(|| {
            XemuBridgeError::Emulator("Xbox probe advance returned no completed count".into())
        })?;
        let status = advance["status"].as_str().ok_or_else(|| {
            XemuBridgeError::Emulator("Xbox probe advance returned no terminal status".into())
        })?;
        let input = match self.held_input.as_ref() {
            Some(input) => json!({
                "ownership":"emucap", "buttons":input.buttons, "axes":input.axes
            }),
            None => json!({"ownership":"native", "buttons":[], "axes":{}}),
        };

        Ok(json!({
            "status":status,
            "state":"frozen",
            "requested_frames":requested_frames,
            "completed_frames":completed_frames,
            "start_frame":advance["start_frame"],
            "end_frame":advance["end_frame"],
            "clock":advance["clock"],
            "memory_type":observed["memory_type"],
            "address":observed["address"],
            "length":observed["length"],
            "hex":observed["hex"],
            "base_state":{
                "path":loaded["path"],
                "sha256":loaded["sha256"],
                "format":loaded["format"],
                "launch_id":loaded["launch_id"],
                "media_boundary":loaded["media_boundary"],
            },
            "input_override":input,
        }))
    }
}
