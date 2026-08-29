use super::*;

const MAX_MEMORY_CHUNK: usize = 0x4000;
const MAX_PATTERN_SCAN: usize = 128 * 1024;
const TRACE_CAPACITY: usize = 4096;

impl Np2kaiHost {
    pub(super) fn read_memory(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let length = required_num(params, "length")?;
        let address = region_address(params, length)?;
        let bytes = self.read_absolute(
            address,
            usize::try_from(length).map_err(|_| {
                Np2kaiError::BadParams("memory length does not fit this host".into())
            })?,
        )?;
        Ok(json!({"hex": hex::encode(bytes)}))
    }

    pub(super) fn write_memory(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let raw = required_str(params, "hex")?;
        if raw.len() % 2 != 0 {
            return Err(Np2kaiError::BadParams("hex must have even length".into()));
        }
        let bytes =
            hex::decode(raw).map_err(|_| Np2kaiError::BadParams("hex decode failed".into()))?;
        if bytes.is_empty() {
            return Err(Np2kaiError::BadParams("hex must contain data".into()));
        }
        let address = region_address(params, bytes.len() as u64)?;
        if unsafe { (self.api.debug_write_memory)(address, bytes.as_ptr(), bytes.len()) } == 0 {
            return Err(Np2kaiError::Core("NP2kai memory write failed".into()));
        }
        Ok(json!({"written": bytes.len()}))
    }

    pub(super) fn find_pattern(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let region = requested_region(params)?;
        let pattern = hex::decode(required_str(params, "hex")?)
            .map_err(|_| Np2kaiError::BadParams("hex decode failed".into()))?;
        if pattern.is_empty() {
            return Err(Np2kaiError::BadParams(
                "hex must contain at least one byte".into(),
            ));
        }
        let start = optional_num(params, "start")?.unwrap_or(0);
        if start > u64::from(region.size) {
            return Err(Np2kaiError::BadParams(format!(
                "{} scan start exceeds region size",
                region.name
            )));
        }
        let available = u64::from(region.size) - start;
        let requested = optional_num(params, "length")?.unwrap_or(available);
        if requested > available {
            return Err(Np2kaiError::BadParams(format!(
                "{} scan range exceeds region size",
                region.name
            )));
        }
        let scan_len = usize::try_from(requested.min(MAX_PATTERN_SCAN as u64)).unwrap();
        let bytes = self.read_absolute(region.base + start as u32, scan_len)?;
        let align = optional_num(params, "align")?.unwrap_or(1).max(1) as usize;
        let max_matches = optional_num(params, "max_matches")?
            .unwrap_or(256)
            .clamp(1, 4096) as usize;
        let mut matches = Vec::new();
        let mut more_matches = false;
        for offset in 0..=bytes.len().saturating_sub(pattern.len()) {
            if offset % align == 0 && bytes[offset..].starts_with(&pattern) {
                if matches.len() == max_matches {
                    more_matches = true;
                    break;
                }
                matches.push(start as usize + offset);
            }
        }
        Ok(json!({
            "matches": matches,
            "count": matches.len(),
            "start": start,
            "scanned": scan_len,
            "truncated_scan": requested > scan_len as u64,
            "truncated_matches": more_matches,
            "truncated": requested > scan_len as u64 || more_matches
        }))
    }

    pub(super) fn dump_memory(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let path = absolute_path_param(params, "path")?;
        fs::create_dir_all(&path)?;
        let mut regions = Vec::new();
        for name in DUMP_REGIONS {
            let region = memory_region(name).expect("declared dump region");
            let bytes = self.read_absolute(region.base, region.size as usize)?;
            atomic_write(&path.join(format!("{name}.bin")), &bytes)?;
            regions.push(json!({
                "name": name,
                "memory_type": name,
                "base_address": region.base,
                "size": region.size
            }));
        }
        atomic_write(&path.join("regions.json"), &serde_json::to_vec(&regions)?)?;
        Ok(json!({"path": path.display().to_string(), "regions": regions.len()}))
    }

    pub(super) fn get_state(&mut self) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        Ok(json!({"state": registers_value(self.registers()?)}))
    }

    pub(super) fn probe(&mut self, params: &Value) -> Np2kaiResult<Value> {
        let state = required_str(params, "state")?.to_string();
        self.load_state(&json!({"path": state}))?;
        let frames = optional_num(params, "frame")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(0);
        let completed = self.run_exact_frames(frames)?;
        let mut read_params = params.clone();
        if let Some(object) = read_params.as_object_mut() {
            object.remove("state");
            object.remove("frame");
            object.remove("frames");
        }
        let memory = self.read_memory(&read_params)?;
        Ok(json!({
            "status": if completed == frames {"completed"} else {"interrupted"},
            "requested_frames": frames,
            "completed_frames": completed,
            "frame": self.frame,
            "state": "frozen",
            "hex": memory["hex"]
        }))
    }

    pub(super) fn set_breakpoint(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let kind = params.get("kind").and_then(Value::as_str).unwrap_or("exec");
        let native_kind = match kind {
            "exec" => BP_EXEC,
            "read" => BP_READ,
            "write" => BP_WRITE,
            "access" => BP_ACCESS,
            _ => {
                return Err(Np2kaiError::BadParams(
                    "NP2kai PC-98 supports exec/read/write/access breakpoints".into(),
                ))
            }
        };
        let region = requested_region(params)?;
        let start = required_num(params, "start")?;
        let end = optional_num(params, "end")?.unwrap_or(start).max(start);
        if end >= u64::from(region.size) {
            return Err(Np2kaiError::BadParams(format!(
                "{} breakpoint range exceeds region size {:#x}",
                region.name, region.size
            )));
        }
        let pause_on_hit = params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let snapshots = parse_snapshots(params.get("snapshot"))?;
        if !pause_on_hit && !snapshots.is_empty() {
            return Err(Np2kaiError::BadParams(
                "NP2kai snapshots require pause_on_hit=true; hit registers describe the access boundary and snapshots describe the resulting frozen boundary".into(),
            ));
        }
        let (pc_min, pc_max) = breakpoint_pc_range(params)?;
        let (has_value, value, value_mask, value_len) = breakpoint_value_filter(params, kind)?;
        let id = self.allocate_breakpoint_id()?;
        let native = NativeBreakpoint {
            id,
            kind: native_kind,
            start: region.base + start as u32,
            end: region.base + end as u32,
            pause_on_hit: u32::from(pause_on_hit),
            has_pc_min: u32::from(pc_min.is_some()),
            pc_min: pc_min.unwrap_or(0),
            has_pc_max: u32::from(pc_max.is_some()),
            pc_max: pc_max.unwrap_or(0),
            has_value: u32::from(has_value),
            value,
            value_mask,
            value_len,
            ..NativeBreakpoint::default()
        };
        self.install_breakpoint(native)?;
        self.breakpoints.insert(
            id,
            DebugBreakpoint {
                kind: kind.into(),
                start: Some(start as u32),
                end: Some(end as u32),
                memory_type: Some(region.name.into()),
                pause_on_hit,
                snapshots,
                register: None,
                min: None,
                max: None,
            },
        );
        Ok(json!({"id": id}))
    }

    pub(super) fn watch_register(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let raw = params
            .get("register")
            .and_then(Value::as_str)
            .unwrap_or("sp");
        let (register, index) = normalize_register(raw)?;
        let min = optional_num(params, "min")?.unwrap_or(0);
        let max = optional_num(params, "max")?.unwrap_or(u64::from(u32::MAX));
        if min > max || max > u64::from(u32::MAX) {
            return Err(Np2kaiError::BadParams(
                "register range must satisfy 0 <= min <= max <= 0xffffffff".into(),
            ));
        }
        let pause_on_hit = params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let id = self.allocate_breakpoint_id()?;
        self.install_breakpoint(NativeBreakpoint {
            id,
            kind: BP_REGISTER,
            pause_on_hit: u32::from(pause_on_hit),
            register_index: index,
            register_min: min as u32,
            register_max: max as u32,
            ..NativeBreakpoint::default()
        })?;
        self.breakpoints.insert(
            id,
            DebugBreakpoint {
                kind: "reg".into(),
                start: None,
                end: None,
                memory_type: None,
                pause_on_hit,
                snapshots: Vec::new(),
                register: Some(register.into()),
                min: Some(min as u32),
                max: Some(max as u32),
            },
        );
        Ok(json!({"id": id}))
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> Np2kaiResult<Value> {
        let id = required_num(params, "id")?;
        if !self.breakpoints.contains_key(&id) {
            return Err(Np2kaiError::BadParams(format!(
                "unknown breakpoint id: {id}"
            )));
        }
        self.drain_native_events()?;
        if unsafe { (self.api.debug_clear_breakpoint)(id) } == 0 {
            return Err(Np2kaiError::Core(format!(
                "NP2kai failed to clear breakpoint {id}"
            )));
        }
        self.breakpoints.remove(&id);
        Ok(json!({"cleared": id}))
    }

    pub(super) fn list_breakpoints(&self) -> Np2kaiResult<Value> {
        let rows = self
            .breakpoints
            .iter()
            .map(|(id, bp)| {
                if bp.kind == "reg" {
                    json!({"id": id, "kind": "reg", "register": bp.register,
                    "min": bp.min, "max": bp.max, "pause_on_hit": bp.pause_on_hit})
                } else {
                    json!({"id": id, "kind": bp.kind, "memory_type": bp.memory_type,
                    "start": bp.start, "end": bp.end, "pause_on_hit": bp.pause_on_hit,
                    "snapshot": bp.snapshots.iter().map(snapshot_text).collect::<Vec<_>>()})
                }
            })
            .collect::<Vec<_>>();
        Ok(json!({"breakpoints": rows}))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> Np2kaiResult<Value> {
        self.drain_native_events()?;
        let cleared = self.breakpoints.keys().copied().collect::<Vec<_>>();
        unsafe { (self.api.debug_clear_all_breakpoints)() };
        self.breakpoints.clear();
        self.break_on_reset_id = None;
        Ok(json!({"cleared": cleared}))
    }

    pub(super) fn poll_events(&mut self, params: &Value) -> Np2kaiResult<Value> {
        let filter = optional_num(params, "breakpoint_id")?;
        self.drain_native_events()?;
        let mut events = Vec::new();
        let mut remaining = Vec::new();
        for event in std::mem::take(&mut self.pending_events) {
            if filter.is_none_or(|id| event["breakpoint_id"].as_u64() == Some(id)) {
                events.push(event);
            } else {
                remaining.push(event);
            }
        }
        self.pending_events = remaining;
        let dropped = unsafe { (self.api.debug_take_dropped_events)() };
        Ok(json!({"events": events, "dropped": dropped, "pending": self.pending_events.len()}))
    }

    fn drain_native_events(&mut self) -> Np2kaiResult<()> {
        loop {
            let mut native = NativeEvent::default();
            if unsafe { (self.api.debug_poll_event)(&mut native) } == 0 {
                break;
            }
            let metadata = self.breakpoints.get(&native.breakpoint_id).cloned();
            let kind = native_kind_name(native.kind);
            let mut event = json!({
                "type": if native.kind == BP_RESET {"reset"} else {"breakpoint_hit"},
                "id": native.breakpoint_id,
                "breakpoint_id": native.breakpoint_id,
                "sequence": native.sequence,
                "kind": kind,
                "address": native.address,
                "size": native.size,
                "paused": native.paused != 0,
                "regs": registers_value(native.registers),
            });
            if let Some(value) = authoritative_event_value(native.kind, native.value) {
                event["value"] = json!(value);
            }
            if let Some(metadata) = metadata {
                if !metadata.snapshots.is_empty() {
                    event["snapshot"] = Value::Array(self.capture_snapshots(&metadata.snapshots)?);
                }
            }
            self.pending_events.push(event);
        }
        Ok(())
    }

    pub(super) fn break_on_reset(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(id) = self.break_on_reset_id.take() {
            if unsafe { (self.api.debug_clear_breakpoint)(id) } == 0 {
                return Err(Np2kaiError::Core(format!(
                    "NP2kai failed to clear reset breakpoint {id}"
                )));
            }
        }
        if enabled {
            let id = self.allocate_breakpoint_id()?;
            self.install_breakpoint(NativeBreakpoint {
                id,
                kind: BP_RESET,
                pause_on_hit: 1,
                ..NativeBreakpoint::default()
            })?;
            self.break_on_reset_id = Some(id);
        }
        Ok(json!({"enabled": enabled, "system": "pc98", "mode": "machine_reset_notifier"}))
    }

    pub(super) fn step_instructions(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.require_frozen("step_instructions")?;
        self.ensure_initialized()?;
        let count = optional_num(params, "count")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(1);
        if !(1..=MAX_SYNC_FRAMES).contains(&count) {
            return Err(Np2kaiError::BadParams(format!(
                "instruction count must be in 1..={MAX_SYNC_FRAMES}"
            )));
        }
        let mut completed = 0;
        let mut interrupted = false;
        unsafe { (self.api.debug_clear_stop)() };
        while completed < count {
            let outcome = unsafe { (self.api.debug_step_instruction)() };
            match outcome {
                INSTRUCTION_STEP_FAILED => {
                    return Err(Np2kaiError::Core("NP2kai instruction step failed".into()));
                }
                INSTRUCTION_STEP_EXECUTED => completed += 1,
                INSTRUCTION_STEP_PREEMPTED => {}
                other => {
                    return Err(Np2kaiError::Core(format!(
                        "NP2kai instruction step returned an unknown outcome: {other}"
                    )));
                }
            }
            if unsafe { (self.api.debug_stop_requested)() } != 0 {
                interrupted = true;
                break;
            }
            if outcome == INSTRUCTION_STEP_PREEMPTED {
                return Err(Np2kaiError::Core(
                    "NP2kai instruction step reported preemption without a stop".into(),
                ));
            }
        }
        unsafe { (self.api.debug_clear_stop)() };
        self.drain_native_trace()?;
        Ok(json!({
            "status": if interrupted {"interrupted"} else {"completed"},
            "reason": if interrupted {json!("breakpoint")} else {Value::Null},
            "unit": "instructions", "count": count, "completed": completed,
            "state": "frozen", "frame": self.frame
        }))
    }

    pub(super) fn disassemble(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let mut address = u32::try_from(required_num(params, "address")?)
            .map_err(|_| Np2kaiError::BadParams("address exceeds 32-bit PC-98 space".into()))?;
        let count = optional_num(params, "count")?.unwrap_or(8).clamp(1, 256);
        let mut instructions = Vec::new();
        for _ in 0..count {
            let current = address;
            let mut text = [0_i8; 256];
            let mut next = 0_u32;
            let mut bytes = [0_u8; 16];
            let mut byte_count = bytes.len();
            if unsafe {
                (self.api.debug_disassemble)(
                    current,
                    text.as_mut_ptr(),
                    text.len(),
                    &mut next,
                    bytes.as_mut_ptr(),
                    &mut byte_count,
                )
            } == 0
            {
                return Err(Np2kaiError::BadParams(format!(
                    "NP2kai cannot disassemble address {current:#x} in the current CPU mode"
                )));
            }
            let text = unsafe { CStr::from_ptr(text.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            byte_count = byte_count.min(bytes.len());
            instructions.push(json!({"address": current, "bytes": hex::encode(&bytes[..byte_count]), "text": text}));
            if next <= current {
                break;
            }
            address = next;
        }
        Ok(json!({"instructions": instructions}))
    }

    pub(super) fn set_trace(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        unsafe { (self.api.debug_set_trace)(i32::from(enabled)) };
        self.tracing = enabled;
        if !enabled {
            self.trace_rows.clear();
            self.dropped_trace = 0;
        }
        Ok(json!({"tracing": enabled, "storage": "bounded_memory", "capacity": TRACE_CAPACITY}))
    }

    pub(super) fn get_trace(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.drain_native_trace()?;
        let count = optional_num(params, "count")?
            .unwrap_or(64)
            .clamp(1, TRACE_CAPACITY as u64) as usize;
        let start = self.trace_rows.len().saturating_sub(count);
        Ok(json!({
            "trace": self.trace_rows[start..], "tracing": self.tracing,
            "total": self.trace_rows.len(), "dropped": self.dropped_trace
        }))
    }

    pub(super) fn call_stack(&mut self) -> Np2kaiResult<Value> {
        self.ensure_initialized()?;
        self.drain_native_trace()?;
        if self.tracing && !self.trace_rows.is_empty() {
            let mut frames = Vec::new();
            for row in &self.trace_rows {
                let text = row["text"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if text.starts_with("call") {
                    frames.push(json!({"pc": row["pc"], "text": row["text"]}));
                } else if text.starts_with("ret") {
                    frames.pop();
                }
            }
            return Ok(
                json!({"call_stack": frames.iter().map(|frame| frame["pc"].clone()).collect::<Vec<_>>(),
                "frames": frames, "depth": frames.len(), "method": "trace", "tracing": true}),
            );
        }
        self.call_stack_from_frame_pointer()
    }

    pub(super) fn change_media(&mut self, params: &Value) -> Np2kaiResult<Value> {
        self.require_frozen("change_media")?;
        self.ensure_initialized()?;
        let device = params
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or("hdd0");
        if device != "hdd0" {
            return Err(Np2kaiError::BadParams(
                "NP2kai exposes only media device hdd0".into(),
            ));
        }
        if params
            .get("eject")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(Np2kaiError::BadState(
                "NP2kai hdd0 must remain loaded".into(),
            ));
        }
        let source = absolute_path_param(params, "path")?;
        validate_content(&source)?;
        let source = source.canonicalize()?;
        let sha256 = sha256_file(&source)?;
        if let Some(expected) = params.get("expected_sha256").and_then(Value::as_str) {
            if !expected.eq_ignore_ascii_case(&sha256) {
                return Err(Np2kaiError::BadParams(format!(
                    "media SHA-256 mismatch: expected {expected}, got {sha256}"
                )));
            }
        }
        let media_dir = self.runtime_home.join("media");
        ensure_plain_directory(&media_dir)?;
        let staged = media_dir.join(format!("{sha256}.hdi"));
        match fs::symlink_metadata(&staged) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || sha256_file(&staged)? != sha256
                {
                    return Err(Np2kaiError::BadState(format!(
                        "managed media path is not the expected plain regular file: {}",
                        staged.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let partial = media_dir.join(format!(".{sha256}.partial-{}", std::process::id()));
                copy_file_exclusive(&source, &partial)?;
                if sha256_file(&partial)? != sha256 {
                    let _ = fs::remove_file(&partial);
                    return Err(Np2kaiError::Core(
                        "managed media copy digest mismatch".into(),
                    ));
                }
                fs::rename(&partial, &staged)?;
            }
            Err(error) => return Err(error.into()),
        }
        let staged = staged.canonicalize()?;
        let staged_size = staged.metadata()?.len();
        let staged_sha1 = crate::rom::sha1_of_file(&staged)?;
        let staged_c = path_cstring(&staged)?;
        let previous = self.content_path.clone();
        let previous_c = path_cstring(&previous)?;
        let changed = unsafe { (self.api.debug_change_hdd)(staged_c.as_ptr()) } != 0;
        let verified = self.current_hdd_bytes().as_deref() == Some(staged_c.as_bytes());
        if !changed || !verified {
            let primary = if changed {
                "NP2kai did not report the staged hdd0 path after media change"
            } else {
                "NP2kai rejected hdd0 media change"
            };
            return Err(self.media_change_failure(primary, &previous_c));
        }
        self.content_path = staged;
        self.content_size = staged_size;
        self.content_sha1 = staged_sha1;
        self.content_sha256 = sha256;
        Ok(json!({
            "status": "completed", "action": "mount", "device": "hdd0", "state": "frozen",
            "previous": {"mounted": true, "path": previous.display().to_string()},
            "current": {"mounted": true, "path": self.content_path.display().to_string(), "readonly": false},
            "media": {"path": self.content_path.display().to_string(), "sha1": self.content_sha1,
                "sha256": self.content_sha256, "size": self.content_size}
        }))
    }

    fn read_absolute(&self, address: u32, length: usize) -> Np2kaiResult<Vec<u8>> {
        if length == 0 || length > 0x100000 || address as usize + length > 0x100000 {
            return Err(Np2kaiError::BadParams(
                "physical memory access is out of range".into(),
            ));
        }
        let mut bytes = vec![0_u8; length];
        if unsafe { (self.api.debug_read_memory)(address, bytes.as_mut_ptr(), bytes.len()) } == 0 {
            return Err(Np2kaiError::Core("NP2kai memory read failed".into()));
        }
        Ok(bytes)
    }

    pub(super) fn current_hdd_bytes(&self) -> Option<Vec<u8>> {
        let mounted = unsafe { (self.api.debug_current_hdd)() };
        (!mounted.is_null()).then(|| unsafe { CStr::from_ptr(mounted) }.to_bytes().to_vec())
    }

    pub(super) fn media_change_failure(&self, primary: &str, previous: &CString) -> Np2kaiError {
        let rollback_call = unsafe { (self.api.debug_change_hdd)(previous.as_ptr()) } != 0;
        let observed = self.current_hdd_bytes();
        let restored = rollback_call && observed.as_deref() == Some(previous.as_bytes());
        let rollback = if restored { "restored" } else { "failed" };
        let current = observed
            .as_deref()
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .unwrap_or_else(|| "unavailable".into());
        Np2kaiError::Core(format!(
            "{primary}; rollback={rollback}; current_media={current}"
        ))
    }

    fn registers(&self) -> Np2kaiResult<NativeRegisters> {
        let mut registers = NativeRegisters::default();
        if unsafe { (self.api.debug_get_registers)(&mut registers) } == 0 {
            return Err(Np2kaiError::Core("NP2kai register read failed".into()));
        }
        Ok(registers)
    }

    fn allocate_breakpoint_id(&mut self) -> Np2kaiResult<u64> {
        let id = self.next_breakpoint_id;
        self.next_breakpoint_id = self
            .next_breakpoint_id
            .checked_add(1)
            .ok_or_else(|| Np2kaiError::Core("breakpoint id exhausted".into()))?;
        Ok(id)
    }

    fn install_breakpoint(&self, breakpoint: NativeBreakpoint) -> Np2kaiResult<()> {
        if unsafe { (self.api.debug_set_breakpoint)(&breakpoint) } == 0 {
            return Err(Np2kaiError::Core(
                "NP2kai breakpoint table is full or rejected the request".into(),
            ));
        }
        Ok(())
    }

    fn capture_snapshots(&self, specs: &[SnapshotSpec]) -> Np2kaiResult<Vec<Value>> {
        specs
            .iter()
            .map(|spec| {
                let region = memory_region(&spec.memory_type).expect("validated snapshot region");
                let bytes = self.read_absolute(region.base + spec.address, spec.length as usize)?;
                Ok(
                    json!({"memory_type": spec.memory_type, "address": spec.address,
                "length": spec.length, "hex": hex::encode(bytes)}),
                )
            })
            .collect()
    }

    pub(super) fn drain_native_trace(&mut self) -> Np2kaiResult<()> {
        loop {
            let mut native = NativeTrace::default();
            if unsafe { (self.api.debug_poll_trace)(&mut native) } == 0 {
                break;
            }
            let text = unsafe { CStr::from_ptr(native.text.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            let regs = registers_value(native.registers);
            self.trace_rows.push(json!({
                "sequence": native.sequence,
                "pc": native.registers.cs_base.wrapping_add(native.registers.eip),
                "text": text,
                "regs": regs
            }));
            if self.trace_rows.len() > TRACE_CAPACITY {
                self.trace_rows.remove(0);
                self.dropped_trace = self.dropped_trace.saturating_add(1);
            }
        }
        self.dropped_trace = self
            .dropped_trace
            .saturating_add(unsafe { (self.api.debug_take_dropped_trace)() });
        Ok(())
    }

    fn call_stack_from_frame_pointer(&self) -> Np2kaiResult<Value> {
        let regs = self.registers()?;
        let real_mode = regs.cr0 & 1 == 0;
        let (pointer_size, segment_base, mask) = if real_mode {
            (2_usize, u32::from(regs.ss) << 4, 0xffff_u32)
        } else {
            (4_usize, 0, u32::MAX)
        };
        let mut frame_pointer = regs.ebp & mask;
        let mut frames = Vec::new();
        for _ in 0..64 {
            if frame_pointer == 0 {
                break;
            }
            let address = segment_base.saturating_add(frame_pointer);
            let Ok(bytes) = self.read_absolute(address, pointer_size * 2) else {
                break;
            };
            let saved = little_value(&bytes[..pointer_size]);
            let returned = little_value(&bytes[pointer_size..]);
            frames.push(json!({"pc": returned, "frame_pointer": frame_pointer}));
            if saved <= frame_pointer {
                break;
            }
            frame_pointer = saved & mask;
        }
        Ok(json!({
            "call_stack": frames.iter().map(|frame| frame["pc"].clone()).collect::<Vec<_>>(),
            "frames": frames, "depth": frames.len(), "method": "frame_pointer",
            "mode": if real_mode {"real16"} else {"protected32"},
            "pointer_size": pointer_size, "tracing": false
        }))
    }
}

fn required_num(params: &Value, key: &str) -> Np2kaiResult<u64> {
    optional_num(params, key)?
        .ok_or_else(|| Np2kaiError::BadParams(format!("missing required param: {key}")))
}

fn required_str<'a>(params: &'a Value, key: &str) -> Np2kaiResult<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Np2kaiError::BadParams(format!("missing required param: {key}")))
}

fn memory_region(name: &str) -> Option<&'static MemoryRegion> {
    MEMORY_REGIONS.iter().find(|region| region.name == name)
}

pub(super) fn memory_type_names() -> Vec<&'static str> {
    MEMORY_REGIONS.iter().map(|region| region.name).collect()
}

pub(super) fn region_sizes_value() -> Value {
    Value::Object(
        MEMORY_REGIONS
            .iter()
            .map(|region| (region.name.to_string(), json!(region.size)))
            .collect(),
    )
}

fn requested_region(params: &Value) -> Np2kaiResult<&'static MemoryRegion> {
    let name = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("physical");
    memory_region(name)
        .ok_or_else(|| Np2kaiError::BadParams(format!("unsupported memory_type: {name}")))
}

pub(super) fn region_address(params: &Value, length: u64) -> Np2kaiResult<u32> {
    if length == 0 || length > MAX_MEMORY_CHUNK as u64 {
        return Err(Np2kaiError::BadParams(format!(
            "memory length must be in 1..={MAX_MEMORY_CHUNK}"
        )));
    }
    let region = requested_region(params)?;
    let offset = required_num(params, "address")?;
    if !matches!(offset.checked_add(length), Some(end) if end <= u64::from(region.size)) {
        return Err(Np2kaiError::BadParams(format!(
            "{} access out of range: offset {offset:#x}+{length:#x} exceeds region size {:#x}",
            region.name, region.size
        )));
    }
    Ok(region.base + offset as u32)
}

pub(super) fn breakpoint_pc_range(params: &Value) -> Np2kaiResult<(Option<u32>, Option<u32>)> {
    let convert = |key: &str| -> Np2kaiResult<Option<u32>> {
        optional_num(params, key)?
            .map(|value| {
                u32::try_from(value).map_err(|_| {
                    Np2kaiError::BadParams(format!("{key} exceeds the 32-bit PC-98 address space"))
                })
            })
            .transpose()
    };
    let pc_min = convert("pc_min")?;
    let pc_max = convert("pc_max")?;
    if matches!((pc_min, pc_max), (Some(min), Some(max)) if min > max) {
        return Err(Np2kaiError::BadParams("pc_min must be <= pc_max".into()));
    }
    Ok((pc_min, pc_max))
}

pub(super) fn breakpoint_value_filter(
    params: &Value,
    kind: &str,
) -> Np2kaiResult<(bool, u32, u32, u32)> {
    let has_value = params.get("value").is_some()
        || params.get("value_mask").is_some()
        || params.get("value_len").is_some();
    if has_value && kind != "write" {
        return Err(Np2kaiError::BadParams(
            "NP2kai value filters apply only to write breakpoints; read values are not authoritative at the native hook".into(),
        ));
    }
    let value_len = optional_num(params, "value_len")?.unwrap_or(1);
    if has_value && !(1..=4).contains(&value_len) {
        return Err(Np2kaiError::BadParams("value_len must be in 1..=4".into()));
    }
    let bit_mask = if value_len == 4 {
        u32::MAX
    } else {
        (1_u32 << (value_len * 8)) - 1
    };
    let value = optional_num(params, "value")?.unwrap_or(0);
    let value_mask = optional_num(params, "value_mask")?.unwrap_or(u64::from(bit_mask));
    if value > u64::from(bit_mask) || value_mask > u64::from(bit_mask) {
        return Err(Np2kaiError::BadParams(format!(
            "value and value_mask must fit value_len={value_len}"
        )));
    }
    Ok((has_value, value as u32, value_mask as u32, value_len as u32))
}

pub(super) fn parse_snapshots(raw: Option<&Value>) -> Np2kaiResult<Vec<SnapshotSpec>> {
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let items = raw
        .as_array()
        .ok_or_else(|| Np2kaiError::BadParams("snapshot must be a list".into()))?;
    let mut result = Vec::new();
    for item in items {
        let text = item
            .as_str()
            .ok_or_else(|| Np2kaiError::BadParams(format!("invalid snapshot spec: {item}")))?;
        let parts = text.split(':').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(Np2kaiError::BadParams(format!(
                "invalid snapshot spec: {text}"
            )));
        }
        let region = memory_region(parts[0]).ok_or_else(|| {
            Np2kaiError::BadParams(format!("unsupported snapshot memory_type: {}", parts[0]))
        })?;
        let address = crate::numparse::parse_num_str(parts[1]).map_err(|_| {
            Np2kaiError::BadParams(format!("invalid snapshot address: {}", parts[1]))
        })?;
        let length = crate::numparse::parse_num_str(parts[2]).map_err(|_| {
            Np2kaiError::BadParams(format!("invalid snapshot length: {}", parts[2]))
        })?;
        if length == 0
            || length > MAX_MEMORY_CHUNK as u64
            || address
                .checked_add(length)
                .is_none_or(|end| end > u64::from(region.size))
        {
            return Err(Np2kaiError::BadParams(format!(
                "snapshot range exceeds {}",
                region.name
            )));
        }
        result.push(SnapshotSpec {
            memory_type: region.name.into(),
            address: address as u32,
            length: length as u32,
        });
    }
    Ok(result)
}

fn snapshot_text(spec: &SnapshotSpec) -> String {
    format!(
        "{}:{:#x}:{:#x}",
        spec.memory_type, spec.address, spec.length
    )
}

pub(super) fn normalize_register(raw: &str) -> Np2kaiResult<(&'static str, u32)> {
    let mut name = raw.trim().to_ascii_lowercase();
    if let Some(stripped) = name.strip_prefix("cpu.") {
        name = stripped.into();
    }
    let result = match name.as_str() {
        "eax" | "ax" => ("eax", 0),
        "ecx" | "cx" => ("ecx", 1),
        "edx" | "dx" => ("edx", 2),
        "ebx" | "bx" => ("ebx", 3),
        "esp" | "sp" => ("esp", 4),
        "ebp" | "bp" => ("ebp", 5),
        "esi" | "si" => ("esi", 6),
        "edi" | "di" => ("edi", 7),
        "eip" | "ip" | "offset_pc" => ("eip", 8),
        "eflags" | "flags" => ("eflags", 9),
        "cs" => ("cs", 10),
        "ss" => ("ss", 11),
        "ds" => ("ds", 12),
        "es" => ("es", 13),
        "fs" => ("fs", 14),
        "gs" => ("gs", 15),
        _ => {
            return Err(Np2kaiError::BadParams(format!(
                "unsupported PC-98 register: {raw}"
            )))
        }
    };
    Ok(result)
}

fn registers_value(regs: NativeRegisters) -> Value {
    json!({
        "cpu.eax": regs.eax, "cpu.ecx": regs.ecx, "cpu.edx": regs.edx, "cpu.ebx": regs.ebx,
        "cpu.esp": regs.esp, "cpu.ebp": regs.ebp, "cpu.esi": regs.esi, "cpu.edi": regs.edi,
        "cpu.eip": regs.eip, "cpu.offset_pc": regs.eip,
        "cpu.pc": regs.cs_base.wrapping_add(regs.eip), "cpu.eflags": regs.eflags,
        "cpu.cs": regs.cs, "cpu.ss": regs.ss, "cpu.ds": regs.ds, "cpu.es": regs.es,
        "cpu.fs": regs.fs, "cpu.gs": regs.gs, "cpu.cs_base": regs.cs_base, "cpu.cr0": regs.cr0
    })
}

fn native_kind_name(kind: u32) -> &'static str {
    match kind {
        BP_EXEC => "exec",
        BP_READ => "read",
        BP_WRITE => "write",
        BP_ACCESS => "access",
        BP_REGISTER => "reg",
        BP_RESET => "reset",
        _ => "unknown",
    }
}

pub(super) fn authoritative_event_value(kind: u32, value: u32) -> Option<u32> {
    matches!(kind, BP_WRITE | BP_REGISTER).then_some(value)
}

fn little_value(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .enumerate()
        .fold(0_u32, |value, (index, byte)| {
            value | (u32::from(*byte) << (index * 8))
        })
}
