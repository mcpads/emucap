use super::*;

impl<G: GdbTransport> NdsBridge<G> {
    /// Disassemble `count` instructions from `address`/`start` on the routed CPU (default ARM9).
    /// `mode` ("arm"/"thumb"/"auto", default auto from the CPU's CPSR T-bit) picks the decoder.
    /// Returns `[{addr, bytes, text}]` where `bytes` is the little-endian in-memory opcode hex.
    pub(super) fn disassemble(&mut self, params: &Value) -> NdsResult<Value> {
        let cpu = cpu_from_params(params)?;
        let addr = absolute_address(params)?;
        let count = optional_num(params, "count")?.unwrap_or(8).clamp(1, 4096);
        let mode = match params.get("mode").and_then(Value::as_str) {
            None | Some("auto") => "auto",
            Some("arm") => "arm",
            Some("thumb") => "thumb",
            Some(other) => {
                return Err(NdsBridgeError::BadParams(format!(
                    "unsupported disassemble mode: {other}; valid: arm, thumb, auto"
                )))
            }
        };
        let b64 = self.cpu_mut(cpu)?.disasm_b64(addr, count, mode)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|err| {
                NdsBridgeError::Emulator(format!("disassemble: base64 decode failed: {err}"))
            })?;
        let text = String::from_utf8_lossy(&bytes);
        let instructions = parse_disasm_rows(&text, count as usize);
        if instructions.is_empty() {
            return Err(NdsBridgeError::Emulator(
                "disassemble: DeSmuME produced no instructions".into(),
            ));
        }
        Ok(json!({ "instructions": instructions, "cpu": cpu.as_str(), "mode": mode }))
    }

    /// Best-effort ARM call stack for the routed CPU (default ARM9). Frame 0 is the PC; frame 1
    /// is LR (the current function's return address, valid only before it is overwritten); deeper
    /// frames walk the APCS r11 frame-pointer chain (`[fp-4]`=saved lr, `[fp-12]`=saved fp) and
    /// end early once r11 stops looking like a frame pointer — which is exactly when the game does
    /// not keep one. Each frame PC is sanity-checked against plausible NDS code regions.
    pub(super) fn call_stack(&mut self, params: &Value) -> NdsResult<Value> {
        let cpu = cpu_from_params(params)?;
        let state = state_from_arm_regs_hex(&self.cpu_mut(cpu)?.read_regs_hex()?);
        let reg = |k: &str| state.get(k).and_then(Value::as_u64).unwrap_or(0);
        let pc = reg("cpu.pc");
        let lr = reg("cpu.lr");
        let sp = reg("cpu.sp");
        let mut fp = reg("cpu.r11");

        let mut frames = vec![json!({
            "pc": pc, "kind": "pc", "in_code_region": nds_in_code_region(pc)
        })];
        if lr != 0 {
            frames.push(json!({
                "pc": lr, "kind": "lr", "in_code_region": nds_in_code_region(lr)
            }));
        }
        let conn = self.cpu_mut(cpu)?;
        let mut depth = 0;
        while depth < 64 {
            // The frame base must be RAM-resident and at/above the stack top (stack grows down).
            if fp == 0 || !nds_in_ram(fp) || (sp != 0 && fp < sp) {
                break;
            }
            let (Some(saved_lr), Some(saved_fp)) = (
                conn.read_ptr_le(fp.wrapping_sub(4)),
                conn.read_ptr_le(fp.wrapping_sub(12)),
            ) else {
                break;
            };
            // A saved return address outside code space means r11 was not a frame pointer here.
            if !nds_in_code_region(saved_lr) {
                break;
            }
            frames.push(json!({ "pc": saved_lr, "kind": "fp-walk", "in_code_region": true }));
            // Callers sit at higher stack addresses; a non-increasing/out-of-RAM link ends the chain.
            if saved_fp <= fp || !nds_in_ram(saved_fp) {
                break;
            }
            fp = saved_fp;
            depth += 1;
        }
        Ok(json!({
            "frames": frames,
            "cpu": cpu.as_str(),
            "method": "lr+fp-walk (best-effort)",
            "note": "frame 0 = pc; frame 1 = lr (valid only until the current function overwrites it); deeper frames walk the APCS r11 frame-pointer chain and end early when the game does not keep r11 as a frame pointer. PCs are sanity-checked against NDS code regions.",
        }))
    }

    /// Select the GDB endpoint that initiates a scheduler resume. DeSmuME exposes one endpoint per
    /// CPU but has one execution scheduler, so exactly one `c` packet resumes both CPUs and both
    /// endpoint states. `both` remains an accepted compatibility spelling and uses ARM9.
    pub(super) fn resume_target(&self, params: &Value) -> NdsResult<CpuId> {
        match params.get("cpu").and_then(Value::as_str) {
            None | Some("arm9") | Some("both") | Some("all") => Ok(CpuId::Arm9),
            Some("arm7") if self.arm7.is_some() => Ok(CpuId::Arm7),
            Some("arm7") => Err(NdsBridgeError::Emulator(
                "ARM7 GDB connection is not attached".into(),
            )),
            Some(other) => Err(NdsBridgeError::BadParams(format!(
                "unsupported cpu: {other}; valid: arm9, arm7, both"
            ))),
        }
    }

    pub(super) fn poll_events(&mut self, params: &Value) -> NdsResult<Value> {
        // Validate the `breakpoint_id` filter BEFORE draining the stop sockets or `mem::take`ing any
        // queue (both destructive): a malformed filter must fail without consuming — and thereby
        // losing forever — the just-drained hits plus every previously-held event.
        let filter_id = optional_num(params, "breakpoint_id")?;
        // Drain BOTH cores' async-stop sockets before harvesting any events into a local. Draining
        // appends hits to each core's own queue, so if a later drain (ARM7) errors, the events
        // already drained from an earlier core (ARM9) stay safe in that core's queue and surface on
        // the next poll — harvesting ARM9 into a local between the two drains (the previous order)
        // would drop them when the ARM7 drain's `?` returns.
        self.drain_scheduler_stops()?;
        let mut fresh = std::mem::take(&mut self.arm9.events);
        if let Some(a7) = self.arm7.as_mut() {
            fresh.append(&mut std::mem::take(&mut a7.events));
        }
        for event in &mut fresh {
            self.enrich_event(event);
        }
        let mut all = std::mem::take(&mut self.events);
        all.append(&mut fresh);

        let mut out = Vec::new();
        for mut event in all {
            let matches_filter = match filter_id {
                Some(fid) => event.get("id").and_then(Value::as_u64) == Some(fid),
                None => true,
            };
            if matches_filter {
                if let Some(obj) = event.as_object_mut() {
                    obj.remove("_enriched");
                }
                out.push(event);
            } else {
                self.events.push(event);
            }
        }
        Ok(json!({ "events": out, "dropped": 0 }))
    }

    /// Attach the halted core's registers/PC to a stop event and, when the PC matches a known
    /// exec breakpoint on that core, reclassify it as a breakpoint hit.
    pub(super) fn enrich_event(&mut self, event: &mut Value) {
        if event.get("_enriched").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let cpu_name = event
            .get("cpu")
            .and_then(Value::as_str)
            .unwrap_or("arm9")
            .to_string();
        let cpu_id = CpuId::from_name(&cpu_name).unwrap_or(CpuId::Arm9);
        if event.get("regs").is_none() {
            match self.cpu_mut(cpu_id).and_then(|conn| conn.read_regs_hex()) {
                Ok(hex) => {
                    let state = state_from_arm_regs_hex(&hex);
                    if event.get("pc").is_none() {
                        if let Some(pc) = state.get("cpu.pc").cloned() {
                            set_event_field(event, "pc", pc);
                        }
                    }
                    set_event_field(event, "regs", state);
                }
                Err(err) => set_event_field(event, "regs_error", json!(err.to_string())),
            }
        }
        let pc = event.get("pc").and_then(Value::as_u64).or_else(|| {
            event
                .get("regs")
                .and_then(|r| r.get("cpu.pc"))
                .and_then(Value::as_u64)
        });
        if let Some(pc) = pc {
            let matched = self
                .bps
                .iter()
                .find(|(_, bp)| bp.cpu == cpu_id && bp.kind == "exec" && bp.addr == pc)
                .map(|(id, _)| *id);
            if let Some(id) = matched {
                set_event_field(event, "type", json!("breakpoint_hit"));
                set_event_field(event, "kind", json!("exec"));
                set_event_field(event, "address", json!(pc));
                set_event_field(event, "id", json!(id));
                set_event_field(event, "breakpoint_id", json!(id));
            }
        }
        set_event_field(event, "_enriched", json!(true));
    }

    pub(super) fn pause_target(&self, params: &Value) -> NdsResult<CpuId> {
        match params.get("cpu").and_then(Value::as_str) {
            None | Some("arm9") => Ok(CpuId::Arm9),
            Some("arm7") if self.arm7.is_some() => Ok(CpuId::Arm7),
            Some("arm7") => Err(NdsBridgeError::Emulator(
                "ARM7 GDB connection is not attached".into(),
            )),
            Some(other) => Err(NdsBridgeError::BadParams(format!(
                "unsupported cpu: {other}; valid: arm9, arm7"
            ))),
        }
    }

    pub(super) fn set_scheduler_frozen(&mut self, frozen: bool) {
        self.arm9.frozen = frozen;
        if let Some(arm7) = self.arm7.as_mut() {
            arm7.frozen = frozen;
        }
    }

    /// Drain asynchronous stops from both debugger endpoints before trusting either endpoint's
    /// execution flag. A stop on one CPU halts DeSmuME's shared scheduler, so one reportable stop
    /// makes both bridge-visible CPU states frozen. Events stay in their owning CPU queue.
    pub(super) fn drain_scheduler_stops(&mut self) -> NdsResult<()> {
        self.arm9.drain_stops()?;
        if let Some(arm7) = self.arm7.as_mut() {
            arm7.drain_stops()?;
        }
        if self.arm9.frozen || self.arm7.as_ref().is_some_and(|arm7| arm7.frozen) {
            self.set_scheduler_frozen(true);
        }
        Ok(())
    }

    pub(super) fn primary_frozen(&self) -> bool {
        self.arm9.frozen
    }

    pub(super) fn cpu_status(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "arm9".into(),
            json!({ "connected": true, "state": if self.arm9.frozen { "frozen" } else { "running" } }),
        );
        match &self.arm7 {
            Some(c) => obj.insert(
                "arm7".into(),
                json!({ "connected": true, "state": if c.frozen { "frozen" } else { "running" } }),
            ),
            None => obj.insert("arm7".into(), json!({ "connected": false })),
        };
        Value::Object(obj)
    }

    pub(super) fn connected_cpu_names(&self) -> Vec<&'static str> {
        let mut names = vec!["arm9"];
        if self.arm7.is_some() {
            names.push("arm7");
        }
        names
    }

    pub(super) fn memory_type_names(&self) -> Vec<&'static str> {
        MEMORY_REGIONS
            .iter()
            .filter(|r| r.cpu != CpuId::Arm7 || self.arm7.is_some())
            .map(|r| r.name)
            .collect()
    }

    pub(super) fn region_sizes_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        for region in MEMORY_REGIONS {
            if region.cpu == CpuId::Arm7 && self.arm7.is_none() {
                continue;
            }
            obj.insert(region.name.into(), json!(region.size));
        }
        Value::Object(obj)
    }

    pub(super) fn capability_notes(&self) -> Value {
        json!({
            "backend": "desmume-gdbstub",
            "rust_bridge": true,
            "implemented_methods": METHODS,
            "screenshot": true,
            "input": true,
            "timed_input_terminal_ack": true,
            "timed_input_max_frames": MAX_SYNC_TIMED_INPUT_FRAMES,
            "frame_step": false,
            "step_units": ["instructions"],
            "breakpoints": true,
            "watch_register": false,
            "trace": false,
            "state_restore": true,
            "disassemble": true,
            "call_stack": true,
            "dual_cpu": true,
            "shared_scheduler": true,
            "default_cpu": "arm9",
            "cpus": self.connected_cpu_names(),
        })
    }
}
