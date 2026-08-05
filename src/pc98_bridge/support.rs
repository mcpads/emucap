use super::*;

pub(super) fn error_kind(err: &BridgeError) -> &'static str {
    match err {
        BridgeError::BadParams(_) => "bad_params",
        BridgeError::BadState(_) => "bad_state",
        BridgeError::UnknownMethod(_) => "unknown_method",
        BridgeError::Emulator(_) | BridgeError::Gdb(GdbError::Emulator(_)) => "emulator_error",
        BridgeError::Io(_)
        | BridgeError::Json(_)
        | BridgeError::Zip(_)
        | BridgeError::Gdb(GdbError::Io(_) | GdbError::Poisoned) => "bridge_error",
    }
}

pub(super) fn memory_type_names() -> Vec<&'static str> {
    MEMORY_REGIONS.iter().map(|r| r.name).collect()
}

pub(super) fn region_sizes_json() -> Value {
    let mut obj = serde_json::Map::new();
    for region in MEMORY_REGIONS {
        obj.insert(region.name.into(), json!(region.size));
    }
    Value::Object(obj)
}

pub(super) fn memory_region(name: &str) -> Option<&'static MemoryRegion> {
    MEMORY_REGIONS.iter().find(|r| r.name == name)
}

pub(super) fn region_address(params: &Value, length: u64) -> BridgeResult<u64> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("physical");
    let region = memory_region(memory_type)
        .ok_or_else(|| BridgeError::BadParams(format!("unsupported memory_type: {memory_type}")))?;
    let offset = required_num(params, "address")?;
    if !matches!(offset.checked_add(length), Some(end) if end <= region.size as u64) {
        return Err(BridgeError::BadParams(format!(
            "{memory_type} access out of range: offset {offset:#x}+{length:#x} exceeds region size {region_size:#x}",
            region_size = region.size
        )));
    }
    (region.base as u64).checked_add(offset).ok_or_else(|| {
        BridgeError::BadParams(format!(
            "{memory_type} address overflow at offset {offset:#x}"
        ))
    })
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
    if optional_num(params, "port")?.unwrap_or(0) != 0 {
        return Err(BridgeError::BadParams(
            "PC-98 input supports only port 0".into(),
        ));
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

pub(super) fn find_subslice(buf: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > buf.len() {
        return None;
    }
    buf.windows(needle.len()).position(|w| w == needle)
}

pub(super) fn input_buttons_json() -> Value {
    json!({
        "system": "pc98",
        "buttons": PC98_INPUT_BUTTONS,
        "aliases": {
            "return": "enter",
            "return_key": "enter",
            "start": "enter",
            "escape": "esc",
            "select": "space",
            "delete": "del",
            "insert": "ins",
            "bksp": "backspace",
            "bs": "backspace",
        },
        "notes": "PC-98 uses keyboard inputs. Prefer enter/esc/space/up/down/left/right plus letter, digit, f1-f10, and vf1-vf5 keys.",
    })
}

pub(super) fn normalize_buttons(raw: Option<&Value>) -> BridgeResult<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let Some(items) = raw.as_array() else {
        return Err(BridgeError::BadParams("buttons must be a list".into()));
    };
    items
        .iter()
        .map(|value| {
            let key = value
                .as_str()
                .map(|s| s.trim().to_ascii_lowercase())
                .unwrap_or_else(|| value.to_string().trim_matches('"').to_ascii_lowercase());
            let normalized = input_alias(&key).unwrap_or(&key);
            if PC98_INPUT_BUTTONS.contains(&normalized) {
                Ok(normalized.to_string())
            } else {
                Err(BridgeError::BadParams(format!(
                    "unsupported PC-98 key: {key}"
                )))
            }
        })
        .collect()
}

pub(super) fn input_alias(key: &str) -> Option<&'static str> {
    match key {
        "return" | "return_key" | "start" => Some("enter"),
        "escape" => Some("esc"),
        "select" => Some("space"),
        "delete" => Some("del"),
        "insert" => Some("ins"),
        "bksp" | "bs" => Some("backspace"),
        _ => None,
    }
}

pub(super) fn is_stop_packet(resp: &str) -> bool {
    resp.starts_with('S') || resp.starts_with('T')
}

/// 이 명령의 정상 RSP 응답 자체가 stop 패킷인 명령인지. continue/step/`?`/vCont 외에,
/// framestep·runframes·press는 프레임 노티파이어가 목표에 도달할 때 stop을 지연 응답으로 보내므로
/// 여기에 포함한다 — 이들 응답의 stop은 stale이 아니라 정상 응답이라 demux하면 안 된다.
pub(super) fn command_expects_stop(payload: &str) -> bool {
    payload == "c"
        || payload == "s"
        || payload == "?"
        || payload.starts_with('C')
        || payload.starts_with('S')
        || payload.starts_with("vCont")
        || payload.starts_with("qEmucap,framestep")
        || payload.starts_with("qEmucap,runframes")
        || payload.starts_with("qEmucap,press")
}

pub(super) fn parse_breakpoint_reply(resp: &str) -> BridgeResult<(String, u64)> {
    let (kind, id) = resp
        .split_once(':')
        .ok_or_else(|| BridgeError::Emulator(format!("MAME breakpoint set failed: {resp}")))?;
    let backend = match kind {
        "BP" => "bp",
        "WP" => "wp",
        _ => {
            return Err(BridgeError::Emulator(format!(
                "MAME breakpoint set failed: {resp}"
            )))
        }
    };
    let id = id
        .parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("MAME breakpoint set failed: {resp}")))?;
    Ok((backend.into(), id))
}

pub(super) fn parse_regpoint_reply(resp: &str) -> BridgeResult<u64> {
    let Some(id) = resp.strip_prefix("RP:") else {
        return Err(BridgeError::Emulator(format!(
            "MAME registerpoint set failed: {resp}"
        )));
    };
    id.parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("MAME registerpoint set failed: {resp}")))
}

pub(super) fn empty_to_none(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

pub(super) fn breakpoint_condition(params: &Value, kind: &str) -> BridgeResult<String> {
    let mut clauses = Vec::new();
    let pc_min = optional_num(params, "pc_min")?;
    let pc_max = optional_num(params, "pc_max")?;
    if let Some(pc_min) = pc_min {
        clauses.push(format!("pc >= {pc_min:X}"));
    }
    if let Some(pc_max) = pc_max {
        clauses.push(format!("pc <= {pc_max:X}"));
    }
    if let (Some(pc_min), Some(pc_max)) = (pc_min, pc_max) {
        if pc_min > pc_max {
            return Err(BridgeError::BadParams("pc_min must be <= pc_max".into()));
        }
    }

    let has_value_filter = params.get("value").is_some()
        || params.get("value_mask").is_some()
        || params.get("value_len").is_some();
    if has_value_filter {
        if kind != "read" && kind != "write" {
            return Err(BridgeError::BadParams(
                "value filters only apply to read/write breakpoints".into(),
            ));
        }
        let value = required_num(params, "value")?;
        let value_len = optional_num(params, "value_len")?.unwrap_or(1);
        if !(1..=4).contains(&value_len) {
            return Err(BridgeError::BadParams(
                "value_len must be 1..4 for MAME PC-98".into(),
            ));
        }
        let all_bits = (1u64 << (value_len * 8)) - 1;
        let mask = optional_num(params, "value_mask")?.unwrap_or(all_bits) & all_bits;
        let value = value & all_bits;
        clauses.push(format!("(wpdata & {mask:X}) == {:X}", value & mask));
    }

    Ok(clauses
        .into_iter()
        .map(|clause| format!("({clause})"))
        .collect::<Vec<_>>()
        .join(" && "))
}

pub(super) fn parse_snapshot_specs(raw: Option<&Value>) -> BridgeResult<Vec<SnapshotSpec>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = raw.as_array() else {
        return Err(BridgeError::BadParams("snapshot must be a list".into()));
    };
    let mut out = Vec::new();
    for item in items {
        let Some(raw_spec) = item.as_str() else {
            return Err(BridgeError::BadParams(format!(
                "invalid snapshot spec: {item}"
            )));
        };
        let parts: Vec<_> = raw_spec.split(':').collect();
        if parts.len() != 3 {
            return Err(BridgeError::BadParams(format!(
                "invalid snapshot spec: {raw_spec}"
            )));
        }
        if memory_region(parts[0]).is_none() {
            return Err(BridgeError::BadParams(format!(
                "unsupported snapshot memory_type: {}",
                parts[0]
            )));
        }
        let address = parse_num_str(parts[1]).ok_or_else(|| {
            BridgeError::BadParams(format!("invalid snapshot address: {}", parts[1]))
        })? as usize;
        let length = parse_num_str(parts[2]).ok_or_else(|| {
            BridgeError::BadParams(format!("invalid snapshot length: {}", parts[2]))
        })? as usize;
        if length > MAX_READ_CHUNK {
            return Err(BridgeError::BadParams(format!(
                "snapshot length exceeds {MAX_READ_CHUNK} bytes"
            )));
        }
        out.push(SnapshotSpec {
            memory_type: parts[0].into(),
            address,
            length,
        });
    }
    Ok(out)
}

pub(super) fn normalize_debug_register(
    raw_reg: &str,
) -> BridgeResult<(&'static str, &'static str)> {
    let mut key = raw_reg.trim().to_ascii_lowercase();
    if let Some(stripped) = key.strip_prefix("cpu.") {
        key = stripped.into();
    }
    match key.as_str() {
        "eax" | "ax" => Ok(("eax", "cpu.eax")),
        "ecx" | "cx" => Ok(("ecx", "cpu.ecx")),
        "edx" | "dx" => Ok(("edx", "cpu.edx")),
        "ebx" | "bx" => Ok(("ebx", "cpu.ebx")),
        "esp" | "sp" => Ok(("esp", "cpu.esp")),
        "ebp" | "bp" => Ok(("ebp", "cpu.ebp")),
        "esi" | "si" => Ok(("esi", "cpu.esi")),
        "edi" | "di" => Ok(("edi", "cpu.edi")),
        "eip" | "ip" => Ok(("eip", "cpu.eip")),
        "offset_pc" => Ok(("eip", "cpu.offset_pc")),
        "pc" => Ok(("pc", "cpu.pc")),
        "eflags" | "flags" => Ok(("eflags", "cpu.eflags")),
        "cs" => Ok(("cs", "cpu.cs")),
        "ss" => Ok(("ss", "cpu.ss")),
        "ds" => Ok(("ds", "cpu.ds")),
        "es" => Ok(("es", "cpu.es")),
        "fs" => Ok(("fs", "cpu.fs")),
        "gs" => Ok(("gs", "cpu.gs")),
        _ => Err(BridgeError::BadParams(format!(
            "unsupported PC-98 register: {raw_reg}; valid: ax, bx, cx, dx, sp, bp, si, di, ip, pc, flags, cs, ss, ds, es, fs, gs"
        ))),
    }
}

pub(super) fn stop_event(stop: &str) -> Value {
    let mut event = json!({ "type": "stop", "signal": stop.get(1..3).unwrap_or(""), "raw": stop });
    if !stop.starts_with('T') {
        return event;
    }
    let Some(body) = stop.get(3..) else {
        return event;
    };
    let Some((key, rest)) = body.split_once(':') else {
        return event;
    };
    let mut parts = rest.split(';');
    let raw_hex = parts.next().unwrap_or_default();
    let mut fields = BTreeMap::new();
    for item in parts {
        if let Some((field, value)) = item.split_once(':') {
            fields.insert(field, value);
        }
    }
    let Some(address) = little_hex_to_u64(raw_hex) else {
        return event;
    };
    match key {
        "hwbreak" => {
            set_event_field(&mut event, "type", json!("breakpoint_hit"));
            set_event_field(&mut event, "kind", json!("exec"));
            set_event_field(&mut event, "address", json!(address));
        }
        "watch" => {
            set_event_field(&mut event, "type", json!("breakpoint_hit"));
            set_event_field(&mut event, "kind", json!("write"));
            set_event_field(&mut event, "address", json!(address));
        }
        "rwatch" => {
            set_event_field(&mut event, "type", json!("breakpoint_hit"));
            set_event_field(&mut event, "kind", json!("read"));
            set_event_field(&mut event, "address", json!(address));
        }
        "awatch" => {
            set_event_field(&mut event, "type", json!("breakpoint_hit"));
            set_event_field(&mut event, "kind", json!("access"));
            set_event_field(&mut event, "address", json!(address));
        }
        "reset" => {
            set_event_field(&mut event, "type", json!("reset"));
            set_event_field(&mut event, "pc", json!(address));
            set_event_field(&mut event, "address", json!(address));
        }
        "regwatch" => {
            set_event_field(&mut event, "type", json!("register_break"));
            set_event_field(&mut event, "pc", json!(address));
            set_event_field(&mut event, "address", json!(address));
        }
        _ => {}
    }
    if let Some(idx) = fields.get("idx") {
        match idx.parse::<u64>() {
            Ok(idx) => set_event_field(&mut event, "backend_id", json!(idx)),
            Err(_) => set_event_field(&mut event, "backend_id_error", json!(idx)),
        }
    }
    if let Some(regs_hex) = fields.get("regs") {
        set_event_field(&mut event, "regs", state_from_regs_hex(regs_hex));
    }
    event
}

pub(super) fn little_hex_to_u64(raw: &str) -> Option<u64> {
    let bytes = hex::decode(raw).ok()?;
    let mut padded = [0u8; 8];
    let len = bytes.len().min(8);
    padded[..len].copy_from_slice(&bytes[..len]);
    Some(u64::from_le_bytes(padded))
}

pub(super) fn set_event_field(event: &mut Value, key: &str, value: Value) {
    if let Some(obj) = event.as_object_mut() {
        obj.insert(key.into(), value);
    }
}

pub(super) fn mark_event_enriched(event: &mut Value) {
    set_event_field(event, "_pc98_enriched", json!(true));
}

pub(super) fn state_restore_info() -> Value {
    json!({
        "format": STATE_FORMAT,
        "scope": "cpu-register-packet-plus-ram-tvram-gvram-plus-mame-save-items",
        "deterministic_replay": true,
        "hidden_device_state": true,
        "save_manager_items": true,
        "save_manager_restore": "best_effort_lua_item_write",
        "post_restore_instruction_exact": true,
        "native_atomic_machine_state_load": false,
        "freeze_strategy": "lua_frozen_socket_service",
        "notes": "PC-98 state bundles restore RAM/TVRAM/GVRAM, MAME save-manager items exposed through Lua, and the i386 register packet.",
    })
}

pub(super) fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// A sibling `.partial` temp of `dst` (same parent → the later rename stays on one filesystem and is
/// atomic), tagged with this process id and a nanosecond stamp. save_state stages the zip here and
/// renames over `dst` only when complete, so a mid-save failure never truncates a pre-existing save.
pub(super) fn state_partial_sibling(dst: &Path) -> BridgeResult<PathBuf> {
    let parent = dst.parent().ok_or_else(|| {
        BridgeError::BadParams(format!(
            "save path {} has no parent directory to stage under",
            dst.display()
        ))
    })?;
    let name = dst
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("state.zip");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".{name}.partial.{}.{nanos}", std::process::id())))
}

pub(super) fn unique_temp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    for attempt in 0..100u32 {
        let path = std::env::temp_dir().join(format!(
            "{prefix}{}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
            attempt
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temp directory",
    ))
}

pub(super) fn save_item_members(root: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
                continue;
            }
            if path.is_file() {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let member = rel
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((path, format!("{SAVE_ITEMS_DIR}/{member}")));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

pub(super) fn parse_save_items_response(
    resp: &str,
    command: &str,
) -> BridgeResult<serde_json::Map<String, Value>> {
    let parts: Vec<_> = resp.split('|').collect();
    if parts.len() != 3 || parts[0] != "OK" {
        return Err(BridgeError::Emulator(format!(
            "MAME Lua command {command} failed: {resp}"
        )));
    }
    let items = parts[1]
        .parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("MAME Lua command {command} failed: {resp}")))?;
    let skipped = parts[2]
        .parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("MAME Lua command {command} failed: {resp}")))?;
    let mut out = serde_json::Map::new();
    out.insert("items".into(), json!(items));
    out.insert("skipped".into(), json!(skipped));
    Ok(out)
}

pub(super) fn read_state_manifest<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> BridgeResult<Value> {
    let mut file = archive.by_name("state.json")?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}

pub(super) fn state_format(manifest: &Value) -> BridgeResult<String> {
    let format = manifest
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if format != STATE_FORMAT && format != LEGACY_STATE_FORMAT {
        return Err(BridgeError::BadParams(format!(
            "unsupported PC-98 state format: {format}"
        )));
    }
    Ok(format.into())
}

pub(super) fn extract_save_items<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &Value,
    target_root: &Path,
) -> BridgeResult<Option<PathBuf>> {
    let Some(save_items) = manifest.get("save_items").and_then(Value::as_object) else {
        return Ok(None);
    };
    let directory = save_items
        .get("dir")
        .and_then(Value::as_str)
        .unwrap_or(SAVE_ITEMS_DIR)
        .trim_matches('/');
    if directory != SAVE_ITEMS_DIR {
        return Err(BridgeError::BadParams(format!(
            "unsupported PC-98 save item directory: {directory}"
        )));
    }
    let names = archive
        .file_names()
        .filter(|name| name.starts_with(&format!("{SAVE_ITEMS_DIR}/")))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !names.iter().any(|name| name == SAVE_ITEMS_MANIFEST) {
        return Err(BridgeError::BadParams(
            "PC-98 save item manifest is missing".into(),
        ));
    }
    let out_dir = target_root.join(SAVE_ITEMS_DIR);
    for name in names {
        let rel = &name[SAVE_ITEMS_DIR.len() + 1..];
        if rel.is_empty() || rel.ends_with('/') {
            continue;
        }
        let parts = rel.split('/').collect::<Vec<_>>();
        if parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == ".." || part.contains('\\'))
        {
            return Err(BridgeError::BadParams(format!(
                "unsafe PC-98 save item member: {name}"
            )));
        }
        let dest = parts
            .iter()
            .fold(out_dir.clone(), |path, part| path.join(part));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut src = archive.by_name(&name)?;
        let mut bytes = Vec::new();
        src.read_to_end(&mut bytes)?;
        fs::write(dest, bytes)?;
    }
    Ok(Some(out_dir))
}

pub(super) fn read_state_regions<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &Value,
) -> BridgeResult<Vec<(String, Vec<u8>)>> {
    let Some(regions) = manifest.get("regions").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for region in regions {
        let memory_type = region
            .get("memory_type")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BridgeError::BadParams("PC-98 state region missing memory_type".into())
            })?;
        let file_name = region
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| BridgeError::BadParams("PC-98 state region missing file".into()))?;
        let mut file = archive.by_name(file_name)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        out.push((memory_type.into(), bytes));
    }
    Ok(out)
}

pub(super) fn parse_register_probe_response(resp: &str) -> BridgeResult<Value> {
    if !resp.starts_with("HEX:") {
        return Err(BridgeError::Emulator(format!(
            "MAME register probe failed: {resp}"
        )));
    }
    let mut fields = BTreeMap::new();
    for part in resp.split('|') {
        if let Some((key, value)) = part.split_once(':') {
            fields.insert(key, value);
        }
    }
    let hexstr = fields.get("HEX").copied().unwrap_or_default();
    if hexstr.len() % 2 != 0 {
        return Err(BridgeError::Emulator(format!(
            "MAME register probe returned odd-length hex: {hexstr}"
        )));
    }
    let mut out = serde_json::Map::new();
    out.insert("hex".into(), json!(hexstr));
    out.insert("state_restore".into(), state_restore_info());
    if let Some(frame) = fields.get("FRAME") {
        match frame.parse::<u64>() {
            Ok(frame) => {
                out.insert("frame".into(), json!(frame));
            }
            Err(_) => {
                out.insert("frame_error".into(), json!(frame));
            }
        }
    }
    if let Some(regs_hex) = fields.get("REGS") {
        out.insert("regs".into(), state_from_regs_hex(regs_hex));
    }
    Ok(Value::Object(out))
}

pub(super) fn state_matches_real_mode_pc(current: &Value, target: &Value) -> bool {
    let current_cs = current
        .get("cpu.cs")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let current_eip = current
        .get("cpu.eip")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let target_cs = target
        .get("cpu.cs")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let target_eip = target
        .get("cpu.eip")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    (current_cs & 0xFFFF) == target_cs && (current_eip & 0xFFFF) == target_eip
}

pub(super) fn parse_dasm_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
    count: usize,
) -> Vec<Value> {
    let mut out = Vec::new();
    for line in lines {
        if out.len() >= count {
            break;
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let Some((addr_raw, rest_raw)) = raw.split_once(':') else {
            continue;
        };
        let Ok(addr) = u64::from_str_radix(addr_raw.trim(), 16) else {
            continue;
        };
        let rest = rest_raw.trim();
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let mut byte_parts = Vec::new();
        let mut idx = 0usize;
        while idx < parts.len() && is_hex_byte(parts[idx]) {
            byte_parts.push(parts[idx].to_ascii_lowercase());
            idx += 1;
        }
        let text = if idx < parts.len() {
            parts[idx..].join(" ")
        } else {
            rest.to_string()
        };
        let mut item = serde_json::Map::new();
        item.insert("addr".into(), json!(addr));
        item.insert("text".into(), json!(text));
        if !byte_parts.is_empty() {
            item.insert("bytes".into(), json!(byte_parts.join("")));
        }
        out.push(Value::Object(item));
    }
    out
}

pub(super) fn is_hex_byte(s: &str) -> bool {
    s.len() == 2 && s.as_bytes().iter().all(u8::is_ascii_hexdigit)
}

pub(super) fn parse_trace_line(line: &str) -> Option<Value> {
    let raw = line.trim();
    if raw.is_empty() {
        return None;
    }
    let Some((left, rest_raw)) = raw.split_once(':') else {
        return Some(json!({ "raw": raw }));
    };
    let token = left.split_whitespace().last().unwrap_or(left).trim();
    if token.len() < 4 || token.len() > 8 || !token.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Some(json!({ "raw": raw }));
    }
    let Ok(pc) = u64::from_str_radix(token, 16) else {
        return Some(json!({ "raw": raw }));
    };
    let rest = rest_raw.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let mut byte_parts = Vec::new();
    let mut idx = 0usize;
    while idx < parts.len() && is_hex_byte(parts[idx]) {
        byte_parts.push(parts[idx].to_ascii_lowercase());
        idx += 1;
    }
    let text = if idx < parts.len() {
        parts[idx..].join(" ")
    } else {
        rest.to_string()
    };
    let mut row = serde_json::Map::new();
    row.insert("pc".into(), json!(pc));
    row.insert("text".into(), json!(text));
    row.insert("raw".into(), json!(raw));
    if !byte_parts.is_empty() {
        row.insert("bytes".into(), json!(byte_parts.join("")));
    }
    Some(Value::Object(row))
}

pub(super) fn state_from_regs_hex(resp: &str) -> Value {
    let mut state = serde_json::Map::new();
    if resp.len() >= I386_REGS.len() * 8 {
        decode_regs(resp, I386_REGS, 4, &mut state);
        if let Some(eip) = state.get("cpu.eip").and_then(Value::as_u64) {
            state.insert("cpu.offset_pc".into(), json!(eip));
            state.insert(
                "cpu.pc".into(),
                json!(segmented_pc(
                    state.get("cpu.cs").and_then(Value::as_u64).unwrap_or(0),
                    eip
                )),
            );
        }
    } else if resp.len() >= I86_REGS.len() * 4 {
        decode_regs(resp, I86_REGS, 2, &mut state);
        if let Some(ip) = state.get("cpu.ip").and_then(Value::as_u64) {
            state.insert("cpu.offset_pc".into(), json!(ip));
            state.insert(
                "cpu.pc".into(),
                json!(segmented_pc(
                    state.get("cpu.cs").and_then(Value::as_u64).unwrap_or(0),
                    ip
                )),
            );
        }
    } else {
        state.insert("cpu.raw_register_bytes".into(), json!(resp.len() / 2));
    }
    Value::Object(state)
}

pub(super) fn decode_regs(
    resp: &str,
    names: &[&str],
    width: usize,
    state: &mut serde_json::Map<String, Value>,
) {
    let chars = width * 2;
    for (idx, name) in names.iter().enumerate() {
        let start = idx * chars;
        let end = start + chars;
        if end > resp.len() {
            break;
        }
        if let Ok(bytes) = hex::decode(&resp[start..end]) {
            let mut raw = [0u8; 8];
            raw[..bytes.len()].copy_from_slice(&bytes);
            let value = u64::from_le_bytes(raw);
            state.insert(format!("cpu.{name}"), json!(value));
        }
    }
}

pub(super) fn segmented_pc(cs: u64, ip: u64) -> u64 {
    ((cs << 4) + ip) & 0xFFFF_FFFF
}

pub(super) fn sha1_file(path: &Path) -> std::io::Result<String> {
    let mut h = Sha1::new();
    let mut file = File::open(path)?;
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub(super) fn absolute_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}
