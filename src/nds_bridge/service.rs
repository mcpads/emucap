use super::*;

impl<G: GdbTransport> NdsBridge<G> {
    pub(super) fn cpu_mut(&mut self, id: CpuId) -> NdsResult<&mut CpuConn<G>> {
        match id {
            CpuId::Arm9 => Ok(&mut self.arm9),
            CpuId::Arm7 => self.arm7.as_mut().ok_or_else(|| {
                NdsBridgeError::Emulator(
                    "ARM7 GDB connection is not attached (launch with an ARM7 endpoint to use arm7 memory/cpu)".into(),
                )
            }),
        }
    }

    pub(super) fn hello(&self) -> NdsResult<Value> {
        let mut result = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "nds",
            "adapter": "desmume-nds-rust-gdb",
            "backend": "desmume-gdbstub",
            "debugger": true,
            "methods": METHODS,
            "memory_types": self.memory_type_names(),
            "contracts": crate::contracts::advertisement_value(&[
                "nds.execution.frame-step-absent",
                "nds.call-stack.best-effort",
                "nds.input-hold.port-zero-only",
                "nds.input-pulse.constraints",
                "nds.input-touch.constraints",
            ]),
            "region_sizes": self.region_sizes_json(),
            "capability_notes": self.capability_notes(),
            "input_buttons": nds_input_buttons_json(),
            "cpus": self.connected_cpu_names(),
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
            },
        });
        let obj = result.as_object_mut().expect("hello is an object");
        if let Some(name) = &self.env.name {
            obj.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.env.session_token {
            obj.insert("session_token".into(), json!(token));
        }
        if let Some(launch_id) = &self.env.launch_id {
            obj.insert("launch_id".into(), json!(launch_id));
        }
        if let Some(content) = &self.env.content {
            obj.insert("content".into(), json!(content.display().to_string()));
        }
        obj.insert(
            "build".into(),
            json!(self.env.build.as_deref().unwrap_or("unknown")),
        );
        Ok(result)
    }

    pub(super) fn status(&mut self) -> NdsResult<Value> {
        self.drain_scheduler_stops()?;
        // The fork owns persistent/timed overrides, so query it instead of trusting bridge-local
        // bookkeeping that would be lost on a bridge reconnect. Older binaries remain observable=false.
        let input_override =
            override_status_json(self.arm9.override_remaining("qEmucap,inputstatus").ok());
        let touch_override =
            override_status_json(self.arm9.override_remaining("qEmucap,touchstatus").ok());
        Ok(json!({
            "connected": true,
            "system": "nds",
            "adapter": "desmume-nds-rust-gdb",
            "backend": "desmume-gdbstub",
            "debugger": true,
            "state": if self.primary_frozen() { "frozen" } else { "running" },
            "memory_types": self.memory_type_names(),
            "contracts": crate::contracts::advertisement_value(&[
                "nds.execution.frame-step-absent",
                "nds.call-stack.best-effort",
                "nds.input-hold.port-zero-only",
                "nds.input-pulse.constraints",
                "nds.input-touch.constraints",
            ]),
            "cpus": self.cpu_status(),
            "capability_notes": self.capability_notes(),
            "input_buttons": nds_input_buttons_json(),
            "input_override": input_override,
            "touch_override": touch_override,
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
            },
        }))
    }

    pub(super) fn get_rom_info(&self) -> NdsResult<Value> {
        let content = self.env.content.as_ref().ok_or_else(|| {
            NdsBridgeError::BadParams("EMUCAP_CONTENT is not set for get_rom_info".into())
        })?;
        if !content.is_file() {
            return Err(NdsBridgeError::BadParams(format!(
                "content image not found: {}",
                content.display()
            )));
        }
        Ok(json!({
            "system": "nds",
            "adapter": "desmume-nds-rust-gdb",
            "name": content.file_name().and_then(|s| s.to_str()).unwrap_or(""),
            "path": absolute_display(content),
            "sha1": sha1_file(content)?,
            "size": content.metadata()?.len(),
            "media_type": content.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase(),
        }))
    }

    pub(super) fn read_memory(&mut self, params: &Value) -> NdsResult<Value> {
        let length = required_num(params, "length")? as usize;
        if length > MAX_READ_LEN {
            return Err(NdsBridgeError::BadParams(format!(
                "read length {length:#x} exceeds the {MAX_READ_LEN:#x} cap — read a large region in chunks (advance the start address)"
            )));
        }
        let (cpu, addr, _region) = route(params, length as u64)?;
        let hex = self.cpu_mut(cpu)?.read_abs_hex(addr, length)?;
        Ok(json!({ "hex": hex, "cpu": cpu.as_str() }))
    }

    pub(super) fn write_memory(&mut self, params: &Value) -> NdsResult<Value> {
        let hexstr = required_str(params, "hex")?;
        if hexstr.len() % 2 != 0 {
            return Err(NdsBridgeError::BadParams(
                "hex must have even length".into(),
            ));
        }
        hex::decode(hexstr).map_err(|_| NdsBridgeError::BadParams("hex decode failed".into()))?;
        let size = hexstr.len() / 2;
        if size > MAX_WRITE_LEN {
            return Err(NdsBridgeError::BadParams(format!(
                "write length {size:#x} exceeds the {MAX_WRITE_LEN:#x} cap — write a large region in chunks (advance the start address)"
            )));
        }
        let (cpu, addr, _region) = route(params, size as u64)?;
        // DeSmuME's CPU stubs have separate sockets but share one emulation scheduler. Interrupting
        // the routed core stops that scheduler for the whole chunked write. Do not also interrupt
        // the sibling endpoint: once the first core stops global execution, the second endpoint
        // cannot complete another interrupt until emulation resumes and would time out.
        self.cpu_mut(cpu)?.write_abs_hex(addr, hexstr)?;
        Ok(json!({ "written": size, "cpu": cpu.as_str() }))
    }

    /// Scan a memory region for a hex byte pattern, returning the matching region-relative offsets.
    /// Mirrors the pc98/Mednafen/Mesen `find_pattern`: `memory_type` (default `main`), `hex` pattern,
    /// optional `start`/`length` window, `max_matches` (1..4096, default 256) and `align` (default 1).
    /// The scan window is capped at `MAX_FIND_LEN` and reported via `truncated_scan`. The bulk read
    /// rides the same GDB `m` path as `read_memory`, so a short/failed stub read errors cleanly.
    pub(super) fn find_pattern(&mut self, params: &Value) -> NdsResult<Value> {
        let memory_type = params
            .get("memory_type")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        let region = *memory_region(&memory_type).ok_or_else(|| {
            NdsBridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let pattern = hex::decode(required_str(params, "hex")?)
            .map_err(|_| NdsBridgeError::BadParams("hex decode failed".into()))?;
        if pattern.is_empty() {
            return Err(NdsBridgeError::BadParams(
                "hex must contain at least one byte".into(),
            ));
        }

        let start = optional_num(params, "start")?.unwrap_or(0);
        let mut length =
            optional_num(params, "length")?.unwrap_or_else(|| region.size.saturating_sub(start));
        if start >= region.size {
            length = 0;
        } else {
            length = length.min(region.size - start);
        }
        let length = length as usize;
        let truncated_scan = length > MAX_FIND_LEN;
        let scan_len = length.min(MAX_FIND_LEN);
        let max_matches = optional_num(params, "max_matches")?
            .unwrap_or(256)
            .clamp(1, 4096) as usize;
        let align = optional_num(params, "align")?.unwrap_or(1).max(1) as usize;

        let buf = if scan_len == 0 {
            Vec::new()
        } else {
            self.read_region_bytes(&memory_type, start, scan_len)?
        };
        let mut matches = Vec::new();
        let mut truncated_matches = false;
        let mut pos = 0usize;
        while pos <= buf.len().saturating_sub(pattern.len()) {
            let Some(idx) = find_subslice(&buf[pos..], &pattern) else {
                break;
            };
            let rel = pos + idx;
            if rel.is_multiple_of(align) {
                if matches.len() >= max_matches {
                    truncated_matches = true;
                    break;
                }
                matches.push(start + rel as u64);
            }
            pos = rel + 1;
        }

        Ok(json!({
            "matches": matches,
            "count": matches.len(),
            "truncated": truncated_scan || truncated_matches,
            "truncated_scan": truncated_scan,
            "truncated_matches": truncated_matches,
            "scanned": scan_len,
            "start": start,
            "memory_type": memory_type,
            "cpu": region.cpu.as_str(),
        }))
    }

    /// Snapshot every bounded NDS RAM region to `<path>/<name>.bin` plus a `regions.json` manifest
    /// (`RegionMeta` keys: name/memory_type/base_address/size). The MCP host writes `state.json`
    /// itself, so the bridge only emits the region bytes + manifest. Each region is read whole under a
    /// single freeze (no torn snapshot) with per-chunk length validation, written to a temp file whose
    /// size is verified, then atomically renamed — a short/failed read never leaves a partial `.bin`.
    pub(super) fn dump_memory(&mut self, params: &Value) -> NdsResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        std::fs::create_dir_all(&path)?;
        let mut metas = Vec::new();
        for region in self.dump_regions() {
            let name = region.name;
            let size = region.size as usize;
            let bytes = self.read_region_bytes(name, 0, size)?;
            if bytes.len() as u64 != region.size {
                return Err(NdsBridgeError::Emulator(format!(
                    "dump {name}: read {} of {} bytes",
                    bytes.len(),
                    region.size
                )));
            }
            let final_path = path.join(format!("{name}.bin"));
            let tmp_path = path.join(format!(".{name}.bin.partial"));
            std::fs::write(&tmp_path, &bytes)?;
            let written = std::fs::metadata(&tmp_path)?.len();
            if written != region.size {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(NdsBridgeError::Emulator(format!(
                    "dump {name}: wrote {written} of {} bytes to disk",
                    region.size
                )));
            }
            std::fs::rename(&tmp_path, &final_path)?;
            metas.push(json!({
                "name": name,
                "memory_type": name,
                "base_address": region.base,
                "size": region.size,
            }));
        }
        // regions.json is written last, only after every .bin is complete, so a mid-dump failure
        // never leaves a full manifest pointing at a truncated region.
        let regions_path = path.join("regions.json");
        std::fs::write(&regions_path, serde_json::to_vec(&metas)?)?;
        Ok(json!({ "path": path.display().to_string(), "regions": metas.len() }))
    }

    /// The bounded RAM regions `dump_memory` snapshots — every `dumpable` region whose CPU is
    /// attached (ARM7-hosted regions are skipped when no ARM7 connection is present).
    pub(super) fn dump_regions(&self) -> Vec<NdsRegion> {
        MEMORY_REGIONS
            .iter()
            .copied()
            .filter(|r| r.dumpable && (r.cpu != CpuId::Arm7 || self.arm7.is_some()))
            .collect()
    }

    /// Read `length` bytes at region-relative `start` from `memory_type` over the routed CPU's GDB
    /// `m` path, in bounded chunks. Both CPU endpoints share one emulation scheduler, so a single
    /// routed interrupt stops mutation of shared Main RAM for the whole read. Each chunk's decoded
    /// length is checked against the request, so a short or failed stub read errors cleanly instead
    /// of yielding truncated bytes.
    pub(super) fn read_region_bytes(
        &mut self,
        memory_type: &str,
        start: u64,
        length: usize,
    ) -> NdsResult<Vec<u8>> {
        let region = *memory_region(memory_type).ok_or_else(|| {
            NdsBridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        if !matches!(start.checked_add(length as u64), Some(end) if end <= region.size) {
            return Err(NdsBridgeError::BadParams(format!(
                "{memory_type} access out of range: offset {start:#x}+{length:#x} exceeds region size {size:#x}",
                size = region.size
            )));
        }
        self.with_shared_scheduler_frozen(|bridge| bridge.read_region_chunks(region, start, length))
    }

    /// The chunked `m`-read loop for a bulk read, assuming the shared scheduler is already stopped
    /// (see `with_shared_scheduler_frozen`). `read_abs_hex`'s own `with_frozen` is a no-op here since
    /// the routed core is held, so the whole read is one consistent snapshot.
    pub(super) fn read_region_chunks(
        &mut self,
        region: NdsRegion,
        start: u64,
        length: usize,
    ) -> NdsResult<Vec<u8>> {
        let base = region.base;
        let conn = self.cpu_mut(region.cpu)?;
        let mut out = Vec::with_capacity(length);
        let mut offset = 0usize;
        while offset < length {
            let chunk = MAX_READ_CHUNK.min(length - offset);
            let addr = base + start + offset as u64;
            let hex = conn.read_abs_hex(addr, chunk)?;
            let bytes = hex::decode(&hex)
                .map_err(|_| NdsBridgeError::Emulator("GDB returned invalid hex".into()))?;
            if bytes.len() != chunk {
                return Err(NdsBridgeError::Emulator(format!(
                    "short GDB read at {addr:#x}: requested {chunk} bytes, got {}",
                    bytes.len()
                )));
            }
            out.extend_from_slice(&bytes);
            offset += chunk;
        }
        Ok(out)
    }

    /// Run `f` while DeSmuME's shared scheduler is stopped, then restore the prior logical CPU
    /// states. Interrupting either running core stops global execution. Only one endpoint may be
    /// interrupted: after the first stop, another endpoint cannot service a second interrupt until
    /// execution resumes.
    pub(super) fn with_shared_scheduler_frozen<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> NdsResult<T>,
    ) -> NdsResult<T> {
        let paused = if !self.arm9.frozen {
            if self.arm9.pause()? {
                None
            } else {
                Some(CpuId::Arm9)
            }
        } else if self.arm7.as_ref().is_some_and(|arm7| !arm7.frozen) {
            if self
                .arm7
                .as_mut()
                .expect("checked attached ARM7 above")
                .pause()?
            {
                None
            } else {
                Some(CpuId::Arm7)
            }
        } else {
            None
        };
        let r = f(self);
        match paused {
            Some(CpuId::Arm9) => {
                let _ = self.arm9.resume();
            }
            Some(CpuId::Arm7) => {
                if let Some(arm7) = self.arm7.as_mut() {
                    let _ = arm7.resume();
                }
            }
            None => {}
        }
        r
    }

    pub(super) fn get_state(&mut self, params: &Value) -> NdsResult<Value> {
        let cpu = cpu_from_params(params)?;
        let hex = self.cpu_mut(cpu)?.read_regs_hex()?;
        Ok(json!({ "cpu": cpu.as_str(), "state": state_from_arm_regs_hex(&hex) }))
    }

    pub(super) fn step(&mut self, params: &Value) -> NdsResult<Value> {
        // NDS는 프레임 step을 못 한다 — GDB-RSP엔 프레임 개념이 없고, DeSmuME fork에 run-frames 훅이 아직 없다.
        // 이전 호스트와 직접 wire 호출을 위한 호환 경로다. 현재 공개 MCP의 instruction step은
        // wire step_instructions로 들어온다. unit이 없는데 frames가 오면 진짜 프레임-step 요청이라 거부한다 —
        // 명령으로 조용히 오해석하면 (60프레임→60명령) freeze-step/tap이 어긋난다.
        match params.get("unit").and_then(Value::as_str) {
            Some("instructions") => {}
            Some(other) => {
                return Err(NdsBridgeError::Unsupported(format!(
                    "step unit={other} (nds bridge steps by instructions only)"
                )));
            }
            None => {
                if params.get("frames").is_some() {
                    return Err(NdsBridgeError::Unsupported(
                        "nds bridge: 프레임 step 미지원 — GDB-RSP엔 프레임 개념이 없다. 명령 단위 진행은 \
                         step(unit=instructions)를 쓰라. DeSmuME fork도 frame-run primitive를 제공하지 않는다"
                            .into(),
                    ));
                }
            }
        }
        let count = step_count(params)?;
        self.step_cpu(params, count)
    }

    pub(super) fn step_instructions(&mut self, params: &Value) -> NdsResult<Value> {
        let count = step_count(params)?;
        self.step_cpu(params, count)
    }

    pub(super) fn step_cpu(&mut self, params: &Value, count: u64) -> NdsResult<Value> {
        self.step_cpu_with_budget(
            params,
            count,
            crate::live::temporal::MAX_SYNC_OPERATION_TIME,
        )
    }

    pub(super) fn step_cpu_with_budget(
        &mut self,
        params: &Value,
        count: u64,
        budget: Duration,
    ) -> NdsResult<Value> {
        let cpu = cpu_from_params(params)?;
        let conn = self.cpu_mut(cpu)?;
        let state = conn.step_instructions_and_read_state(count, budget)?;
        self.set_scheduler_frozen(true);
        let pc = state.get("cpu.pc").and_then(Value::as_u64);
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "count": count,
            "cpu": cpu.as_str(),
            "pc": pc,
            "state": state,
        }))
    }
}
