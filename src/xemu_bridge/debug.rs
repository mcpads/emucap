use super::memory::finish_operation;
use super::*;
use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn read_cpu_state(&mut self) -> XemuResult<Value> {
        let response = self.gdb_command("g")?;
        if response.starts_with('E') {
            return Err(XemuBridgeError::Emulator(format!(
                "GDB register read failed: {response}"
            )));
        }
        parse_i386_registers(&response)
    }

    pub(super) fn get_state(&mut self) -> XemuResult<Value> {
        let marker = self.freeze_for_operation()?;
        let outcome = self.read_cpu_state();
        let cleanup = self.restore_after_operation(marker);
        let state = finish_operation(outcome, cleanup, "get_state")?;
        Ok(json!({"cpu":"i386", "state":state}))
    }

    pub(super) fn set_breakpoint(&mut self, params: &Value) -> XemuResult<Value> {
        if params.get("pause_on_hit").and_then(Value::as_bool) == Some(false) {
            return Err(XemuBridgeError::Unsupported(
                "xemu GDB breakpoints always pause; pause_on_hit=false is unavailable".into(),
            ));
        }
        if params.get("auto_savestate").and_then(Value::as_bool) == Some(true)
            || params
                .get("snapshot")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
        {
            return Err(XemuBridgeError::Unsupported(
                "Xbox breakpoint snapshots and auto_savestate are unavailable".into(),
            ));
        }
        for key in ["pc_min", "pc_max", "value", "value_mask", "value_len"] {
            if params.get(key).is_some_and(|value| !value.is_null()) {
                return Err(XemuBridgeError::Unsupported(format!(
                    "Xbox breakpoint option {key} is unavailable"
                )));
            }
        }
        let kind = params.get("kind").and_then(Value::as_str).unwrap_or("exec");
        let ztype = match kind {
            "exec" => 0,
            "write" => 2,
            "read" => 3,
            "access" => 4,
            other => {
                return Err(XemuBridgeError::BadParams(format!(
                    "unsupported Xbox breakpoint kind: {other}"
                )))
            }
        };
        let start = params
            .get("start")
            .or_else(|| params.get("address"))
            .and_then(parse_num)
            .ok_or_else(|| XemuBridgeError::BadParams("missing breakpoint start".into()))?;
        let end = optional_num(params, "end")?.unwrap_or(start);
        if end < start {
            return Err(XemuBridgeError::BadParams(
                "breakpoint end must be greater than or equal to start".into(),
            ));
        }
        if kind == "exec" && end != start {
            return Err(XemuBridgeError::BadParams(
                "Xbox exec breakpoints require start==end".into(),
            ));
        }
        let length = end
            .checked_sub(start)
            .and_then(|span| span.checked_add(1))
            .ok_or_else(|| XemuBridgeError::BadParams("breakpoint range overflow".into()))?;
        if kind != "exec" && length > 8 {
            return Err(XemuBridgeError::BadParams(
                "x86 GDB watchpoints support at most 8 contiguous bytes".into(),
            ));
        }
        let mut routed = params.clone();
        routed["address"] = json!(start);
        let (memory_type, absolute, _) = region_address(&routed, length)?;
        if self.breakpoints.values().any(|breakpoint| {
            breakpoint.ztype == ztype
                && breakpoint.absolute == absolute
                && breakpoint.length == length
        }) {
            return Err(XemuBridgeError::BadParams(
                "an equivalent Xbox breakpoint is already registered".into(),
            ));
        }
        let marker = self.freeze_for_operation()?;
        let outcome = self.gdb_command(&format!("Z{ztype},{absolute:x},{length:x}"));
        let cleanup = self.restore_after_operation(marker);
        let response = finish_operation(outcome, cleanup, "set_breakpoint")?;
        if response != "OK" {
            return Err(XemuBridgeError::Emulator(format!(
                "GDB breakpoint set failed: {response}"
            )));
        }
        let id = self.next_breakpoint_id;
        self.next_breakpoint_id = id
            .checked_add(1)
            .ok_or_else(|| XemuBridgeError::Emulator("breakpoint id space exhausted".into()))?;
        self.breakpoints.insert(
            id,
            XemuBreakpoint {
                kind: kind.into(),
                memory_type: memory_type.clone(),
                start,
                end,
                absolute,
                length,
                ztype,
            },
        );
        Ok(json!({
            "id":id, "kind":kind, "memory_type":memory_type,
            "start":start, "end":end, "pause_on_hit":true
        }))
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> XemuResult<Value> {
        let id = required_num(params, "id")?;
        let breakpoint =
            self.breakpoints.get(&id).cloned().ok_or_else(|| {
                XemuBridgeError::BadParams(format!("unknown breakpoint id: {id}"))
            })?;
        let marker = self.freeze_for_operation()?;
        let outcome = self.gdb_command(&format!(
            "z{},{:x},{:x}",
            breakpoint.ztype, breakpoint.absolute, breakpoint.length
        ));
        let cleanup = self.restore_after_operation(marker);
        let response = finish_operation(outcome, cleanup, "clear_breakpoint")?;
        if response != "OK" && response != "E00" {
            return Err(XemuBridgeError::Emulator(format!(
                "GDB breakpoint clear failed: {response}"
            )));
        }
        self.breakpoints.remove(&id);
        Ok(json!({"cleared":id}))
    }

    pub(super) fn list_breakpoints(&self) -> XemuResult<Value> {
        let rows = self
            .breakpoints
            .iter()
            .map(|(id, breakpoint)| {
                json!({
                    "id":id, "kind":breakpoint.kind, "memory_type":breakpoint.memory_type,
                    "start":breakpoint.start, "end":breakpoint.end, "pause_on_hit":true
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"breakpoints":rows}))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> XemuResult<Value> {
        let mut cleared = Vec::new();
        for id in self.breakpoints.keys().copied().collect::<Vec<_>>() {
            self.clear_breakpoint(&json!({"id":id}))?;
            cleared.push(id);
        }
        Ok(json!({"cleared":cleared}))
    }

    pub(super) fn poll_events(&mut self, params: &Value) -> XemuResult<Value> {
        let filter = optional_num(params, "breakpoint_id")?;
        self.drain_gdb_stops(true)?;
        if self.events.is_empty() {
            let machine = self.machine_status()?;
            self.capture_debug_stop(&machine)?;
        }
        let mut pending = std::mem::take(&mut self.events);
        let state = if pending.is_empty() {
            None
        } else {
            match self.read_cpu_state() {
                Ok(state) => Some(state),
                Err(error) => {
                    self.events.append(&mut pending);
                    return Err(error);
                }
            }
        };
        for event in &mut pending {
            if event.get("pc").is_none() {
                if let Some(state) = &state {
                    if let Some(object) = event.as_object_mut() {
                        object.insert("pc".into(), state["cpu.eip"].clone());
                        object.insert("regs".into(), state.clone());
                    }
                }
            }
            let pc = event.get("pc").and_then(Value::as_u64);
            let watch = event.get("watch_address").and_then(Value::as_u64);
            if let Some((id, breakpoint)) = self.breakpoints.iter().find(|(_, breakpoint)| {
                if breakpoint.kind == "exec" {
                    pc == Some(breakpoint.absolute)
                } else {
                    watch.is_some_and(|address| {
                        address >= breakpoint.absolute
                            && address < breakpoint.absolute + breakpoint.length
                    })
                }
            }) {
                if let Some(object) = event.as_object_mut() {
                    object.insert("type".into(), json!("breakpoint_hit"));
                    object.insert("id".into(), json!(id));
                    object.insert("breakpoint_id".into(), json!(id));
                    object.insert("kind".into(), json!(breakpoint.kind));
                    object.insert("address".into(), json!(breakpoint.start));
                    object.insert("memory_type".into(), json!(breakpoint.memory_type));
                }
            }
        }
        let mut returned = Vec::new();
        for event in pending {
            if filter.is_none() || event.get("breakpoint_id").and_then(Value::as_u64) == filter {
                returned.push(event);
            } else {
                self.events.push(event);
            }
        }
        Ok(json!({"events":returned, "dropped":0}))
    }

    pub(super) fn disassemble(&mut self, params: &Value) -> XemuResult<Value> {
        let count = optional_num(params, "count")?.unwrap_or(8).clamp(1, 256);
        let start = params
            .get("address")
            .or_else(|| params.get("start"))
            .and_then(parse_num)
            .ok_or_else(|| XemuBridgeError::BadParams("missing disassembly address".into()))?;
        let mut routed = params.clone();
        routed["address"] = json!(start);
        if routed.get("memory_type").is_none() {
            routed["memory_type"] = json!("cpu");
        }
        let requested_byte_budget = count
            .checked_mul(15)
            .ok_or_else(|| XemuBridgeError::BadParams("disassembly byte budget overflow".into()))?;
        // Validate that the first byte is addressable, then cap the decoder's lookahead at the
        // selected address-space boundary. An x86 instruction beginning near 0xffff_ffff (most
        // notably the reset vector at 0xffff_fff0) remains valid input even though the usual
        // count*15 worst-case budget would extend beyond the 32-bit CPU view.
        let (memory_type, absolute, offset) = region_address(&routed, 1)?;
        let region_size = if memory_type == "main" {
            XBOX_RAM_SIZE
        } else {
            0x1_0000_0000
        };
        let byte_budget = requested_byte_budget.min(region_size - offset);
        let marker = self.freeze_for_operation()?;
        let outcome = self.read_routed_bytes(&memory_type, absolute, byte_budget as usize);
        let cleanup = self.restore_after_operation(marker);
        let bytes = finish_operation(outcome, cleanup, "disassemble")?;
        let mut decoder = Decoder::with_ip(32, &bytes, absolute, DecoderOptions::NONE);
        let mut formatter = IntelFormatter::new();
        let mut instructions = Vec::new();
        while decoder.can_decode() && instructions.len() < count as usize {
            let offset = decoder.position();
            let instruction = decoder.decode();
            let end = decoder.position();
            let mut text = String::new();
            formatter.format(&instruction, &mut text);
            instructions.push(json!({
                "addr":instruction.ip(),
                "bytes":hex::encode(&bytes[offset..end]),
                "text":text,
            }));
        }
        if instructions.is_empty() {
            return Err(XemuBridgeError::Emulator(
                "xemu disassembler returned no instructions".into(),
            ));
        }
        Ok(json!({"cpu":"i386", "memory_type":memory_type, "instructions":instructions}))
    }

    pub(super) fn call_stack(&mut self) -> XemuResult<Value> {
        let marker = self.freeze_for_operation()?;
        let outcome = (|| {
            let state = self.read_cpu_state()?;
            let value = |name: &str| state.get(name).and_then(Value::as_u64).unwrap_or(0);
            let mut ebp = value("cpu.ebp");
            let esp = value("cpu.esp");
            let mut frames = vec![json!({"pc":value("cpu.eip"), "kind":"pc"})];
            let mut truncated = false;
            for _ in 0..64 {
                if ebp == 0 || ebp < esp || ebp > u32::MAX as u64 - 8 {
                    break;
                }
                let bytes = match self.read_abs_bytes(ebp, 8) {
                    Ok(bytes) => bytes,
                    Err(_) => break,
                };
                let next = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as u64;
                let return_address = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as u64;
                if return_address == 0 {
                    break;
                }
                frames.push(json!({
                    "pc":return_address, "kind":"ebp-walk", "frame_pointer":ebp
                }));
                if next <= ebp || next - ebp > 0x0100_0000 {
                    break;
                }
                ebp = next;
                if frames.len() == 65 {
                    truncated = true;
                }
            }
            Ok(json!({
                "cpu":"i386", "frames":frames, "method":"x86-ebp-chain",
                "authority":"best_effort", "truncated":truncated,
                "note":"Optimized code may omit or repurpose EBP; the walk stops at the first invalid frame link."
            }))
        })();
        let cleanup = self.restore_after_operation(marker);
        finish_operation(outcome, cleanup, "call_stack")
    }
}
