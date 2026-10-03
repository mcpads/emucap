//! N64 request parameter and address validation.
use super::*;

pub(super) fn parse_num(value: &Value) -> Option<u64> {
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

pub(super) fn required_num(params: &Value, key: &str) -> N64Result<u64> {
    params
        .get(key)
        .and_then(parse_num)
        .ok_or_else(|| N64Error::BadParams(format!("missing or invalid param: {key}")))
}

pub(super) fn optional_num(params: &Value, key: &str) -> N64Result<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| N64Error::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

pub(super) fn rdram_address(params: &Value, length: u64) -> N64Result<u64> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("rdram");
    if memory_type != "rdram" {
        return Err(N64Error::BadParams(format!(
            "unsupported N64 memory_type: {memory_type}"
        )));
    }
    let offset = required_num(params, "address")?;
    if !matches!(offset.checked_add(length), Some(end) if end <= RDRAM_SIZE) {
        return Err(N64Error::BadParams(format!(
            "rdram access out of range: offset {offset:#x}+{length:#x} exceeds {RDRAM_SIZE:#x}"
        )));
    }
    RDRAM_BASE
        .checked_add(offset)
        .ok_or_else(|| N64Error::BadParams("RDRAM address overflow".into()))
}

pub(super) fn require_r4300(params: &Value) -> N64Result<()> {
    match params.get("cpu").and_then(Value::as_str) {
        None | Some("main" | "r4300" | "maincpu") => Ok(()),
        Some(cpu) => Err(N64Error::BadParams(format!(
            "N64 execution control currently supports the R4300 CPU only, got {cpu}"
        ))),
    }
}
