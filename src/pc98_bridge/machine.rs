use super::*;

impl<G: GdbTransport> Bridge<G> {
    pub(super) fn get_rom_info(&self) -> BridgeResult<Value> {
        let content = self.env.content.as_ref().ok_or_else(|| {
            BridgeError::BadParams("EMUCAP_CONTENT is not set for get_rom_info".into())
        })?;
        if !content.is_file() {
            return Err(BridgeError::BadParams(format!(
                "content image not found: {}",
                content.display()
            )));
        }
        Ok(json!({
            "system": "pc98",
            "adapter": "mame-pc98-rust-gdb",
            "name": content.file_name().and_then(|s| s.to_str()).unwrap_or(""),
            "path": absolute_display(content),
            "sha1": sha1_file(content)?,
            "size": content.metadata()?.len(),
            "media_type": content.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase(),
        }))
    }

    pub(super) fn pause(&mut self) -> BridgeResult<Value> {
        if !self.frozen {
            // Preserve a breakpoint stop that arrived just before this explicit pause. The raw
            // 0x03 interrupt itself returns one stop packet, which the transport consumes and ACKs.
            self.drain_buffered_stops()?;
            let _ = self.gdb.interrupt()?;
            self.frozen = true;
        }
        Ok(json!({ "state": "frozen" }))
    }

    pub(super) fn resume(&mut self) -> BridgeResult<Value> {
        if self.frozen {
            self.gdb.send_no_reply("c")?;
            self.frozen = false;
        }
        Ok(json!({ "state": "running" }))
    }

    pub(super) fn screenshot(&mut self) -> BridgeResult<Value> {
        let state = if self.frozen { "frozen" } else { "running" };
        let frame_before = self.current_frame();
        let path = std::env::temp_dir().join(format!(
            "emucap-pc98-{}-{}.png",
            std::process::id(),
            ulid::Ulid::generate().to_string().to_ascii_lowercase()
        ));
        let result = (|| {
            self.lua_cmd("snapshot", Some(path.to_string_lossy().as_ref()))?;
            let data = crate::path_safety::read_bounded_regular_file_no_follow(
                &path,
                crate::live::protocol::MAX_INLINE_SCREENSHOT_BYTES,
            )?;
            if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(BridgeError::Emulator(
                    "MAME snapshot did not produce a PNG".into(),
                ));
            }
            let frame_after = self.current_frame();
            let frame_stable = frame_before.is_some() && frame_before == frame_after;
            let mut hasher = Sha256::new();
            hasher.update(&data);
            Ok(json!({
                "png_base64": base64::engine::general_purpose::STANDARD.encode(&data),
                "sha256": hex::encode(hasher.finalize()),
                "byte_len": data.len(),
                "state": state,
                "frame_before": frame_before,
                "frame_after": frame_after,
                "frame_stable": frame_stable,
                "freshness": "unverified",
                "frame_binding": "unverified",
            }))
        })();
        let _ = fs::remove_file(&path);
        result
    }

    pub(super) fn set_input(&mut self, params: &Value) -> BridgeResult<Value> {
        require_input_port_zero(params)?;
        let buttons = normalize_buttons(params.get("buttons"))?;
        if let Err(err) = self.lua_cmd("setinput", Some(&buttons.join(","))) {
            return Err(self.explain_input_failure(err, &buttons));
        }
        Ok(json!({ "buttons": buttons }))
    }

    pub(super) fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        require_input_port_zero(params)?;
        let buttons = normalize_buttons(params.get("buttons"))?;
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        if frames > MAX_SYNC_TIMED_INPUT_FRAMES {
            return Err(BridgeError::BadParams(format!(
                "PC-98 synchronous press_buttons supports at most {MAX_SYNC_TIMED_INPUT_FRAMES} frames; split the pulse or use set_input with an explicit set_input([]) release"
            )));
        }
        let arg = format!("{frames}:{}", buttons.join(","));
        let stop = match self.deferred_lua_op("press", &arg, frames) {
            Ok(stop) => stop,
            Err(err) => return Err(self.explain_input_failure(err, &buttons)),
        };
        if let Some(raw) = stop {
            self.frozen = true;
            return Ok(json!({
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": raw,
                "buttons": buttons,
                "frames": frames,
                "frame": self.current_frame(),
            }));
        }
        self.frozen = false;
        Ok(json!({
            "status": "completed",
            "buttons": buttons,
            "frames": frames,
            "frame": self.current_frame(),
            "state": "running",
        }))
    }

    pub(super) fn move_pointer(&mut self, params: &Value) -> BridgeResult<Value> {
        require_input_port_zero(params)?;
        if !self.pointer_relative_available() {
            return Err(BridgeError::BadState(
                "relative pointer movement is unavailable in this MAME build".into(),
            ));
        }
        let dx = required_signed_num(params, "dx")?;
        let dy = required_signed_num(params, "dy")?;
        if dx == 0 && dy == 0 {
            return Err(BridgeError::BadParams(
                "move_pointer requires a non-zero relative delta".into(),
            ));
        }
        if !(-MAX_POINTER_DELTA..=MAX_POINTER_DELTA).contains(&dx)
            || !(-MAX_POINTER_DELTA..=MAX_POINTER_DELTA).contains(&dy)
        {
            return Err(BridgeError::BadParams(format!(
                "PC-98 relative pointer deltas must be between -{MAX_POINTER_DELTA} and {MAX_POINTER_DELTA}; split larger movement"
            )));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1);
        if frames == 0 {
            return Err(BridgeError::BadParams(
                "move_pointer frames must be at least 1".into(),
            ));
        }
        if frames > MAX_SYNC_TIMED_INPUT_FRAMES {
            return Err(BridgeError::BadParams(format!(
                "PC-98 synchronous move_pointer supports at most {MAX_SYNC_TIMED_INPUT_FRAMES} frames; split the movement"
            )));
        }
        let arg = format!("{frames}:{dx}:{dy}");
        let stop = self.deferred_lua_op("pointermove", &arg, frames)?;
        self.frozen = true;
        if let Some(raw) = stop {
            return Ok(json!({
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": raw,
                "dx": dx,
                "dy": dy,
                "frames": frames,
                "frame": self.current_frame(),
                "state": "frozen",
            }));
        }
        Ok(json!({
            "status": "completed",
            "dx": dx,
            "dy": dy,
            "frames": frames,
            "frame": self.current_frame(),
            "state": "frozen",
        }))
    }

    pub(super) fn pointer_relative_available(&mut self) -> bool {
        if let Some(available) = self.pointer_relative {
            return available;
        }
        let available = self
            .lua_cmd_reply("pointerstatus", None)
            .is_ok_and(|reply| reply == "RELATIVE");
        self.pointer_relative = Some(available);
        available
    }

    pub(super) fn advertised_methods(&mut self) -> Vec<&'static str> {
        METHODS
            .iter()
            .copied()
            .filter(|method| *method != "move_pointer" || self.pointer_relative_available())
            .collect()
    }

    pub(super) fn input_override_info(&mut self) -> Value {
        let Ok(raw) = self.lua_cmd_reply("inputstatus", None) else {
            return json!({ "observable": false });
        };
        let Ok(remaining) = raw.parse::<i64>() else {
            return json!({ "observable": false });
        };
        match remaining {
            0 => json!({ "observable": true, "engaged": false, "mode": "native" }),
            value if value < 0 => {
                json!({ "observable": true, "engaged": true, "mode": "persistent" })
            }
            value => json!({
                "observable": true,
                "engaged": true,
                "mode": "timed",
                "remaining_frames": value,
            }),
        }
    }

    pub(super) fn refresh_input_fields(&mut self) -> Vec<String> {
        // Query key fields actually registered in the machine's I/O ports. Normalize
        // display names from older plugins to callable canonical names and hide backend
        // fields outside the public surface. Older plugins return an empty error response;
        // fall back to an empty list and cache only a non-empty discovery result.
        if let Some(cached) = &self.input_fields {
            return cached.clone();
        }
        let fields = self
            .lua_cmd_reply("inputfields", None)
            .ok()
            .map(|s| normalize_discovered_input_fields(&s))
            .unwrap_or_default();
        if !fields.is_empty() {
            self.input_fields = Some(fields.clone());
        }
        fields
    }

    pub(super) fn explain_input_failure(
        &mut self,
        err: BridgeError,
        buttons: &[String],
    ) -> BridgeError {
        // E08 means the key is absent from this machine's I/O ports. Return the
        // unavailable name and callable alternatives instead of forwarding a bare E08.
        // Prefer E08:<key> from the plugin; otherwise infer it from discovered fields.
        let msg = err.to_string();
        let Some(idx) = msg.find("E08") else {
            return err;
        };
        let reported = msg[idx + 3..]
            .trim_start_matches(':')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        let available = self.refresh_input_fields();
        let unavailable: Vec<String> = if !reported.is_empty() {
            vec![reported]
        } else {
            buttons
                .iter()
                .filter(|b| !available.iter().any(|a| a == *b))
                .cloned()
                .collect()
        };
        let avail_str = if available.is_empty() {
            "(unknown; plugin does not report input fields)".to_string()
        } else {
            available.join(", ")
        };
        BridgeError::Emulator(format!(
            "PC-98 input(s) not registered on this machine: {}; available: {}",
            unavailable.join(", "),
            avail_str
        ))
    }
}
