use super::*;

pub(super) fn max_sync_frame_count() -> u64 {
    let deadline_ms = crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64;
    deadline_ms
        .saturating_sub(FRAME_OPERATION_STARTUP_MS)
        .checked_div(FRAME_OPERATION_BUDGET_MS)
        .unwrap_or(0)
        .min(crate::live::temporal::MAX_SYNC_ADVANCE_COUNT)
}

pub(super) fn required_path(params: &Value, key: &str) -> BridgeResult<PathBuf> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| BridgeError::BadParams(format!("missing or invalid param: {key}")))
}

pub(super) fn error_kind(error: &BridgeError) -> &'static str {
    match error {
        BridgeError::BadParams(_) => "bad_params",
        BridgeError::BadState(_) => "bad_state",
        BridgeError::UnknownMethod(_) => "unknown_method",
        BridgeError::Emulator(_) | BridgeError::Gdb(GdbError::Emulator(_)) => "emulator_error",
        BridgeError::Io(_) | BridgeError::Gdb(_) => "bridge_error",
    }
}

pub(super) fn is_stop(value: &str) -> bool {
    value.starts_with('S') || value.starts_with('T')
}

fn parse_num(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => {
            let value = value.trim();
            if let Some(value) = value.strip_prefix("0x").or_else(|| value.strip_prefix('$')) {
                u64::from_str_radix(value, 16).ok()
            } else {
                value.parse().ok()
            }
        }
        _ => None,
    }
}

pub(super) fn required_num(params: &Value, key: &str) -> BridgeResult<u64> {
    params
        .get(key)
        .and_then(parse_num)
        .ok_or_else(|| BridgeError::BadParams(format!("missing or invalid param: {key}")))
}

pub(super) fn optional_num(params: &Value, key: &str) -> BridgeResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| BridgeError::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

pub(super) fn region_address(
    profile: NeoGeoProfile,
    params: &Value,
    length: u64,
) -> BridgeResult<u64> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("ram");
    if memory_type != "ram" {
        return Err(BridgeError::BadParams(format!(
            "unsupported Neo Geo memory_type: {memory_type}"
        )));
    }
    let offset = required_num(params, "address")?;
    let (ram_base, ram_size) = profile.ram();
    if !matches!(offset.checked_add(length), Some(end) if end <= ram_size) {
        return Err(BridgeError::BadParams(format!(
            "ram access out of range: offset {offset:#x}+{length:#x} exceeds {ram_size:#x}"
        )));
    }
    ram_base
        .checked_add(offset)
        .ok_or_else(|| BridgeError::BadParams("ram address overflow".into()))
}

pub(super) fn require_port_zero(params: &Value) -> BridgeResult<()> {
    if optional_num(params, "port")?.unwrap_or(0) == 0 {
        Ok(())
    } else {
        Err(BridgeError::BadParams(
            "Neo Geo input currently supports port 0 only".into(),
        ))
    }
}

pub(super) fn require_main_cpu(params: &Value) -> BridgeResult<()> {
    match params.get("cpu").and_then(Value::as_str) {
        None | Some("main" | "maincpu" | "m68000" | "m68k" | "68k") => Ok(()),
        Some(cpu) => Err(BridgeError::BadParams(format!(
            "Neo Geo execution control currently supports the m68000 main CPU only, got {cpu}"
        ))),
    }
}

pub(super) fn normalize_buttons(
    profile: NeoGeoProfile,
    value: Option<&Value>,
) -> BridgeResult<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| BridgeError::BadParams("buttons must be a list".into()))?;
    values
        .iter()
        .map(|value| {
            let key = value
                .as_str()
                .ok_or_else(|| BridgeError::BadParams("button names must be strings".into()))?
                .trim()
                .to_ascii_lowercase();
            if profile.input_buttons().contains(&key.as_str()) {
                Ok(key)
            } else {
                Err(BridgeError::BadParams(format!(
                    "unsupported Neo Geo button: {key}"
                )))
            }
        })
        .collect()
}
