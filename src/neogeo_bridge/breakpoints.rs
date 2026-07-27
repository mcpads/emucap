use super::*;

const MAX_BREAKPOINTS: usize = 128;
const MAX_SNAPSHOTS: usize = 16;
const MAX_SNAPSHOT_BYTES: u64 = 0x4000;
const MAX_DASM_COUNT: u64 = 64;

pub(super) fn breakpoint_kinds() -> Value {
    json!([
        {
            "kind": "exec",
            "range_unit": "address",
            "range_mode": "exact",
            "memory_type_used": false,
            "pause_on_hit": [true],
            "snapshot": true,
            "snapshot_timing": "pre_instruction",
        },
        {
            "kind": "read",
            "range_unit": "address",
            "range_mode": "inclusive",
            "memory_type_used": true,
            "pause_on_hit": [true],
            "snapshot": true,
            "snapshot_timing": "backend_stop",
        },
        {
            "kind": "write",
            "range_unit": "address",
            "range_mode": "inclusive",
            "memory_type_used": true,
            "pause_on_hit": [true],
            "snapshot": true,
            "snapshot_timing": "backend_stop",
        }
    ])
}

impl<G: GdbTransport> NeoGeoBridge<G> {
    pub(super) fn set_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        if self.breakpoints.len() >= MAX_BREAKPOINTS {
            return Err(BridgeError::BadParams(format!(
                "Neo Geo supports at most {MAX_BREAKPOINTS} public breakpoints"
            )));
        }
        reject_unsupported_breakpoint_options(params)?;
        if !params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return Err(BridgeError::BadParams(
                "Neo Geo breakpoints require pause_on_hit=true".into(),
            ));
        }

        let kind = params.get("kind").and_then(Value::as_str).unwrap_or("exec");
        let start = required_num(params, "start")?;
        let end = optional_num(params, "end")?.unwrap_or(start);
        if end < start {
            return Err(BridgeError::BadParams(
                "breakpoint end must be greater than or equal to start".into(),
            ));
        }

        let (absolute_start, backend_kind) = match kind {
            "exec" => {
                if end != start {
                    return Err(BridgeError::BadParams(
                        "Neo Geo exec breakpoints require an exact address".into(),
                    ));
                }
                if params
                    .get("memory_type")
                    .and_then(Value::as_str)
                    .is_some_and(|memory_type| memory_type != "cpu")
                {
                    return Err(BridgeError::BadParams(
                        "Neo Geo exec breakpoints use an absolute 68000 address; memory_type may only be cpu"
                            .into(),
                    ));
                }
                if start > 0x00ff_ffff {
                    return Err(BridgeError::BadParams(
                        "Neo Geo exec address exceeds the 68000 24-bit program space".into(),
                    ));
                }
                (start, "bp")
            }
            "read" | "write" => {
                let memory_type = params
                    .get("memory_type")
                    .and_then(Value::as_str)
                    .unwrap_or("ram");
                if memory_type != "ram" {
                    return Err(BridgeError::BadParams(
                        "Neo Geo read/write breakpoints currently support memory_type=ram only"
                            .into(),
                    ));
                }
                let span = end
                    .checked_sub(start)
                    .and_then(|value| value.checked_add(1))
                    .ok_or_else(|| BridgeError::BadParams("breakpoint range overflow".into()))?;
                (
                    region_address(
                        self.profile,
                        &json!({"memory_type":"ram", "address":start}),
                        span,
                    )?,
                    "wp",
                )
            }
            other => {
                return Err(BridgeError::BadParams(format!(
                    "unsupported Neo Geo breakpoint kind: {other}; supported: exec, read, write"
                )))
            }
        };

        if self.breakpoints.values().any(|breakpoint| {
            breakpoint.kind == kind
                && breakpoint.absolute_start == absolute_start
                && breakpoint.end - breakpoint.start == end - start
        }) {
            return Err(BridgeError::BadParams(
                "an identical Neo Geo breakpoint is already armed".into(),
            ));
        }

        let snapshots = parse_snapshots(self.profile, params.get("snapshot"))?;
        let mut breakpoint = NeoGeoBreakpoint {
            kind: kind.into(),
            start,
            end,
            absolute_start,
            backend_kind: backend_kind.into(),
            backend_id: None,
            snapshots,
            arm_state: NeoGeoArmState::Failed("not armed".into()),
        };
        let (reply_kind, backend_id) = self.arm_native_breakpoint(&breakpoint)?;
        if reply_kind != breakpoint.backend_kind {
            let _ = self.clear_native_breakpoint(&reply_kind, backend_id);
            return Err(BridgeError::Emulator(format!(
                "MAME returned {reply_kind} for a {} breakpoint",
                breakpoint.kind
            )));
        }
        breakpoint.backend_id = Some(backend_id);
        breakpoint.arm_state = NeoGeoArmState::Armed;
        let id = self.next_breakpoint_id;
        self.next_breakpoint_id = self.next_breakpoint_id.saturating_add(1);
        self.breakpoints.insert(id, breakpoint);
        Ok(json!({"id":id, "set":true, "arm_state":"armed"}))
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let id = required_num(params, "id")?;
        let breakpoint = self
            .breakpoints
            .get(&id)
            .cloned()
            .ok_or_else(|| BridgeError::BadParams(format!("unknown breakpoint id: {id}")))?;
        if let Some(backend_id) = breakpoint.backend_id {
            self.clear_native_breakpoint(&breakpoint.backend_kind, backend_id)?;
        }
        self.breakpoints.remove(&id);
        Ok(json!({"cleared":id}))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> BridgeResult<Value> {
        let ids = self.breakpoints.keys().copied().collect::<Vec<_>>();
        let mut cleared = Vec::new();
        for id in ids {
            self.clear_breakpoint(&json!({"id":id}))?;
            cleared.push(id);
        }
        Ok(json!({"cleared":cleared}))
    }

    pub(super) fn list_breakpoints(&self) -> BridgeResult<Value> {
        let breakpoints = self
            .breakpoints
            .iter()
            .map(|(id, breakpoint)| {
                let (arm_state, arm_error) = match &breakpoint.arm_state {
                    NeoGeoArmState::Armed => ("armed", None),
                    NeoGeoArmState::Failed(error) => ("failed", Some(error.as_str())),
                };
                json!({
                    "id": id,
                    "kind": breakpoint.kind,
                    "start": breakpoint.start,
                    "end": breakpoint.end,
                    "backend_id": breakpoint.backend_id,
                    "arm_state": arm_state,
                    "arm_error": arm_error,
                    "pause_on_hit": true,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"breakpoints":breakpoints}))
    }

    pub(super) fn poll_events(&mut self, params: &Value) -> BridgeResult<Value> {
        self.drain_breakpoint_packets()?;
        let filter = optional_num(params, "breakpoint_id")?;
        let mut returned = Vec::new();
        let mut remaining = VecDeque::new();
        while let Some(event) = self.events.pop_front() {
            if filter.is_none()
                || event.get("id").and_then(Value::as_u64) == filter
                || event.get("breakpoint_id").and_then(Value::as_u64) == filter
            {
                returned.push(event);
            } else {
                remaining.push_back(event);
            }
        }
        self.events = remaining;
        Ok(json!({"events":returned, "dropped":0}))
    }

    pub(super) fn drain_breakpoint_packets(&mut self) -> BridgeResult<()> {
        for _ in 0..256 {
            let Some(packet) = self.gdb.recv_nonblocking()? else {
                break;
            };
            if is_breakpoint_stop(&packet) {
                self.record_breakpoint_hit(packet)?;
            }
        }
        Ok(())
    }

    pub(super) fn record_breakpoint_hit(&mut self, packet: String) -> BridgeResult<Value> {
        self.frozen = true;
        let parsed = parse_breakpoint_stop(&packet).ok_or_else(|| {
            BridgeError::Emulator(format!("invalid Neo Geo breakpoint stop packet: {packet}"))
        })?;
        let public_id = self
            .breakpoints
            .iter()
            .find_map(|(id, breakpoint)| {
                (breakpoint.backend_id == Some(parsed.backend_id) && breakpoint.kind == parsed.kind)
                    .then_some(*id)
            })
            .ok_or_else(|| {
                BridgeError::Emulator(format!(
                    "Neo Geo stop references unknown native {} id {}",
                    parsed.kind, parsed.backend_id
                ))
            })?;

        let snapshots = self
            .breakpoints
            .get(&public_id)
            .expect("matched breakpoint exists")
            .snapshots
            .clone();
        let Some(hit_seq) = parsed.hit_seq else {
            if let Some(breakpoint) = self.breakpoints.get_mut(&public_id) {
                breakpoint.backend_id = None;
                breakpoint.arm_state =
                    NeoGeoArmState::Failed("stop packet omitted callback sequence".into());
            }
            return Err(BridgeError::Emulator(format!(
                "invalid Neo Geo breakpoint stop packet: {packet}"
            )));
        };
        if hit_seq <= self.last_hit_seq {
            if let Some(breakpoint) = self.breakpoints.get_mut(&public_id) {
                breakpoint.backend_id = None;
                breakpoint.arm_state = NeoGeoArmState::Failed(format!(
                    "callback sequence did not advance from {} to {hit_seq}",
                    self.last_hit_seq
                ));
            }
            return Err(BridgeError::Emulator(format!(
                "Neo Geo breakpoint hit sequence did not advance: last={}, received={}",
                self.last_hit_seq, hit_seq
            )));
        }
        self.last_hit_seq = hit_seq;
        if let Some(breakpoint) = self.breakpoints.get_mut(&public_id) {
            breakpoint.backend_id = None;
        }
        let mut event = json!({
            "type":"breakpoint_hit",
            "id":public_id,
            "breakpoint_id":public_id,
            "backend_id":parsed.backend_id,
            "kind":parsed.kind,
            "address":parsed.address,
            "hit_seq":hit_seq,
            "raw":packet,
            "regs":parsed.regs,
            "snapshot_timing": if parsed.kind == "exec" { "pre_instruction" } else { "backend_stop" },
        });
        if let Some(pc) = event
            .get("regs")
            .and_then(|registers| registers.get("pc"))
            .and_then(Value::as_u64)
        {
            event
                .as_object_mut()
                .expect("event object")
                .insert("pc".into(), json!(pc));
        }

        if !snapshots.is_empty() {
            match self.capture_breakpoint_snapshots(&snapshots) {
                Ok(values) => {
                    event
                        .as_object_mut()
                        .expect("event object")
                        .insert("snapshot".into(), Value::Array(values));
                }
                Err(error) => {
                    event
                        .as_object_mut()
                        .expect("event object")
                        .insert("snapshot_error".into(), json!(error.to_string()));
                }
            }
        }

        match self.rearm_public_breakpoint(public_id) {
            Ok(backend_id) => {
                let object = event.as_object_mut().expect("event object");
                object.insert("rearmed".into(), json!(true));
                object.insert("backend_id_after".into(), json!(backend_id));
            }
            Err(error) => {
                let object = event.as_object_mut().expect("event object");
                object.insert("rearmed".into(), json!(false));
                object.insert("rearm_error".into(), json!(error.to_string()));
            }
        }
        self.events.push_back(event.clone());
        Ok(event)
    }

    fn rearm_public_breakpoint(&mut self, id: u64) -> BridgeResult<u64> {
        let breakpoint = self
            .breakpoints
            .get(&id)
            .cloned()
            .ok_or_else(|| BridgeError::BadParams(format!("unknown breakpoint id: {id}")))?;
        let result = self.arm_native_breakpoint(&breakpoint);
        match result {
            Ok((backend_kind, backend_id)) if backend_kind == breakpoint.backend_kind => {
                if let Some(record) = self.breakpoints.get_mut(&id) {
                    record.backend_id = Some(backend_id);
                    record.arm_state = NeoGeoArmState::Armed;
                }
                Ok(backend_id)
            }
            Ok((backend_kind, backend_id)) => {
                let _ = self.clear_native_breakpoint(&backend_kind, backend_id);
                let message = format!(
                    "MAME returned {backend_kind} while rearming {}",
                    breakpoint.kind
                );
                if let Some(record) = self.breakpoints.get_mut(&id) {
                    record.arm_state = NeoGeoArmState::Failed(message.clone());
                }
                Err(BridgeError::Emulator(message))
            }
            Err(error) => {
                if let Some(record) = self.breakpoints.get_mut(&id) {
                    record.arm_state = NeoGeoArmState::Failed(error.to_string());
                }
                Err(error)
            }
        }
    }

    fn arm_native_breakpoint(
        &mut self,
        breakpoint: &NeoGeoBreakpoint,
    ) -> BridgeResult<(String, u64)> {
        let code = match breakpoint.kind.as_str() {
            "exec" => "0",
            "write" => "2",
            "read" => "3",
            _ => {
                return Err(BridgeError::BadParams(format!(
                    "unsupported breakpoint kind: {}",
                    breakpoint.kind
                )))
            }
        };
        let size = breakpoint
            .end
            .checked_sub(breakpoint.start)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| BridgeError::BadParams("breakpoint range overflow".into()))?;
        let spec = format!("{code}|{:x}|{size:x}|1|", breakpoint.absolute_start);
        let response = self.lua_cmd("setpoint", Some(&spec))?;
        parse_arm_reply(&response)
    }

    fn clear_native_breakpoint(&mut self, kind: &str, id: u64) -> BridgeResult<()> {
        let response = self.lua_cmd("clearpoint", Some(&format!("{kind}|{id}")))?;
        if response == "OK" || response == "E00" {
            Ok(())
        } else {
            Err(BridgeError::Emulator(format!(
                "MAME breakpoint clear failed: {response}"
            )))
        }
    }

    fn capture_breakpoint_snapshots(
        &mut self,
        snapshots: &[NeoGeoSnapshot],
    ) -> BridgeResult<Vec<Value>> {
        let mut values = Vec::with_capacity(snapshots.len());
        for snapshot in snapshots {
            let absolute = region_address(
                self.profile,
                &json!({
                    "memory_type":snapshot.memory_type,
                    "address":snapshot.address,
                }),
                snapshot.length,
            )?;
            let raw = self
                .gdb
                .send(&format!("m{absolute:x},{:x}", snapshot.length))?;
            let bytes = hex::decode(raw.trim())
                .map_err(|_| BridgeError::Emulator("invalid MAME snapshot bytes".into()))?;
            if bytes.len() as u64 != snapshot.length {
                return Err(BridgeError::Emulator(format!(
                    "short MAME snapshot: expected {}, got {}",
                    snapshot.length,
                    bytes.len()
                )));
            }
            values.push(json!({
                "memory_type":snapshot.memory_type,
                "address":snapshot.address,
                "length":snapshot.length,
                "hex":hex::encode(bytes),
            }));
        }
        Ok(values)
    }

    pub(super) fn disassemble(&mut self, params: &Value) -> BridgeResult<Value> {
        let address = required_num(params, "address")?;
        if address > 0x00ff_ffff {
            return Err(BridgeError::BadParams(
                "Neo Geo disassemble address exceeds the 68000 24-bit program space".into(),
            ));
        }
        let count = optional_num(params, "count")?.unwrap_or(8);
        if !(1..=MAX_DASM_COUNT).contains(&count) {
            return Err(BridgeError::BadParams(format!(
                "Neo Geo disassemble count must be in 1..={MAX_DASM_COUNT}"
            )));
        }
        let directory = self.adapter_home.join("debug");
        let path = directory.join(format!(
            "dasm-{}-{}.txt",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        let path_text = path.to_str().ok_or_else(|| {
            BridgeError::BadState(format!(
                "Neo Geo disassemble path is not valid UTF-8: {}",
                path.display()
            ))
        })?;
        if path_text.contains(['"', '\r', '\n']) {
            return Err(BridgeError::BadState(
                "Neo Geo disassemble path contains characters unsafe for the MAME debugger".into(),
            ));
        }
        fs::create_dir_all(&directory)?;
        let byte_len = count.saturating_mul(12).max(16);
        let result = (|| {
            let spec = format!("{path_text}|{address:x}|{byte_len:x}");
            let response = self.lua_cmd("dasm", Some(&spec))?;
            if response != "OK" {
                return Err(BridgeError::Emulator(format!(
                    "MAME disassemble failed: {response}"
                )));
            }
            let output_len = fs::metadata(&path)?.len();
            if output_len > MAX_DASM_OUTPUT_BYTES {
                return Err(BridgeError::Emulator(format!(
                    "MAME disassemble output exceeds {MAX_DASM_OUTPUT_BYTES} bytes"
                )));
            }
            let text = fs::read_to_string(&path)?;
            let instructions = parse_dasm_lines(text.lines(), count as usize);
            if instructions.is_empty() {
                return Err(BridgeError::Emulator(
                    "MAME disassemble produced no instructions".into(),
                ));
            }
            Ok(json!({
                "instructions":instructions,
                "artifact_scope": if self.env.launch_id.is_some() {
                    "generation"
                } else {
                    "adapter"
                },
            }))
        })();
        let _ = fs::remove_file(path);
        result
    }
}

#[derive(Debug)]
struct ParsedStop {
    kind: String,
    address: u64,
    backend_id: u64,
    hit_seq: Option<u64>,
    regs: Value,
}

fn reject_unsupported_breakpoint_options(params: &Value) -> BridgeResult<()> {
    for key in [
        "condition",
        "value",
        "value_mask",
        "value_len",
        "pc_min",
        "pc_max",
    ] {
        if params.get(key).is_some() {
            return Err(BridgeError::BadParams(format!(
                "Neo Geo breakpoint option {key} is not supported"
            )));
        }
    }
    Ok(())
}

fn parse_snapshots(
    profile: NeoGeoProfile,
    value: Option<&Value>,
) -> BridgeResult<Vec<NeoGeoSnapshot>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| BridgeError::BadParams("snapshot must be a list".into()))?;
    if entries.len() > MAX_SNAPSHOTS {
        return Err(BridgeError::BadParams(format!(
            "snapshot accepts at most {MAX_SNAPSHOTS} ranges"
        )));
    }
    let mut snapshots = Vec::with_capacity(entries.len());
    let mut total = 0_u64;
    for entry in entries {
        let raw = entry.as_str().ok_or_else(|| {
            BridgeError::BadParams("snapshot entries must be memory_type:address:length".into())
        })?;
        let parts = raw.split(':').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(BridgeError::BadParams(format!(
                "invalid snapshot entry: {raw}"
            )));
        }
        let memory_type = parts[0];
        let address = parse_snapshot_num(parts[1])
            .ok_or_else(|| BridgeError::BadParams(format!("invalid snapshot address: {raw}")))?;
        let length = parse_snapshot_num(parts[2])
            .filter(|length| *length > 0)
            .ok_or_else(|| BridgeError::BadParams(format!("invalid snapshot length: {raw}")))?;
        total = total
            .checked_add(length)
            .ok_or_else(|| BridgeError::BadParams("snapshot byte total overflow".into()))?;
        if total > MAX_SNAPSHOT_BYTES {
            return Err(BridgeError::BadParams(format!(
                "snapshot byte total exceeds {MAX_SNAPSHOT_BYTES}"
            )));
        }
        region_address(
            profile,
            &json!({"memory_type":memory_type, "address":address}),
            length,
        )?;
        snapshots.push(NeoGeoSnapshot {
            memory_type: memory_type.into(),
            address,
            length,
        });
    }
    Ok(snapshots)
}

fn parse_snapshot_num(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix('$')) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        raw.parse().ok()
    }
}

fn parse_arm_reply(response: &str) -> BridgeResult<(String, u64)> {
    let (kind, id) = response.split_once(':').ok_or_else(|| {
        BridgeError::Emulator(format!("invalid MAME breakpoint response: {response}"))
    })?;
    let backend_kind = match kind {
        "BP" => "bp",
        "WP" => "wp",
        _ => {
            return Err(BridgeError::Emulator(format!(
                "invalid MAME breakpoint kind: {response}"
            )))
        }
    };
    let id = id
        .parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("invalid MAME breakpoint id: {response}")))?;
    Ok((backend_kind.into(), id))
}

pub(super) fn is_breakpoint_stop(packet: &str) -> bool {
    packet.starts_with("T05hwbreak:")
        || packet.starts_with("T05watch:")
        || packet.starts_with("T05rwatch:")
}

fn parse_breakpoint_stop(packet: &str) -> Option<ParsedStop> {
    let body = packet.strip_prefix("T05")?;
    let (head, rest) = body.split_once(';').unwrap_or((body, ""));
    let (tag, address) = head.split_once(':')?;
    let kind = match tag {
        "hwbreak" => "exec",
        "watch" => "write",
        "rwatch" => "read",
        _ => return None,
    };
    let address = little_endian_hex(address)?;
    let mut backend_id = None;
    let mut hit_seq = None;
    let mut regs = json!({});
    for field in rest.split(';').filter(|field| !field.is_empty()) {
        let Some((key, value)) = field.split_once(':') else {
            continue;
        };
        match key {
            "idx" => backend_id = value.parse().ok(),
            "seq" => hit_seq = value.parse().ok(),
            "regs" => regs = parse_m68k_registers(value),
            _ => {}
        }
    }
    Some(ParsedStop {
        kind: kind.into(),
        address,
        backend_id: backend_id?,
        hit_seq,
        regs,
    })
}

fn little_endian_hex(raw: &str) -> Option<u64> {
    let bytes = hex::decode(raw).ok()?;
    if bytes.len() > 8 {
        return None;
    }
    let mut padded = [0_u8; 8];
    padded[..bytes.len()].copy_from_slice(&bytes);
    Some(u64::from_le_bytes(padded))
}

fn parse_m68k_registers(raw: &str) -> Value {
    let Ok(bytes) = hex::decode(raw) else {
        return json!({"decode_error":"invalid hex"});
    };
    if bytes.len() != REG_NAMES.len() * 4 {
        return json!({"decode_error":"unexpected register packet length", "byte_len":bytes.len()});
    }
    let mut registers = serde_json::Map::new();
    for (index, name) in REG_NAMES.iter().enumerate() {
        let offset = index * 4;
        let value = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("register has four bytes"),
        );
        registers.insert((*name).into(), json!(value));
    }
    Value::Object(registers)
}

fn parse_dasm_lines<'a>(lines: impl Iterator<Item = &'a str>, count: usize) -> Vec<Value> {
    let mut instructions = Vec::new();
    for line in lines {
        if instructions.len() >= count {
            break;
        }
        let raw = line.trim();
        let Some((address, rest)) = raw.split_once(':') else {
            continue;
        };
        let Ok(address) = u64::from_str_radix(address.trim(), 16) else {
            continue;
        };
        let parts = rest.split_whitespace().collect::<Vec<_>>();
        let byte_count = parts
            .iter()
            .take_while(|part| part.len() == 2 && part.as_bytes().iter().all(u8::is_ascii_hexdigit))
            .count();
        let bytes = parts[..byte_count].join("").to_ascii_lowercase();
        let text = parts[byte_count..].join(" ");
        instructions.push(json!({
            "addr":address,
            "bytes":bytes,
            "length":bytes.len() / 2,
            "text":text,
        }));
    }
    instructions
}
