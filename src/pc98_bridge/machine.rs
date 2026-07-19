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
            "emucap_pc98_{}_{}.png",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let result = (|| {
            self.lua_cmd("snapshot", Some(path.to_string_lossy().as_ref()))?;
            let data = fs::read(&path)?;
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
                "sha256": format!("{:x}", hasher.finalize()),
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
        // 머신 ioport에 실제 등록된 키 필드를 조회한다. 버튼 이름은 균일 매핑을 유지하고,
        // 가용성만 머신별로 다르므로 status/에러가 이 목록을 그대로 노출한다. 구 plugin은
        // 이 쿼리를 몰라 빈 응답→Err이니 빈 목록으로 폴백한다(비-non-empty만 캐시).
        if let Some(cached) = &self.input_fields {
            return cached.clone();
        }
        let fields = self
            .lua_cmd_reply("inputfields", None)
            .ok()
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
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
        // E08 = 이 머신 ioport에 등록되지 않은 키. 어느 버튼이 없고 무엇이 가능한지 이름을 붙여
        // 돌려준다(맨몸 E08 패스스루 금지). plugin이 E08:<key>로 미해결 키를 보고하면 그걸 쓰고,
        // 아니면 가용 목록과 대조해 유추한다.
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
            "PC-98 key(s) not registered on this machine: {}; available: {}",
            unavailable.join(", "),
            avail_str
        ))
    }
}
