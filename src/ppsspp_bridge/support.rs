use super::*;

pub(super) fn is_timeout_error(err: &BridgeError) -> bool {
    fn is_timeout_io(e: &std::io::Error) -> bool {
        matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    }
    match err {
        BridgeError::Io(e) => is_timeout_io(e),
        BridgeError::Ws(tungstenite::Error::Io(e)) => is_timeout_io(e),
        _ => false,
    }
}

pub(super) fn capability_notes() -> Value {
    // `planned_methods` discloses every real emucap tool name this bridge doesn't dispatch yet —
    // both concretely planned (`PLANNED_METHODS`) and platform-gapped (`UNSUPPORTED_METHODS`, which
    // resolve to an `unsupported` error rather than `unknown_method`) — so a caller can see the
    // not-yet-here surface without a trial call.
    let mut planned: Vec<&str> = PLANNED_METHODS.to_vec();
    planned.extend_from_slice(UNSUPPORTED_METHODS);
    json!({
        "backend": "ppsspp-debugger-ws",
        "rust_bridge": true,
        "implemented_methods": METHODS,
        "planned_methods": planned,
        "screenshot": true,
        "input": true,
        "frame_step": true,
        "step_units": ["frames", "instructions"],
        "breakpoints": true,
        "watch_register": false,
        "trace": false,
        "state_restore": true,
        "disassemble": true,
        "call_stack": true,
    })
}

pub(super) fn error_kind(err: &BridgeError) -> &'static str {
    match err {
        BridgeError::BadParams(_) => "bad_params",
        BridgeError::BadState(_) => "bad_state",
        BridgeError::UnknownMethod(_) => "unknown_method",
        BridgeError::Unsupported(_) => "unsupported",
        BridgeError::Emulator(_) => "emulator_error",
        BridgeError::Io(_) | BridgeError::Json(_) | BridgeError::Ws(_) => "bridge_error",
    }
}

/// Mark a `poll_events` stop event as a hit on breakpoint `id`, matching the NDS bridge's
/// `breakpoint_hit` shape.
pub(super) fn mark_breakpoint_hit(event: &mut Value, id: u64, kind: &str, address: u64) {
    if let Some(obj) = event.as_object_mut() {
        obj.insert("type".into(), json!("breakpoint_hit"));
        obj.insert("kind".into(), json!(kind));
        obj.insert("address".into(), json!(address));
        obj.insert("id".into(), json!(id));
        obj.insert("breakpoint_id".into(), json!(id));
    }
}

/// Compile `set_breakpoint`'s structured `pc_min`/`pc_max` filters plus an optional raw `condition`
/// string into a single PPSSPP `condition` expression (`Core/Debugger/WebSocket/BreakpointSubscriber.cpp`
/// parses/evaluates it via `initExpression`/`parseExpression`; a bad expression comes back as an
/// `emulator_error`, not a silent no-op). Returns `None` when no filter was given (PPSSPP breaks
/// unconditionally then).
pub(super) fn breakpoint_condition(params: &Value) -> BridgeResult<Option<String>> {
    let mut clauses = Vec::new();
    if let Some(raw) = params.get("condition").and_then(Value::as_str) {
        if !raw.trim().is_empty() {
            clauses.push(format!("({raw})"));
        }
    }
    let pc_min = optional_num(params, "pc_min")?;
    let pc_max = optional_num(params, "pc_max")?;
    if let (Some(min), Some(max)) = (pc_min, pc_max) {
        if min > max {
            return Err(BridgeError::BadParams("pc_min must be <= pc_max".into()));
        }
    }
    if let Some(min) = pc_min {
        clauses.push(format!("(pc >= 0x{min:x})"));
    }
    if let Some(max) = pc_max {
        clauses.push(format!("(pc <= 0x{max:x})"));
    }
    if clauses.is_empty() {
        Ok(None)
    } else {
        Ok(Some(clauses.join(" && ")))
    }
}

/// `step_instructions`' instruction count — accepts `count`, `n`, or `frames` (the MCP's
/// step-by-instructions tool sends `frames`; see the NDS bridge's identical `step_count`), defaulting
/// to 1 and never 0 (a 0-count step would be a silent no-op).
pub(super) fn step_count(params: &Value) -> BridgeResult<u64> {
    let count = match optional_num(params, "count")? {
        Some(count) => count,
        None => match optional_num(params, "n")? {
            Some(n) => n,
            None => optional_num(params, "frames")?.unwrap_or(1),
        },
    };
    Ok(count.max(1))
}

/// Resolve a `read_memory`/`write_memory` request's absolute PSP address from `memory_type`
/// (default `main`, the only region today) + `address`/`start` offset, bounding `[offset, offset+len)`
/// to the region. `len` is the access length (read `length`, write byte count).
pub(super) fn route_main_address(params: &Value, len: u64) -> BridgeResult<u64> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("main");
    if !MEMORY_TYPES.contains(&memory_type) {
        return Err(BridgeError::BadParams(format!(
            "unsupported memory_type: {memory_type}; valid: {}",
            MEMORY_TYPES.join(", ")
        )));
    }
    let offset = region_offset(params)?;
    // `[offset, offset+len)` must stay within `main` (user RAM). An offset past the region would be
    // forwarded to PPSSPP as an aliased/other region, so a `main` write could corrupt non-`main`
    // memory while the bridge reports success. Reject it (checked add, no wrap); read and write both
    // route here.
    if !matches!(offset.checked_add(len.max(1)), Some(end) if end <= PSP_MAIN_RAM_SIZE) {
        return Err(BridgeError::BadParams(format!(
            "{memory_type} access out of range: offset {offset:#x}+{len:#x} exceeds region size {PSP_MAIN_RAM_SIZE:#x}"
        )));
    }
    PSP_MAIN_RAM_BASE.checked_add(offset).ok_or_else(|| {
        BridgeError::BadParams(format!(
            "{memory_type} address overflow at offset {offset:#x}"
        ))
    })
}

pub(super) fn region_offset(params: &Value) -> BridgeResult<u64> {
    if let Some(value) = optional_num(params, "address")? {
        return Ok(value);
    }
    if let Some(value) = optional_num(params, "start")? {
        return Ok(value);
    }
    Err(BridgeError::BadParams(
        "missing required param: address".into(),
    ))
}

/// Absolute PSP address for `disassemble` — a raw address (e.g. `cpu.pc` from `get_state`), no
/// `memory_type` base added (unlike `read_memory`/`write_memory`).
pub(super) fn absolute_address(params: &Value) -> BridgeResult<u64> {
    if let Some(value) = optional_num(params, "address")? {
        return Ok(value);
    }
    if let Some(value) = optional_num(params, "start")? {
        return Ok(value);
    }
    Err(BridgeError::BadParams(
        "missing required param: address".into(),
    ))
}

/// Resolve a `set_breakpoint` address by kind. An exec breakpoint's `address`/`start` is a raw
/// absolute PSP address — a PC straight from `get_state`'s `cpu.pc` or `disassemble` — so
/// `memory_type` is ignored (a PC is not a `main`-region offset and is not always inside `main` RAM;
/// PPSSPP's cpu breakpoint takes an absolute address either way). A read/write watchpoint's
/// `address`/`start` is symmetric with `read_memory`/`write_memory`: a `memory_type` region offset
/// routed through the same `route_main_address` those two use (→ `PSP_MAIN_RAM_BASE + offset`, with
/// the identical out-of-range rejection), so it lands where `read_memory` reads. `len` is the watched
/// span (the memory breakpoint's `length`) and bounds `[offset, offset+len)` to the region.
pub(super) fn route_breakpoint_address(kind: &str, params: &Value, len: u64) -> BridgeResult<u64> {
    if kind == "exec" {
        absolute_address(params)
    } else {
        route_main_address(params, len)
    }
}

pub(super) fn required_num(params: &Value, key: &str) -> BridgeResult<u64> {
    let value = params
        .get(key)
        .ok_or_else(|| BridgeError::BadParams(format!("missing required param: {key}")))?;
    parse_num(value).ok_or_else(|| BridgeError::BadParams(format!("invalid numeric param: {key}")))
}

pub(super) fn optional_num(params: &Value, key: &str) -> BridgeResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| BridgeError::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

pub(super) fn require_input_port_zero(params: &Value) -> BridgeResult<()> {
    let port = optional_num(params, "port")?.unwrap_or(0);
    if port != 0 {
        return Err(BridgeError::BadParams(format!(
            "PPSSPP input supports only controller port 0 (got {port})"
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

pub(super) fn required_str<'a>(params: &'a Value, key: &str) -> BridgeResult<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::BadParams(format!("missing required param: {key}")))
}

/// sha1 of a content image's bytes, for `get_rom_info` (matches the NDS/PC-98 bridges' own
/// `sha1_file`).
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
    Ok(hex::encode(hasher.finalize()))
}

pub(super) fn absolute_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}
