use super::*;

pub(super) fn launch_start_value(controlled_start: bool) -> Value {
    json!({
        "requested_frozen": controlled_start,
        "controlled": controlled_start,
        "boundary": if controlled_start {
            json!("pre_first_instruction")
        } else {
            Value::Null
        }
    })
}

impl Np2kaiHost {
    fn launch_start(&self) -> Value {
        launch_start_value(self.controlled_start)
    }

    pub(super) fn hello(&self) -> Np2kaiResult<Value> {
        let mut value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "pc98",
            "adapter": "np2kai-libretro",
            "backend": "np2kai-libretro",
            "debugger": true,
            "host_features": HOST_FEATURES,
            "methods": METHODS,
            "memory_types": debug::memory_type_names(),
            "state_groups": ["cpu"],
            "cpu_targets": [{
                "id":"main", "aliases":["i386", "x86"], "default":true,
                "disassembly_modes":["auto", "x86"]
            }],
            "region_sizes": debug::region_sizes_value(),
            "media_devices": [{
                "id": "hdd0", "kind": "hard_disk", "reset_on_load": false,
                "must_be_loaded": true, "supports_runtime_change": true, "supports_eject": false
            }],
            "breakpoint_kinds": [
                {"kind":"exec", "range_unit":"address", "range_mode":"inclusive", "memory_type_used":true, "snapshot":"pause_on_hit"},
                {"kind":"read", "range_unit":"address", "range_mode":"inclusive", "memory_type_used":true, "snapshot":"pause_on_hit", "value":"unavailable"},
                {"kind":"write", "range_unit":"address", "range_mode":"inclusive", "memory_type_used":true, "snapshot":"pause_on_hit", "value":"authoritative", "value_filter":true},
                {"kind":"access", "range_unit":"address", "range_mode":"inclusive", "memory_type_used":true, "snapshot":"pause_on_hit", "value":"unavailable"}
            ],
            "input_buttons": INPUT_BUTTONS,
            "execution_limits": {
                "max_sync_advance_count": MAX_SYNC_FRAMES,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
                "frame": {"max_count": MAX_SYNC_FRAMES}
            },
            "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS),
            "content": self.content_path.display().to_string(),
            "build": self.frontend_build,
            "host_build": {
                "upstream_commit": self.upstream_commit,
                "patchset_sha256": self.patchset_sha256,
                "build_profile": self.build_profile,
                "core_sha256": self.core_sha256,
                "host_api": 2
            },
            "capability_notes": {
                "backend_role": "pc98_hdi_compatibility_and_debugging",
                "implemented_methods": METHODS,
                "step_units": ["frames", "instructions"],
                "start_frozen": true,
                "deep_debugging": true,
                "media_change": true,
                "breakpoints": true,
                "watch_register": true,
                "trace": true,
                "relative_pointer": true,
                "pointer_buttons": ["mouse_left", "mouse_right"],
                "breakpoint_value_filters": ["write"],
                "audio_output": false,
                "first_frame_boundary": "no guest frame or instruction executes before an explicit execution request"
            },
            "runtime_home": self.runtime_home.display().to_string(),
            "launch_start": self.launch_start()
        });
        let object = value.as_object_mut().expect("NP2kai hello object");
        if let Some(name) = &self.name {
            object.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.session_token {
            object.insert("session_token".into(), json!(token));
        }
        if let Some(launch_id) = &self.launch_id {
            object.insert("launch_id".into(), json!(launch_id));
        }
        Ok(value)
    }

    pub(super) fn status(&self) -> Np2kaiResult<Value> {
        Ok(json!({
            "connected": true,
            "system": "pc98",
            "adapter": "np2kai-libretro",
            "backend": "np2kai-libretro",
            "debugger": true,
            "host_features": HOST_FEATURES,
            "methods": METHODS,
            "memory_types": debug::memory_type_names(),
            "region_sizes": debug::region_sizes_value(),
            "media_devices": [{
                "id": "hdd0", "kind": "hard_disk", "reset_on_load": false,
                "must_be_loaded": true, "supports_runtime_change": true, "supports_eject": false
            }],
            "mounted_media": [{
                "device": "hdd0", "mounted": true, "readonly": false,
                "path": self.content_path.display().to_string(), "sha256": self.content_sha256
            }],
            "input_buttons": {
                "buttons": INPUT_BUTTONS,
                "available": INPUT_BUTTONS
            },
            "input_override": {
                "engaged": !self.held_buttons.is_empty(),
                "owner": if self.held_buttons.is_empty() {"frontend"} else {"mcp"},
                "buttons": self.held_buttons.iter().collect::<Vec<_>>()
            },
            "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS),
            "state": if self.frozen {"frozen"} else {"running"},
            "frame": self.frame,
            "initialized": self.initialized,
            "video_fresh": self.video_fresh,
            "launch_start": self.launch_start(),
            "execution_limits": {
                "max_sync_advance_count": MAX_SYNC_FRAMES,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
                "frame": {"max_count": MAX_SYNC_FRAMES}
            }
            ,"capability_notes": {
                "backend_role": "pc98_hdi_compatibility_and_debugging",
                "implemented_methods": METHODS,
                "step_units": ["frames", "instructions"],
                "start_frozen": true,
                "deep_debugging": true,
                "breakpoints": true,
                "watch_register": true,
                "trace": true,
                "media_change": true,
                "relative_pointer": true,
                "pointer_buttons": ["mouse_left", "mouse_right"],
                "breakpoint_value_filters": ["write"],
                "audio_output": false
            }
        }))
    }

    pub(super) fn get_rom_info(&self) -> Np2kaiResult<Value> {
        Ok(json!({
            "system": "pc98",
            "content": self.content_path.display().to_string(),
            "rom_size": self.content_size,
            "rom_sha1": self.content_sha1,
            "content_sha256": self.content_sha256,
            "content_identity_scope": "complete_hdi_file",
            "backend": "np2kai-libretro",
            "core_sha256": self.core_sha256,
            "firmware_sha256": self.firmware_sha256
        }))
    }

    pub(super) fn pause(&mut self) -> Np2kaiResult<Value> {
        self.frozen = true;
        Ok(json!({"status":"completed", "state":"frozen", "frame":self.frame}))
    }

    pub(super) fn resume(&mut self) -> Np2kaiResult<Value> {
        self.frozen = false;
        Ok(json!({"status":"completed", "state":"running", "frame":self.frame}))
    }

    pub(super) fn step(&mut self, params: &Value) -> Np2kaiResult<Value> {
        require_frame_unit(params)?;
        let count = frame_count(params)?;
        let before = self.frame;
        self.frozen = true;
        let completed = self.run_exact_frames(count)?;
        Ok(json!({
            "status":if completed == count {"completed"} else {"interrupted"},
            "reason":if completed == count {Value::Null} else {json!("breakpoint")},
            "unit":"frames", "count":count, "completed":completed,
            "frame_before":before, "frame":self.frame, "state":"frozen"
        }))
    }

    pub(super) fn run_frames(&mut self, params: &Value) -> Np2kaiResult<Value> {
        require_frame_unit(params)?;
        let count = frame_count(params)?;
        let was_frozen = self.frozen;
        let before = self.frame;
        let completed = self.run_exact_frames(count)?;
        self.frozen = was_frozen;
        if completed != count {
            self.frozen = true;
        }
        Ok(json!({
            "status":if completed == count {"completed"} else {"interrupted"},
            "reason":if completed == count {Value::Null} else {json!("breakpoint")},
            "unit":"frames", "count":count, "completed":completed,
            "frame_before":before, "frame":self.frame,
            "state":if self.frozen {"frozen"} else {"running"}
        }))
    }

    pub(super) fn set_input(&mut self, params: &Value) -> Np2kaiResult<Value> {
        require_port_zero(params)?;
        let buttons = normalize_buttons(params.get("buttons"))?;
        set_controls(&buttons);
        self.held_buttons = buttons;
        Ok(json!({
            "status":"completed",
            "buttons":self.held_buttons.iter().collect::<Vec<_>>(),
            "override_engaged":!self.held_buttons.is_empty(),
            "ownership":if self.held_buttons.is_empty() {"frontend"} else {"mcp"}
        }))
    }

    pub(super) fn press_buttons(&mut self, params: &Value) -> Np2kaiResult<Value> {
        require_port_zero(params)?;
        let pulse = normalize_buttons(params.get("buttons"))?;
        if pulse.is_empty() {
            return Err(Np2kaiError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1);
        if !(1..=MAX_INPUT_PULSE_FRAMES).contains(&frames) {
            return Err(Np2kaiError::BadParams(format!(
                "press_buttons frames must be in 1..={MAX_INPUT_PULSE_FRAMES}, got {frames}"
            )));
        }
        let was_frozen = self.frozen;
        let active = self
            .held_buttons
            .union(&pulse)
            .cloned()
            .collect::<BTreeSet<_>>();
        set_controls(&active);
        let result = self.run_exact_frames(frames);
        set_controls(&self.held_buttons);
        let completed = result?;
        self.frozen = was_frozen;
        if completed != frames {
            self.frozen = true;
        }
        Ok(json!({
            "status":if completed == frames {"completed"} else {"interrupted"},
            "buttons":pulse.iter().collect::<Vec<_>>(), "frames":frames, "completed":completed,
            "persistent_buttons":self.held_buttons.iter().collect::<Vec<_>>(),
            "transient_override_engaged":false,
            "state":if self.frozen {"frozen"} else {"running"}, "frame":self.frame
        }))
    }

    pub(super) fn move_pointer(&mut self, params: &Value) -> Np2kaiResult<Value> {
        require_port_zero(params)?;
        let dx = required_signed_num(params, "dx")?;
        let dy = required_signed_num(params, "dy")?;
        if dx == 0 && dy == 0 {
            return Err(Np2kaiError::BadParams(
                "move_pointer requires a non-zero relative delta".into(),
            ));
        }
        if !(-127..=127).contains(&dx) || !(-127..=127).contains(&dy) {
            return Err(Np2kaiError::BadParams(
                "NP2kai relative pointer deltas must be in -127..=127".into(),
            ));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1);
        if !(1..=MAX_INPUT_PULSE_FRAMES).contains(&frames) {
            return Err(Np2kaiError::BadParams(format!(
                "move_pointer frames must be in 1..={MAX_INPUT_PULSE_FRAMES}, got {frames}"
            )));
        }
        self.frozen = true;
        let mut completed = 0;
        for _ in 0..frames {
            set_pointer_delta(dx as i16, dy as i16);
            let result = self.run_one_frame();
            set_pointer_delta(0, 0);
            if !result? {
                break;
            }
            completed += 1;
        }
        Ok(json!({
            "status":if completed == frames {"completed"} else {"interrupted"},
            "reason":if completed == frames {Value::Null} else {json!("breakpoint")},
            "dx":dx, "dy":dy, "frames":frames, "completed":completed,
            "frame":self.frame, "state":"frozen", "coordinate_mode":"relative"
        }))
    }

    pub(super) fn screenshot(&self) -> Np2kaiResult<Value> {
        self.require_frozen("screenshot")?;
        if !self.video_fresh {
            return Err(Np2kaiError::BadState("screenshot is unavailable until one frozen frame is completed after launch or load_state".into()));
        }
        let (png, width, height) = captured_png()?;
        Ok(json!({
            "png_base64":base64::engine::general_purpose::STANDARD.encode(&png),
            "sha256":hex::encode(Sha256::digest(&png)), "byte_len":png.len(),
            "width":width, "height":height, "frame":self.frame, "frame_stable":true,
            "state":"frozen", "freshness":"current",
            "capture_boundary":"last_completed_retro_run_video_callback"
        }))
    }

    pub(super) fn save_state(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.require_frozen("save_state")?;
        self.ensure_initialized()?;
        let path = absolute_path_param(params, "path")?;
        let size = unsafe { (self.api.serialize_size)() };
        if size == 0 {
            return Err(Np2kaiError::Core(
                "NP2kai reported a zero serialization size".into(),
            ));
        }
        let mut data = vec![0_u8; size];
        if !unsafe { (self.api.serialize)(data.as_mut_ptr().cast(), data.len()) } {
            return Err(Np2kaiError::Core("retro_serialize failed".into()));
        }
        atomic_write(&path, &data)?;
        let state_sha256 = hex::encode(Sha256::digest(&data));
        let identity = self.state_identity(state_sha256.clone());
        atomic_write(
            &state_sidecar(&path),
            &serde_json::to_vec_pretty(&identity)?,
        )?;
        Ok(json!({
            "status":"completed", "path":path.display().to_string(),
            "sidecar":state_sidecar(&path).display().to_string(), "format":"np2kai-libretro-state",
            "bytes":data.len(), "sha256":state_sha256, "state":"frozen", "frame":self.frame
        }))
    }

    pub(super) fn load_state(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.require_frozen("load_state")?;
        self.ensure_initialized()?;
        let path = absolute_path_param(params, "path")?;
        let data = fs::read(&path)?;
        if data.is_empty() {
            return Err(Np2kaiError::BadParams("state file is empty".into()));
        }
        let identity: StateIdentity = serde_json::from_slice(&fs::read(state_sidecar(&path))?)?;
        let expected = self.state_identity(hex::encode(Sha256::digest(&data)));
        if identity != expected {
            return Err(Np2kaiError::BadParams("save state identity does not match this exact PC-98 media, firmware, core, patch, profile, frontend build, OS, and architecture".into()));
        }
        if !unsafe { (self.api.unserialize)(data.as_ptr().cast(), data.len()) } {
            return Err(Np2kaiError::Core("retro_unserialize failed".into()));
        }
        self.video_fresh = false;
        self.frozen = true;
        Ok(json!({
            "status":"completed", "path":path.display().to_string(), "format":"np2kai-libretro-state",
            "state":"frozen", "frame":self.frame, "frame_counter_continuous":true,
            "screenshot_freshness":"unverified_until_frozen_frame_step"
        }))
    }

    pub(super) fn reset(&mut self) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        unsafe {
            (self.api.reset)();
            // A reset breakpoint's event remains queued, but the synchronous
            // reset operation has already established the frozen boundary.
            (self.api.debug_clear_stop)();
        }
        self.frozen = true;
        self.video_fresh = false;
        Ok(json!({
            "status":"completed", "state":"frozen", "frame":self.frame,
            "guest_frames_advanced":0, "initialization_pending":false
        }))
    }

    pub(super) fn run_exact_frames(&mut self, count: u64) -> Np2kaiResult<u64> {
        let mut completed = 0;
        for _ in 0..count {
            if !self.run_one_frame()? {
                break;
            }
            completed += 1;
        }
        Ok(completed)
    }

    pub(super) fn run_one_frame(&mut self) -> Np2kaiResult<bool> {
        self.ensure_initialized()?;
        let before = callback_count()?;
        unsafe { (self.api.run)() };
        let after = callback_count()?;
        if unsafe { (self.api.debug_stop_requested)() } != 0 {
            self.frozen = true;
            unsafe { (self.api.debug_clear_stop)() };
            self.drain_native_trace()?;
            if after > before + 1 {
                return Err(Np2kaiError::Core(format!(
                    "breakpoint-interrupted retro_run emitted too many video callbacks: before={before}, after={after}"
                )));
            }
            return Ok(false);
        }
        if after != before + 1 {
            return Err(Np2kaiError::Core(format!("retro_run did not produce exactly one video callback: before={before}, after={after}")));
        }
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or_else(|| Np2kaiError::Core("frontend frame counter overflow".into()))?;
        self.video_fresh = true;
        self.drain_native_trace()?;
        Ok(true)
    }

    pub(super) fn ensure_initialized(&mut self) -> Np2kaiResult<()> {
        if !self.initialized {
            let callbacks_before = callback_count()?;
            unsafe { (self.api.run)() };
            let callbacks_after = callback_count()?;
            if callbacks_after != callbacks_before {
                return Err(Np2kaiError::Core(
                    "NP2kai initialization unexpectedly emitted a video frame".into(),
                ));
            }
            self.initialized = true;
        }
        Ok(())
    }

    pub(super) fn require_frozen(&self, operation: &str) -> Np2kaiResult<()> {
        if self.frozen {
            Ok(())
        } else {
            Err(Np2kaiError::BadState(format!(
                "{operation} requires a frozen PC-98 machine; call pause first"
            )))
        }
    }

    fn state_identity(&self, state_sha256: String) -> StateIdentity {
        StateIdentity {
            format: "np2kai-libretro-state".into(),
            system: "pc98".into(),
            target_os: std::env::consts::OS.into(),
            target_arch: std::env::consts::ARCH.into(),
            media_sha256: self.content_sha256.clone(),
            firmware_sha256: self.firmware_sha256.clone(),
            core_sha256: self.core_sha256.clone(),
            upstream_commit: self.upstream_commit.clone(),
            patchset_sha256: self.patchset_sha256.clone(),
            build_profile: self.build_profile.clone(),
            frontend_build: self.frontend_build.clone(),
            state_sha256,
        }
    }
}
