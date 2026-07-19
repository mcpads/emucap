use super::*;

pub(super) fn cleanup_timed_override_error(
    primary: NdsBridgeError,
    cleanup: NdsResult<()>,
) -> NdsBridgeError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup_err) => NdsBridgeError::Emulator(format!(
            "{primary}; transient input cleanup also failed: {cleanup_err}"
        )),
    }
}

pub(super) fn override_status_json(remaining: Option<i64>) -> Value {
    match remaining {
        None => json!({ "observable": false }),
        Some(0) => json!({
            "observable": true,
            "engaged": false,
            "mode": "native",
            "remaining_frames": 0,
        }),
        Some(-1) => json!({
            "observable": true,
            "engaged": true,
            "mode": "persistent",
        }),
        Some(remaining) => json!({
            "observable": true,
            "engaged": true,
            "mode": "timed",
            "remaining_frames": remaining,
        }),
    }
}

pub(super) fn error_kind(err: &NdsBridgeError) -> &'static str {
    match err {
        NdsBridgeError::BadParams(_) => "bad_params",
        NdsBridgeError::UnknownMethod(_) => "unknown_method",
        NdsBridgeError::Unsupported(_) => "unsupported",
        NdsBridgeError::Emulator(_) | NdsBridgeError::Gdb(GdbError::Emulator(_)) => {
            "emulator_error"
        }
        NdsBridgeError::Io(_)
        | NdsBridgeError::Json(_)
        | NdsBridgeError::Gdb(GdbError::Io(_) | GdbError::Poisoned) => "bridge_error",
    }
}

pub(super) fn memory_region(name: &str) -> Option<&'static NdsRegion> {
    MEMORY_REGIONS.iter().find(|r| r.name == name)
}

/// Resolve a request's `(cpu, absolute_address, region)` from `memory_type` + `address`/`start`.
/// The routing CPU is the memory_type's default unless an explicit `cpu` param overrides it; the
/// resolved region is returned so callers can apply its bounds and snapshot policy.
pub(super) fn route(params: &Value, len: u64) -> NdsResult<(CpuId, u64, &'static NdsRegion)> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("arm9");
    let region = memory_region(memory_type).ok_or_else(|| {
        NdsBridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
    })?;
    let cpu = match params.get("cpu").and_then(Value::as_str) {
        None => region.cpu,
        Some("arm9") => CpuId::Arm9,
        Some("arm7") => CpuId::Arm7,
        Some(other) => {
            return Err(NdsBridgeError::BadParams(format!(
                "unsupported cpu: {other}; valid: arm9, arm7"
            )))
        }
    };
    let offset = region_offset(params)?;
    // [offset, offset+len)이 선택된 region 안이어야 한다. main(4 MB) 같은 유한 region 밖 offset을 절대주소로
    // 감싸 보내면(wrapping) 무관한 DS 버스를 읽고/쓰게 되므로 거부한다 — arm9/arm7은 size=4 GB(전체 버스)라
    // 유효한 32비트 주소만 통과한다. read/write/BP가 모두 이 경로를 탄다.
    if !matches!(offset.checked_add(len.max(1)), Some(end) if end <= region.size) {
        return Err(NdsBridgeError::BadParams(format!(
            "{memory_type} access out of range: offset {offset:#x}+{len:#x} exceeds region size {size:#x}",
            size = region.size
        )));
    }
    let addr = region.base.checked_add(offset).ok_or_else(|| {
        NdsBridgeError::BadParams(format!(
            "{memory_type} address overflow at offset {offset:#x}"
        ))
    })?;
    Ok((cpu, addr, region))
}

pub(super) fn region_offset(params: &Value) -> NdsResult<u64> {
    if let Some(value) = optional_num(params, "address")? {
        return Ok(value);
    }
    if let Some(value) = optional_num(params, "start")? {
        return Ok(value);
    }
    Err(NdsBridgeError::BadParams(
        "missing required param: address".into(),
    ))
}

/// Absolute code address for disassemble/call-stack use (a raw PC-style address, no region base
/// added — unlike `read_memory` these consume absolute addresses such as `cpu.pc`).
pub(super) fn absolute_address(params: &Value) -> NdsResult<u64> {
    if let Some(value) = optional_num(params, "address")? {
        return Ok(value);
    }
    if let Some(value) = optional_num(params, "start")? {
        return Ok(value);
    }
    Err(NdsBridgeError::BadParams(
        "missing required param: address".into(),
    ))
}

/// Parse the fork's disassembly block (`<addrhex>|<opcodehex>|<text>` per line) into
/// `[{addr, bytes, text}]`. `bytes` is re-emitted in little-endian in-memory order (the fork
/// prints the opcode as a big-endian value), matching the pc98 adapter's byte convention.
pub(super) fn parse_disasm_rows(text: &str, count: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text.lines() {
        if out.len() >= count {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '|');
        let addr_raw = parts.next().unwrap_or("").trim();
        let op_raw = parts.next().unwrap_or("").trim();
        let insn = parts.next().unwrap_or("").trim();
        let Ok(addr) = u64::from_str_radix(addr_raw, 16) else {
            continue;
        };
        let mut item = serde_json::Map::new();
        item.insert("addr".into(), json!(addr));
        item.insert("text".into(), json!(insn));
        item.insert("bytes".into(), json!(opcode_hex_to_le_bytes(op_raw)));
        out.push(Value::Object(item));
    }
    out
}

/// Convert a big-endian opcode value hex string (as the fork prints, e.g. "e3a00001") to the
/// little-endian in-memory byte order ("0100a0e3"). Odd/invalid input is passed through as-is.
pub(super) fn opcode_hex_to_le_bytes(op_hex: &str) -> String {
    match hex::decode(op_hex) {
        Ok(mut bytes) => {
            bytes.reverse();
            hex::encode(bytes)
        }
        Err(_) => op_hex.to_ascii_lowercase(),
    }
}

/// NDS RAM windows a stack/frame pointer can legitimately live in (main RAM + WRAM). Used to
/// gate the best-effort stack walk's pointer reads away from MMIO.
pub(super) fn nds_in_ram(addr: u64) -> bool {
    (0x0200_0000..0x0240_0000).contains(&addr) || (0x0300_0000..0x0400_0000).contains(&addr)
}

/// Plausible NDS executable regions for a return-address sanity check. The Thumb low bit is
/// masked off. Intentionally lenient (main RAM, ITCM, WRAM, ARM9 BIOS) — a hard reject here
/// would prune legitimate frames, so callers treat this as advisory (`in_code_region`).
pub(super) fn nds_in_code_region(addr: u64) -> bool {
    let a = addr & !1;
    (0x0200_0000..0x0240_0000).contains(&a)      // main RAM
        || (0x0100_0000..0x0200_0000).contains(&a) // ITCM (ARM9)
        || (0x0300_0000..0x0400_0000).contains(&a) // shared + ARM7 WRAM
        || (0xFFFF_0000..0xFFFF_8000).contains(&a) // ARM9 BIOS
}

pub(super) fn cpu_from_params(params: &Value) -> NdsResult<CpuId> {
    match params.get("cpu").and_then(Value::as_str) {
        None | Some("arm9") => Ok(CpuId::Arm9),
        Some("arm7") => Ok(CpuId::Arm7),
        Some(other) => Err(NdsBridgeError::BadParams(format!(
            "unsupported cpu: {other}; valid: arm9, arm7"
        ))),
    }
}

pub(super) fn step_count(params: &Value) -> NdsResult<u64> {
    let count = match optional_num(params, "count")? {
        Some(count) => count,
        None => match optional_num(params, "n")? {
            Some(n) => n,
            None => optional_num(params, "frames")?.unwrap_or(1),
        },
    };
    Ok(count.max(1))
}

pub(super) fn required_num(params: &Value, key: &str) -> NdsResult<u64> {
    let value = params
        .get(key)
        .ok_or_else(|| NdsBridgeError::BadParams(format!("missing required param: {key}")))?;
    parse_num(value)
        .ok_or_else(|| NdsBridgeError::BadParams(format!("invalid numeric param: {key}")))
}

pub(super) fn optional_num(params: &Value, key: &str) -> NdsResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| NdsBridgeError::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

pub(super) fn require_input_port_zero(params: &Value) -> NdsResult<()> {
    let port = optional_num(params, "port")?.unwrap_or(0);
    if port != 0 {
        return Err(NdsBridgeError::BadParams(format!(
            "NDS input supports only controller port 0 (got {port})"
        )));
    }
    Ok(())
}

pub(super) fn parse_num(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => parse_num_str(s),
        _ => None,
    }
}

pub(super) fn parse_num_str(s: &str) -> Option<u64> {
    let raw = s.trim();
    if let Some(hex) = raw.strip_prefix('$') {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<u64>().ok()
    }
}

pub(super) fn required_str<'a>(params: &'a Value, key: &str) -> NdsResult<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| NdsBridgeError::BadParams(format!("missing required param: {key}")))
}

pub(super) fn find_subslice(buf: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > buf.len() {
        return None;
    }
    buf.windows(needle.len()).position(|w| w == needle)
}

pub(super) fn is_stop_packet(resp: &str) -> bool {
    resp.starts_with('S') || resp.starts_with('T')
}

/// `S02` / `T02…` = SIGINT — the pause/interrupt WE injected (via `with_frozen` or `pause`), not an
/// async game event. Distinguished from a breakpoint stop (`S05` = SIGTRAP) so `note_stop` can drop
/// our own pauses instead of flooding the poll_events queue with them.
pub(super) fn is_interrupt_stop(resp: &str) -> bool {
    is_stop_packet(resp) && resp.get(1..3) == Some("02")
}

/// A stray async stop that a base64 reply (screenshot/disasm) reader could mistake for its reply.
/// base64 is `[A-Za-z0-9+/=]` only, and `is_stop_packet` over-matches because a base64 blob can begin
/// with 'S'/'T'. So match only stop shapes that a real base64 reply can never be: an S-stop is exactly
/// "S"+2 hex (a real base64 reply is far longer than 3 chars), and a T-stop carries ';'/':' which
/// base64 lacks. This catches e.g. a stray "S05" that would otherwise base64-decode to a padding error.
pub(super) fn looks_like_stray_stop(resp: &str) -> bool {
    let b = resp.as_bytes();
    (b.len() == 3 && b[0] == b'S' && b[1].is_ascii_hexdigit() && b[2].is_ascii_hexdigit())
        || (b.first() == Some(&b'T') && (resp.contains(';') || resp.contains(':')))
}

/// Commands whose normal RSP reply is itself a stop packet — their stop is a real reply, not a
/// stale async stop, so it must not be demuxed.
pub(super) fn command_expects_stop(payload: &str) -> bool {
    payload == "c"
        || payload == "s"
        || payload == "?"
        || payload.starts_with('C')
        || payload.starts_with('S')
        || payload.starts_with("vCont")
}

pub(super) fn stop_event(stop: &str) -> Value {
    json!({ "type": "stop", "signal": stop.get(1..3).unwrap_or(""), "raw": stop })
}

pub(super) fn set_event_field(event: &mut Value, key: &str, value: Value) {
    if let Some(obj) = event.as_object_mut() {
        obj.insert(key.into(), value);
    }
}

pub(super) fn nds_input_buttons_json() -> Value {
    json!({
        "system": "nds",
        "buttons": NDS_INPUT_BUTTONS,
        "implemented": true,
        "notes": "Injected on the ARM9 connection via the DeSmuME fork's QEmucap,input command. set_input holds until changed; press_buttons holds for N frames while the emulator runs.",
    })
}

/// emucap common NDS button → bit in the 12-bit mask the DeSmuME fork consumes
/// (`QEmucap,input:<hexmask>`). Layout matches the fork's decode in NDSSystem.cpp.
pub(super) fn nds_button_bit(name: &str) -> Option<u16> {
    let bit = match name {
        "a" => 0,
        "b" => 1,
        "select" => 2,
        "start" => 3,
        "right" => 4,
        "left" => 5,
        "up" => 6,
        "down" => 7,
        "r" => 8,
        "l" => 9,
        "x" => 10,
        "y" => 11,
        _ => return None,
    };
    Some(1 << bit)
}

/// Fold a small set of aliases onto the canonical shared button names.
pub(super) fn nds_button_alias(name: &str) -> &str {
    match name {
        "sel" => "select",
        "lb" | "l1" => "l",
        "rb" | "r1" => "r",
        other => other,
    }
}

/// Parse a `buttons` param (list of names) into the fork's 12-bit mask plus the normalized
/// names. An unknown button is rejected rather than silently dropped.
pub(super) fn buttons_to_mask(raw: Option<&Value>) -> NdsResult<(u16, Vec<String>)> {
    let Some(raw) = raw else {
        return Ok((0, Vec::new()));
    };
    let Some(items) = raw.as_array() else {
        return Err(NdsBridgeError::BadParams("buttons must be a list".into()));
    };
    let mut mask = 0u16;
    let mut names = Vec::new();
    for value in items {
        let key = value
            .as_str()
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_else(|| value.to_string().trim_matches('"').to_ascii_lowercase());
        let normalized = nds_button_alias(&key);
        match nds_button_bit(normalized) {
            Some(bit) => {
                mask |= bit;
                names.push(normalized.to_string());
            }
            None => {
                return Err(NdsBridgeError::BadParams(format!(
                    "unsupported nds button: {key}; valid: {}",
                    NDS_INPUT_BUTTONS.join(", ")
                )))
            }
        }
    }
    Ok((mask, names))
}

/// Decode DeSmuME's standard ARM `g` packet. Layout (168 bytes): words 0..15 = r0..r15
/// (r13=sp, r14=lr, r15=pc), then FPA f0-f7 (96B) + FPS (4B) ignored, then CPSR as the last
/// 4 bytes. Each 32-bit word is little-endian byte order. A compact 68-byte layout
/// (r0..r15 + CPSR, no FPA) is also accepted.
pub(super) fn state_from_arm_regs_hex(resp: &str) -> Value {
    let mut state = serde_json::Map::new();
    for i in 0..16 {
        let start = i * 8;
        let end = start + 8;
        if end > resp.len() {
            break;
        }
        if let Some(value) = le_hex_to_u32(&resp[start..end]) {
            state.insert(format!("cpu.r{i}"), json!(value));
        }
    }
    let cpsr = if resp.len() >= 336 {
        le_hex_to_u32(&resp[328..336])
    } else if resp.len() >= 136 {
        le_hex_to_u32(&resp[128..136])
    } else {
        None
    };
    if let Some(cpsr) = cpsr {
        state.insert("cpu.cpsr".into(), json!(cpsr));
    }
    if let Some(pc) = state.get("cpu.r15").cloned() {
        state.insert("cpu.pc".into(), pc);
    }
    if let Some(sp) = state.get("cpu.r13").cloned() {
        state.insert("cpu.sp".into(), sp);
    }
    if let Some(lr) = state.get("cpu.r14").cloned() {
        state.insert("cpu.lr".into(), lr);
    }
    if state.is_empty() {
        state.insert("cpu.raw_register_bytes".into(), json!(resp.len() / 2));
    }
    Value::Object(state)
}

pub(super) fn le_hex_to_u32(hex: &str) -> Option<u32> {
    let bytes = hex::decode(hex).ok()?;
    if bytes.len() != 4 {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(super) fn sha1_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = Sha1::new();
    let mut file = File::open(path)?;
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn absolute_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}
