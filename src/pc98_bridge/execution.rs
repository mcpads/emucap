use super::*;

impl<G: GdbTransport> Bridge<G> {
    pub(super) fn reset(&mut self) -> BridgeResult<Value> {
        self.lua_cmd("reset", None)?;
        Ok(json!({ "reset": "scheduled" }))
    }

    pub(super) fn break_on_reset(&mut self, params: &Value) -> BridgeResult<Value> {
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.lua_cmd("breakonreset", Some(if enabled { "1" } else { "0" }))?;
        Ok(json!({
            "enabled": enabled,
            "system": "pc98",
            "mode": "machine_reset_notifier",
        }))
    }

    pub(super) fn step(&mut self, params: &Value) -> BridgeResult<Value> {
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        let unit = params
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("frames");
        if unit == "instructions" {
            return self.step_instruction_count(frames);
        }
        if unit != "frames" {
            return Err(BridgeError::BadParams(format!(
                "unsupported PC-98 step unit: {unit}"
            )));
        }
        let stop = self.frames_op("framestep", frames)?;
        self.frozen = true;
        if let Some(raw) = stop {
            return Ok(json!({
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": raw,
                "frame": self.current_frame(),
            }));
        }
        Ok(json!({
            "status": "completed",
            "unit": "frames",
            "frames": frames,
            "frame": self.current_frame(),
        }))
    }

    pub(super) fn step_instructions(&mut self, params: &Value) -> BridgeResult<Value> {
        let count = match optional_num(params, "count")? {
            Some(count) => count,
            None => optional_num(params, "frames")?.unwrap_or(1),
        }
        .max(1);
        self.step_instruction_count(count)
    }

    pub(super) fn run_frames(&mut self, params: &Value) -> BridgeResult<Value> {
        let frames = match optional_num(params, "n")? {
            Some(frames) => frames,
            None => optional_num(params, "frames")?.unwrap_or(1),
        }
        .max(1);
        let stop = self.frames_op("runframes", frames)?;
        if let Some(raw) = stop {
            self.frozen = true;
            return Ok(json!({
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": raw,
                "frame": self.current_frame(),
            }));
        }
        self.frozen = false;
        Ok(json!({
            "status": "completed",
            "frames": frames,
            "frame": self.current_frame(),
            "state": "running",
        }))
    }

    pub(super) fn disassemble(&mut self, params: &Value) -> BridgeResult<Value> {
        let address = required_num(params, "address")?;
        let count = optional_num(params, "count")?.unwrap_or(8).clamp(1, 256) as usize;
        let byte_len = (count * 16).max(16);
        let path = std::env::temp_dir().join(format!(
            "emucap-pc98-dasm-{}-{}.txt",
            std::process::id(),
            ulid::Ulid::generate().to_string().to_ascii_lowercase()
        ));
        let result = {
            let spec = format!("{}|{address:x}|{byte_len:x}", path.to_string_lossy());
            match self.lua_cmd("dasm", Some(&spec)) {
                Ok(_) => match crate::path_safety::read_bounded_regular_file_no_follow(
                    &path,
                    1024 * 1024,
                ) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let instructions = parse_dasm_lines(text.lines(), count);
                        if instructions.is_empty() {
                            Err(BridgeError::Emulator(
                                "MAME disassemble produced no instructions".into(),
                            ))
                        } else {
                            Ok(json!({ "instructions": instructions }))
                        }
                    }
                    Err(err) => Err(BridgeError::Io(err)),
                },
                Err(err) => Err(err),
            }
        };
        let _ = fs::remove_file(&path);
        result
    }
}
