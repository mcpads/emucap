use super::input::{xbox_input_axes_json, xbox_input_buttons_json};
use super::*;
use std::time::Instant;

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn hello(&mut self) -> XemuResult<Value> {
        let status = self.extension_status()?;
        let machine = self.machine_status()?;
        if !self.hello_completed {
            if machine["running"].as_bool() != Some(false) {
                return Err(XemuBridgeError::BadState(
                    "managed xemu launch reached its first hello after guest execution began"
                        .into(),
                ));
            }
            self.read_cpu_state()?;
            if !self.controlled_start {
                self.gdb.send_no_reply("c")?;
                self.wait_running()?;
            }
            self.hello_completed = true;
        }
        let mut value = json!({
            "protocol_version": crate::live::protocol::PROTOCOL_VERSION,
            "system": "xbox",
            "adapter": "xemu-rust-qmp-gdb",
            "backend": "xemu-qmp-gdb",
            "debugger": true,
            "host_features": HOST_FEATURES,
            "methods": METHODS,
            "memory_types": ["main", "cpu"],
            "state_groups": ["cpu"],
            "cpu_targets": [{
                "id":"main", "aliases":["i386", "x86"], "default":true,
                "disassembly_modes":["auto", "x86"]
            }],
            "region_sizes": {"main": XBOX_RAM_SIZE, "cpu": 0x1_0000_0000u64},
            "breakpoint_kinds": [
                {"kind":"exec", "range_unit":"address", "range_mode":"exact", "memory_type_used":true, "snapshot":false},
                {"kind":"read", "range_unit":"byte", "range_mode":"inclusive", "memory_type_used":true, "snapshot":false},
                {"kind":"write", "range_unit":"byte", "range_mode":"inclusive", "memory_type_used":true, "snapshot":false},
                {"kind":"access", "range_unit":"byte", "range_mode":"inclusive", "memory_type_used":true, "snapshot":false},
            ],
            "contracts": crate::contracts::advertisement_value(CONTRACT_EXCEPTIONS),
            "input_buttons": xbox_input_buttons_json(),
            "input_axes": xbox_input_axes_json(),
            "capability_notes": self.capability_notes(),
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_MS,
            },
            "host_api": status["api"],
            "machine_inputs": self.machine_identity.value(),
            "host_build": self.state_environment.host_build,
            "launch_start": self.launch_start_value(),
        });
        let object = value.as_object_mut().expect("hello is an object");
        if let Some(name) = &self.env.name {
            object.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.env.session_token {
            object.insert("session_token".into(), json!(token));
        }
        if let Some(launch_id) = &self.env.launch_id {
            object.insert("launch_id".into(), json!(launch_id));
        }
        if let Some(content) = &self.current_disc {
            object.insert("content".into(), json!(content.display().to_string()));
        }
        object.insert(
            "build".into(),
            json!(self.env.build.as_deref().unwrap_or("unknown")),
        );
        Ok(value)
    }

    pub(super) fn status(&mut self) -> XemuResult<Value> {
        self.drain_gdb_stops(true)?;
        let machine = self.machine_status()?;
        let extension = self.extension_status()?;
        let running = machine["running"].as_bool().unwrap_or(false);
        let input_engaged = extension["input-engaged"].as_bool().unwrap_or(false);
        Ok(json!({
            "connected": true,
            "system": "xbox",
            "adapter": "xemu-rust-qmp-gdb",
            "backend": "xemu-qmp-gdb",
            "debugger": true,
            "host_features": HOST_FEATURES,
            "state": if running {"running"} else {"frozen"},
            "state_integrity": if self.state_integrity_error.is_some() {"unresolved"} else {"coherent"},
            "state_integrity_error": self.state_integrity_error,
            "state_snapshot_cleanup_pending": self.pending_state_snapshot_cleanup.len(),
            "qmp_status": machine["status"],
            "frame": extension["frame-boundary"],
            "frame_step": {
                "active": extension["frame-step-active"],
                "remaining": extension["frame-step-remaining"],
                "boundary": "nv2a_display_update_accepted_while_running",
            },
            "input_override": {
                "engaged": input_engaged,
                "buttons": if input_engaged { self.held_input.as_ref().map(|input| input.buttons.clone()) } else { None },
                "axes": if input_engaged { self.held_input.as_ref().map(|input| input.axes.clone()) } else { None },
                "ownership": if input_engaged {"emucap"} else {"native"},
            },
            "methods": METHODS,
            "memory_types": ["main", "cpu"],
            "region_sizes": {"main": XBOX_RAM_SIZE, "cpu": 0x1_0000_0000u64},
            "breakpoint_kinds": [
                {"kind":"exec", "range_unit":"address", "range_mode":"exact", "memory_type_used":true, "snapshot":false},
                {"kind":"read", "range_unit":"byte", "range_mode":"inclusive", "memory_type_used":true, "snapshot":false},
                {"kind":"write", "range_unit":"byte", "range_mode":"inclusive", "memory_type_used":true, "snapshot":false},
                {"kind":"access", "range_unit":"byte", "range_mode":"inclusive", "memory_type_used":true, "snapshot":false},
            ],
            "contracts": crate::contracts::advertisement_value(CONTRACT_EXCEPTIONS),
            "input_buttons": xbox_input_buttons_json(),
            "input_axes": xbox_input_axes_json(),
            "capability_notes": self.capability_notes(),
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_MS,
            },
            "machine_inputs": self.machine_identity.value(),
            "host_build": self.state_environment.host_build,
            "launch_start": self.launch_start_value(),
        }))
    }

    pub(super) fn launch_start_value(&self) -> Value {
        json!({
            "requested_frozen": self.controlled_start,
            "controlled": self.controlled_start,
            "boundary": if self.controlled_start {
                json!("pre_first_instruction")
            } else {
                Value::Null
            },
            "reset_linear_address": if self.controlled_start {
                json!(0xffff_fff0u64)
            } else {
                Value::Null
            },
        })
    }

    pub(super) fn capability_notes(&self) -> Value {
        json!({
            "implemented_methods": METHODS,
            "execution": {
                "units": ["frames", "instructions"],
                "frame_boundary": "NV2A display update accepted while VM running",
                "frame_step_terminal": "frozen",
                "breakpoint_preemption": true,
            },
            "memory": {
                "main": "64 MiB physical guest RAM exposed as offsets from zero",
                "cpu": "absolute 32-bit x86 virtual address view",
                "max_transfer": MAX_MEMORY_TRANSFER,
            },
            "cpu_state": "cpu.eip is the architectural EIP register, not a guaranteed linear address in segmented modes; controlled launch advertises the exact reset linear address separately",
            "state": "save_state/load_state use a generation-bound container that binds the internal VM/HDD snapshot, EEPROM bytes, current disc identity, host build, and controller topology; load is frozen-only and same-generation-only",
            "media": "change_media is frozen-only and accepts a regular raw .iso/.xiso file",
        })
    }

    pub(super) fn machine_status(&mut self) -> XemuResult<Value> {
        let value = self.qmp.execute("query-status", None)?;
        if value.get("running").and_then(Value::as_bool).is_none()
            || value.get("status").and_then(Value::as_str).is_none()
        {
            return Err(XemuBridgeError::Emulator(
                "xemu query-status returned an incomplete response".into(),
            ));
        }
        Ok(value)
    }

    pub(super) fn extension_status(&mut self) -> XemuResult<Value> {
        let value = self.qmp.execute("xemu-emucap-status", None)?;
        let api = value
            .get("api")
            .and_then(Value::as_u64)
            .ok_or_else(|| XemuBridgeError::Emulator("xemu extension status omitted api".into()))?;
        if api != REQUIRED_HOST_API {
            return Err(XemuBridgeError::Emulator(format!(
                "xemu emucap API mismatch: need {REQUIRED_HOST_API}, got {api}"
            )));
        }
        for key in [
            "frame-boundary",
            "frame-step-active",
            "frame-step-remaining",
            "input-engaged",
        ] {
            if value.get(key).is_none() {
                return Err(XemuBridgeError::Emulator(format!(
                    "xemu extension status omitted {key}"
                )));
            }
        }
        Ok(value)
    }

    pub(super) fn is_running(&mut self) -> XemuResult<bool> {
        Ok(self.machine_status()?["running"].as_bool().unwrap_or(false))
    }

    pub(super) fn wait_frozen(&mut self) -> XemuResult<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.is_running()? {
            if Instant::now() >= deadline {
                return Err(XemuBridgeError::Emulator(
                    "xemu did not enter the frozen state within 5 seconds".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    pub(super) fn wait_running(&mut self) -> XemuResult<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.is_running()? {
            if Instant::now() >= deadline {
                return Err(XemuBridgeError::Emulator(
                    "xemu did not enter the running state within 5 seconds".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    /// Stop a running VM for one debugger operation. The returned event count lets cleanup avoid
    /// resuming past a breakpoint that raced with the bridge-requested stop.
    pub(super) fn freeze_for_operation(&mut self) -> XemuResult<Option<usize>> {
        self.drain_gdb_stops(true)?;
        if !self.is_running()? {
            return Ok(None);
        }
        let events_before = self.events.len();
        self.qmp.execute("stop", None)?;
        self.wait_frozen()?;
        self.drain_gdb_stops(true)?;
        Ok(Some(events_before))
    }

    pub(super) fn restore_after_operation(&mut self, marker: Option<usize>) -> XemuResult<()> {
        if marker.is_some_and(|before| self.events.len() == before) {
            self.debug_stop_observed = false;
            self.gdb.send_no_reply("c")?;
        }
        Ok(())
    }

    pub(super) fn pause(&mut self) -> XemuResult<Value> {
        self.drain_gdb_stops(true)?;
        if self.is_running()? {
            self.qmp.execute("stop", None)?;
            self.wait_frozen()?;
            self.drain_gdb_stops(true)?;
        }
        let frame = self.extension_status()?["frame-boundary"].clone();
        Ok(json!({"state":"frozen", "frame":frame}))
    }

    pub(super) fn resume(&mut self) -> XemuResult<Value> {
        self.drain_gdb_stops(true)?;
        if !self.is_running()? {
            self.debug_stop_observed = false;
            self.gdb.send_no_reply("c")?;
        }
        Ok(json!({"state":"running"}))
    }

    pub(super) fn step(&mut self, params: &Value) -> XemuResult<Value> {
        match params.get("unit").and_then(Value::as_str) {
            None | Some("frames") => self.step_frames(params),
            Some("instructions") => self.step_instructions(params),
            Some(other) => Err(XemuBridgeError::BadParams(format!(
                "unsupported Xbox step unit: {other}; valid: frames, instructions"
            ))),
        }
    }

    pub(super) fn step_frames(&mut self, params: &Value) -> XemuResult<Value> {
        let count = step_count(params)?;
        self.pause()?;
        self.synchronize_gdb_stop()?;
        let before = self.extension_status()?;
        let start = before["frame-boundary"].as_u64().ok_or_else(|| {
            XemuBridgeError::Emulator("xemu returned a nonnumeric frame boundary".into())
        })?;
        self.qmp
            .execute("xemu-emucap-arm-frame-step", Some(json!({"count": count})))?;
        self.debug_stop_observed = false;
        let previous_timeout = self.gdb.get_timeout()?;
        let deadline = crate::live::temporal::OperationDeadline::after(
            crate::live::temporal::MAX_SYNC_OPERATION_TIME,
        );
        let outcome = (|| {
            let timeout = deadline.remaining_timeout().ok_or_else(|| {
                XemuBridgeError::Emulator("Xbox frame step deadline expired before continue".into())
            })?;
            self.gdb.set_timeout(timeout)?;
            self.gdb.send_no_reply("c")?;
            self.wait_frame_step(count, start, deadline)
        })();
        let restore = self
            .gdb
            .set_timeout(previous_timeout)
            .map_err(XemuBridgeError::from);
        let outcome = match (outcome, restore) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(primary), Ok(())) => Err(primary),
            (Ok(_), Err(cleanup)) => Err(XemuBridgeError::Emulator(format!(
                "Xbox frame step completed but failed to restore the GDB timeout: {cleanup}"
            ))),
            (Err(primary), Err(cleanup)) => Err(XemuBridgeError::Emulator(format!(
                "{primary}; additionally failed to restore the GDB timeout: {cleanup}"
            ))),
        };
        match outcome {
            Ok(value) => Ok(value),
            Err(error) => self.fail_frame_step(error),
        }
    }

    fn wait_frame_step(
        &mut self,
        count: u64,
        start: u64,
        deadline: crate::live::temporal::OperationDeadline,
    ) -> XemuResult<Value> {
        let stop = self.gdb.recv_reply()?;
        if !is_stop_packet(&stop) {
            return Err(XemuBridgeError::Emulator(format!(
                "GDB frame step returned an unexpected response: {stop}"
            )));
        }
        self.note_stop(stop, true)?;

        loop {
            let extension = self.extension_status()?;
            let machine = self.machine_status()?;
            let active = extension["frame-step-active"].as_bool().unwrap_or(false);
            let remaining = extension["frame-step-remaining"].as_u64().unwrap_or(count);
            let end = extension["frame-boundary"].as_u64().ok_or_else(|| {
                XemuBridgeError::Emulator("xemu returned a nonnumeric frame boundary".into())
            })?;
            let running = machine["running"].as_bool().unwrap_or(false);

            if !active && !running {
                let advanced = end.checked_sub(start).ok_or_else(|| {
                    XemuBridgeError::Emulator("xemu frame counter moved backwards".into())
                })?;
                if remaining != 0 || advanced != count {
                    return Err(XemuBridgeError::Emulator(format!(
                        "xemu frame step stopped inconsistently: requested {count}, advanced {advanced}, remaining {remaining}"
                    )));
                }
                self.drain_gdb_stops(false)?;
                return Ok(json!({
                    "status":"completed", "unit":"frames", "count":advanced,
                    "requested":count, "start_frame":start, "end_frame":end,
                    "clock":"nv2a_display_update_accepted_while_running", "state":"frozen"
                }));
            }

            if active && !running {
                self.qmp.execute("xemu-emucap-cancel-frame-step", None)?;
                let advanced = end.saturating_sub(start);
                return Ok(json!({
                    "status":"interrupted", "reason":"debugger_stop", "unit":"frames",
                    "count":advanced, "requested":count, "start_frame":start, "end_frame":end,
                    "clock":"nv2a_display_update_accepted_while_running", "state":"frozen"
                }));
            }

            if deadline.expired() {
                return Err(XemuBridgeError::Emulator(format!(
                    "Xbox frame step exceeded {} ms after {} of {count}",
                    crate::live::temporal::MAX_SYNC_OPERATION_MS,
                    end.saturating_sub(start)
                )));
            }
            std::thread::sleep(FRAME_POLL_INTERVAL);
        }
    }

    fn fail_frame_step<T>(&mut self, primary: XemuBridgeError) -> XemuResult<T> {
        let stop = self.qmp.execute("stop", None);
        let frozen = if stop.is_ok() {
            self.wait_frozen()
        } else {
            Err(XemuBridgeError::Emulator(
                "could not request terminal stop".into(),
            ))
        };
        let cancel = self.qmp.execute("xemu-emucap-cancel-frame-step", None);
        let mut cleanup = Vec::new();
        if let Err(error) = stop {
            cleanup.push(format!("stop failed: {error}"));
        }
        if let Err(error) = frozen {
            cleanup.push(format!("freeze confirmation failed: {error}"));
        }
        if let Err(error) = cancel {
            cleanup.push(format!("frame-step cancel failed: {error}"));
        }
        if cleanup.is_empty() {
            Err(primary)
        } else {
            Err(XemuBridgeError::Emulator(format!(
                "{primary}; terminal cleanup also failed: {}",
                cleanup.join("; ")
            )))
        }
    }

    pub(super) fn step_instructions(&mut self, params: &Value) -> XemuResult<Value> {
        let count = step_count(params)?;
        self.pause()?;
        let initial_state = self.synchronize_gdb_stop()?;
        let previous_timeout = self.gdb.get_timeout()?;
        let deadline = crate::live::temporal::OperationDeadline::after(
            crate::live::temporal::MAX_SYNC_OPERATION_TIME,
        );
        let outcome = (|| {
            let exec_armed = self
                .breakpoints
                .values()
                .any(|breakpoint| breakpoint.kind == "exec");
            let mut last_state = None;

            if exec_armed {
                let state = initial_state;
                if self.record_instruction_exec_hit("bridge:pre-step-pc-match", &state) {
                    return Ok(json!({
                        "status":"interrupted", "reason":"debugger_stop",
                        "unit":"instructions", "count":0, "requested":count,
                        "state":"frozen"
                    }));
                }
            }

            for completed in 0..count {
                let timeout = deadline.remaining_timeout().ok_or_else(|| {
                    XemuBridgeError::Emulator(format!(
                        "Xbox instruction step timed out after {completed} of {count}; VM remains frozen"
                    ))
                })?;
                self.gdb.set_timeout(timeout.min(Duration::from_secs(5)))?;
                self.debug_stop_observed = false;
                let mut response = self.gdb.send("s")?;
                let mut stale_interrupts = 0;
                while is_stop_packet(&response) && stop_signal(&response) == "02" {
                    stale_interrupts += 1;
                    if stale_interrupts > 4 {
                        return Err(XemuBridgeError::Emulator(
                            "too many stale GDB interrupt stops before instruction-step completion"
                                .into(),
                        ));
                    }
                    response = self.gdb.recv_reply()?;
                }
                if !is_stop_packet(&response) {
                    return Err(XemuBridgeError::Emulator(format!(
                        "GDB instruction step returned an unexpected response: {response}"
                    )));
                }

                if stop_watch_address(&response).is_some() {
                    self.note_stop(response, true)?;
                    return Ok(json!({
                        "status":"interrupted", "reason":"debugger_stop",
                        "unit":"instructions", "count":completed + 1,
                        "requested":count, "state":"frozen"
                    }));
                }
                if stop_signal(&response) != "05" {
                    self.note_stop(response, true)?;
                    return Ok(json!({
                        "status":"interrupted", "reason":"debugger_stop",
                        "unit":"instructions", "count":completed,
                        "requested":count, "state":"frozen"
                    }));
                }
                if exec_armed {
                    let state = self.read_cpu_state()?;
                    if self.record_instruction_exec_hit(&response, &state) {
                        return Ok(json!({
                            "status":"interrupted", "reason":"debugger_stop",
                            "unit":"instructions", "count":completed + 1,
                            "requested":count, "state":"frozen"
                        }));
                    }
                    last_state = Some(state);
                }
            }

            let state = match last_state {
                Some(state) => state,
                None => self.read_cpu_state()?,
            };
            Ok(json!({
                "status":"completed", "unit":"instructions", "count":count,
                "pc":state["cpu.eip"], "state":state
            }))
        })();
        let restore = self
            .gdb
            .set_timeout(previous_timeout)
            .map_err(XemuBridgeError::from);
        match (outcome, restore) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(primary), Ok(())) => Err(primary),
            (Ok(_), Err(cleanup)) => Err(XemuBridgeError::Emulator(format!(
                "Xbox instruction step completed but failed to restore the GDB timeout: {cleanup}"
            ))),
            (Err(primary), Err(cleanup)) => Err(XemuBridgeError::Emulator(format!(
                "{primary}; additionally failed to restore the GDB timeout: {cleanup}"
            ))),
        }
    }

    fn record_instruction_exec_hit(&mut self, raw: &str, state: &Value) -> bool {
        let Some(pc) = state.get("cpu.eip").and_then(Value::as_u64) else {
            return false;
        };
        let Some((id, breakpoint)) = self
            .breakpoints
            .iter()
            .find(|(_, breakpoint)| breakpoint.kind == "exec" && breakpoint.absolute == pc)
            .map(|(id, breakpoint)| (*id, breakpoint.clone()))
        else {
            return false;
        };
        self.debug_stop_observed = true;
        self.events.push(json!({
            "type":"breakpoint_hit", "signal":"05", "raw":raw,
            "source":"instruction_step_pc_match", "pc":pc, "regs":state,
            "id":id, "breakpoint_id":id, "kind":"exec",
            "address":breakpoint.start, "memory_type":breakpoint.memory_type,
        }));
        true
    }

    fn synchronize_gdb_stop(&mut self) -> XemuResult<Value> {
        self.drain_gdb_stops(false)?;
        // QEMU treats RSP `?` as a new-debugger handshake and removes every registered
        // breakpoint. A register round trip proves the frozen stub is serviceable without
        // mutating debugger state; gdb_command also demultiplexes a pending stop first.
        self.read_cpu_state()
    }

    pub(super) fn reset(&mut self) -> XemuResult<Value> {
        self.pause()?;
        self.release_input_override()?;
        self.debug_stop_observed = false;
        self.qmp.execute("system_reset", None)?;
        self.wait_frozen()?;
        self.drain_gdb_stops(false)?;
        let frame = self.extension_status()?["frame-boundary"].clone();
        Ok(json!({"status":"completed", "state":"frozen", "frame":frame}))
    }
}
