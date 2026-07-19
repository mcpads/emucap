use super::*;

const MAX_BREAKPOINT_SPAN: u64 = 0x1_0000;
const BREAKPOINT_REGISTER_COUNT: usize = 34;
const MAX_EVENT_COUNT: usize = 256;
const MAX_STACK_DEPTH: usize = 256;

#[derive(Clone)]
pub(super) struct Pcsx2Breakpoint {
    kind: &'static str,
    start: u32,
    end: u32,
}

impl Pcsx2Breakpoint {
    fn native_kind(&self) -> u32 {
        match self.kind {
            "exec" => 0,
            "read" => 1,
            "write" => 2,
            _ => unreachable!("validated breakpoint kind"),
        }
    }

    fn command_body(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(12);
        body.extend_from_slice(&self.native_kind().to_le_bytes());
        body.extend_from_slice(&self.start.to_le_bytes());
        body.extend_from_slice(&self.end.to_le_bytes());
        body
    }

    fn overlaps(&self, address: u32, length: u32) -> bool {
        let access_end = address.saturating_add(length.saturating_sub(1));
        address <= self.end && self.start <= access_end
    }

    fn matches_exec(&self, address: u32) -> bool {
        standardize_ee_debug_address(self.start) == standardize_ee_debug_address(address)
    }
}

impl<T: PineTransport> Pcsx2Bridge<T> {
    pub(super) fn set_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let kind = match params.get("kind").and_then(Value::as_str).unwrap_or("exec") {
            "exec" => "exec",
            "read" => "read",
            "write" => "write",
            other => {
                return Err(Pcsx2BridgeError::BadParams(format!(
                    "unsupported PS2 breakpoint kind `{other}`; supported: exec, read, write"
                )))
            }
        };
        if params.get("pause_on_hit").and_then(Value::as_bool) == Some(false) {
            return Err(Pcsx2BridgeError::BadParams(
                "PCSX2 breakpoints always pause; pause_on_hit=false is unsupported".into(),
            ));
        }
        if params.get("auto_savestate").and_then(Value::as_bool) == Some(true) {
            return Err(Pcsx2BridgeError::BadParams(
                "PCSX2 breakpoint auto_savestate is unsupported".into(),
            ));
        }
        if params
            .get("snapshot")
            .and_then(Value::as_array)
            .is_some_and(|values| !values.is_empty())
        {
            return Err(Pcsx2BridgeError::BadParams(
                "PCSX2 breakpoint snapshots are unsupported; poll_events already captures EE registers"
                    .into(),
            ));
        }
        for option in ["pc_min", "pc_max", "value", "value_mask", "value_len"] {
            if params.get(option).is_some_and(|value| !value.is_null()) {
                return Err(Pcsx2BridgeError::BadParams(format!(
                    "PCSX2 breakpoint option `{option}` is unsupported"
                )));
            }
        }
        match params.get("memory_type").and_then(Value::as_str) {
            Some("ee") => {}
            Some(other) => {
                return Err(Pcsx2BridgeError::BadParams(format!(
                    "unsupported memory_type `{other}`; valid for PS2 breakpoints: ee"
                )))
            }
            None => {
                return Err(Pcsx2BridgeError::BadParams(
                    "memory_type is required for PS2 breakpoints".into(),
                ))
            }
        }

        let raw_start = required_num_alias(params, &["address", "start"])?;
        let raw_end = optional_num(params, "end")?.unwrap_or(raw_start);
        if raw_end < raw_start {
            return Err(Pcsx2BridgeError::BadParams(
                "breakpoint end must be greater than or equal to start".into(),
            ));
        }
        if kind == "exec" && raw_end != raw_start {
            return Err(Pcsx2BridgeError::BadParams(
                "PCSX2 supports exact exec breakpoints only (start must equal end)".into(),
            ));
        }

        let (start, end) = if kind == "exec" {
            (
                u32::try_from(raw_start).map_err(|_| {
                    Pcsx2BridgeError::BadParams(
                        "exec breakpoint address exceeds the EE address width".into(),
                    )
                })?,
                u32::try_from(raw_end).expect("same validated exec address"),
            )
        } else {
            let length = raw_end
                .checked_sub(raw_start)
                .and_then(|span| span.checked_add(1))
                .ok_or_else(|| Pcsx2BridgeError::BadParams("breakpoint range overflow".into()))?;
            if length > MAX_BREAKPOINT_SPAN {
                return Err(Pcsx2BridgeError::BadParams(format!(
                    "PS2 read/write breakpoint span {length:#x} exceeds {MAX_BREAKPOINT_SPAN:#x}"
                )));
            }
            let end_exclusive = raw_end
                .checked_add(1)
                .ok_or_else(|| Pcsx2BridgeError::BadParams("breakpoint range overflow".into()))?;
            if end_exclusive > PCSX2_EE_RAM_SIZE {
                return Err(Pcsx2BridgeError::BadParams(format!(
                    "EE breakpoint range [{raw_start:#x}, {end_exclusive:#x}) exceeds [0, {PCSX2_EE_RAM_SIZE:#x})"
                )));
            }
            (
                u32::try_from(raw_start).expect("EE RAM address fits u32"),
                u32::try_from(raw_end).expect("EE RAM address fits u32"),
            )
        };

        if self.breakpoints.values().any(|breakpoint| {
            breakpoint.kind == kind
                && if kind == "exec" {
                    breakpoint.matches_exec(start)
                } else {
                    breakpoint.start == start && breakpoint.end == end
                }
        }) {
            return Err(Pcsx2BridgeError::BadParams(
                "an identical PCSX2 breakpoint is already armed".into(),
            ));
        }

        let id = self.next_breakpoint_id;
        let next_id = id
            .checked_add(1)
            .ok_or_else(|| Pcsx2BridgeError::Protocol("breakpoint id space exhausted".into()))?;
        let breakpoint = Pcsx2Breakpoint { kind, start, end };
        self.command(MSG_EMUCAP_SET_BREAKPOINT, &breakpoint.command_body())?;
        self.next_breakpoint_id = next_id;
        self.breakpoints.insert(id, breakpoint);
        Ok(json!({
            "id": id,
            "kind": kind,
            "memory_type": "ee",
            "start": start,
            "end": end,
            "pause_on_hit": true,
        }))
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let id = required_num(params, "id")?;
        let breakpoint =
            self.breakpoints.get(&id).cloned().ok_or_else(|| {
                Pcsx2BridgeError::BadParams(format!("unknown breakpoint id: {id}"))
            })?;
        self.command(MSG_EMUCAP_CLEAR_BREAKPOINT, &breakpoint.command_body())?;
        self.breakpoints.remove(&id);
        Ok(json!({ "cleared": id }))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> BridgeResult<Value> {
        let mut cleared = Vec::new();
        for id in self.breakpoints.keys().copied().collect::<Vec<_>>() {
            self.clear_breakpoint(&json!({ "id": id }))?;
            cleared.push(id);
        }
        Ok(json!({ "cleared": cleared }))
    }

    pub(super) fn list_breakpoints(&self) -> BridgeResult<Value> {
        let breakpoints = self
            .breakpoints
            .iter()
            .map(|(&id, breakpoint)| {
                json!({
                    "id": id,
                    "kind": breakpoint.kind,
                    "memory_type": "ee",
                    "start": breakpoint.start,
                    "end": breakpoint.end,
                    "pause_on_hit": true,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({ "breakpoints": breakpoints }))
    }

    pub(super) fn poll_events(&mut self) -> BridgeResult<Value> {
        let payload = self.command(MSG_EMUCAP_POLL_EVENTS, &[])?;
        let mut cursor = SliceCursor::new(&payload);
        let count = cursor.u32()? as usize;
        let dropped = cursor.u32()? as u64;
        if count > MAX_EVENT_COUNT {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "PCSX2 returned {count} breakpoint events; maximum is {MAX_EVENT_COUNT}"
            )));
        }

        let names = [
            "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5",
            "t6", "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1",
            "gp", "sp", "fp", "ra", "hi", "lo",
        ];
        let mut events = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = match cursor.u32()? {
                0 => "exec",
                1 => "read",
                2 => "write",
                value => {
                    return Err(Pcsx2BridgeError::Protocol(format!(
                        "unknown PCSX2 breakpoint event kind: {value}"
                    )))
                }
            };
            let pc = cursor.u32()?;
            let address = cursor.u32()?;
            let length = cursor.u32()?;
            let mut regs = serde_json::Map::new();
            regs.insert("cpu.pc".into(), json!(pc));
            for name in names.iter().take(BREAKPOINT_REGISTER_COUNT) {
                regs.insert(format!("cpu.{name}"), json!(cursor.u64()?));
            }

            let matched = self.breakpoints.iter().find(|(_, breakpoint)| {
                breakpoint.kind == kind
                    && if kind == "exec" {
                        breakpoint.matches_exec(address)
                    } else {
                        breakpoint.overlaps(address, length)
                    }
            });
            let mut event = json!({
                "type": "breakpoint_hit",
                "kind": kind,
                "pc": pc,
                "address": address,
                "length": length,
                "regs": Value::Object(regs),
            });
            if let Some((&id, _)) = matched {
                event["id"] = json!(id);
                event["breakpoint_id"] = json!(id);
            }
            events.push(event);
        }
        if !cursor.is_empty() {
            return Err(Pcsx2BridgeError::Protocol(
                "breakpoint event reply has trailing bytes".into(),
            ));
        }
        Ok(json!({ "events": events, "dropped": dropped }))
    }

    pub(super) fn call_stack(&mut self) -> BridgeResult<Value> {
        self.require_frozen("call_stack")?;
        let payload = self.command(MSG_EMUCAP_CALL_STACK, &[])?;
        let mut cursor = SliceCursor::new(&payload);
        let valid = cursor.u32()? != 0;
        let count = cursor.u32()? as usize;
        if count > MAX_STACK_DEPTH {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "PCSX2 returned stack depth {count}; maximum is {MAX_STACK_DEPTH}"
            )));
        }
        let mut frames = Vec::with_capacity(count);
        for _ in 0..count {
            frames.push(json!({
                "pc": cursor.u32()?,
                "entry": cursor.u32()?,
                "sp": cursor.u32()?,
                "stack_size": cursor.i32()?,
            }));
        }
        if !cursor.is_empty() {
            return Err(Pcsx2BridgeError::Protocol(
                "call stack reply has trailing bytes".into(),
            ));
        }
        Ok(json!({ "call_stack": frames, "depth": count, "valid": valid }))
    }

    pub(super) fn reset(&mut self) -> BridgeResult<Value> {
        let post_reset_pc = self.read_u32_command(MSG_EMUCAP_RESET, &[])?;
        Ok(json!({
            "status": "completed",
            "state": "frozen",
            "post_reset_pc": post_reset_pc,
        }))
    }
}

fn standardize_ee_debug_address(mut address: u32) -> u32 {
    if address >= 0xffff_8000 {
        return address;
    }
    if (0xbfc0_0000..=0xbfff_ffff).contains(&address) {
        address &= 0x1fff_ffff;
    }
    address &= 0x7fff_ffff;
    if matches!(address >> 28, 2 | 3) {
        address &= !(0xf << 28);
    }
    address
}
