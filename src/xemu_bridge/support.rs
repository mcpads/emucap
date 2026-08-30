use super::*;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use sha1::{Digest, Sha1};

pub(super) fn error_kind(error: &XemuBridgeError) -> &'static str {
    match error {
        XemuBridgeError::BadParams(_) => "bad_params",
        XemuBridgeError::BadState(_) => "bad_state",
        XemuBridgeError::UnknownMethod(_) => "unknown_method",
        XemuBridgeError::Unsupported(_) => "unsupported",
        XemuBridgeError::Emulator(_) | XemuBridgeError::Gdb(_) | XemuBridgeError::Qmp(_) => {
            "emulator_error"
        }
        XemuBridgeError::Io(_) | XemuBridgeError::Json(_) => "bridge_error",
    }
}

pub(super) fn parse_num(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => {
            let text = text.trim();
            if let Some(hex) = text
                .strip_prefix("0x")
                .or_else(|| text.strip_prefix("0X"))
                .or_else(|| text.strip_prefix('$'))
            {
                u64::from_str_radix(hex, 16).ok()
            } else {
                text.parse().ok()
            }
        }
        _ => None,
    }
}

pub(super) fn required_num(params: &Value, key: &str) -> XemuResult<u64> {
    params.get(key).and_then(parse_num).ok_or_else(|| {
        XemuBridgeError::BadParams(format!("missing or invalid numeric param: {key}"))
    })
}

pub(super) fn optional_num(params: &Value, key: &str) -> XemuResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| XemuBridgeError::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

pub(super) fn required_str<'a>(params: &'a Value, key: &str) -> XemuResult<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| XemuBridgeError::BadParams(format!("missing required param: {key}")))
}

pub(super) fn step_count(params: &Value) -> XemuResult<u64> {
    let count = optional_num(params, "count")?
        .or(optional_num(params, "frames")?)
        .unwrap_or(1);
    if count == 0 || count > crate::live::temporal::MAX_SYNC_ADVANCE_COUNT {
        return Err(XemuBridgeError::BadParams(format!(
            "step count must be in 1..={}, got {count}",
            crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
        )));
    }
    Ok(count)
}

pub(super) fn sha1_regular_file(path: &Path) -> XemuResult<(u64, String)> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(XemuBridgeError::BadParams(format!(
            "expected a regular non-symlink file: {}",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() || opened_metadata.len() != metadata.len() {
        return Err(XemuBridgeError::BadState(format!(
            "file changed while opening: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened_metadata.dev() != metadata.dev() || opened_metadata.ino() != metadata.ino() {
            return Err(XemuBridgeError::BadState(format!(
                "file identity changed while opening: {}",
                path.display()
            )));
        }
    }
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    if size != opened_metadata.len() {
        return Err(XemuBridgeError::BadState(format!(
            "file changed while hashing: {}",
            path.display()
        )));
    }
    Ok((size, hex::encode(hasher.finalize())))
}

pub(super) fn is_stop_packet(packet: &str) -> bool {
    packet.starts_with('S') || packet.starts_with('T')
}

pub(super) fn stop_signal(packet: &str) -> &str {
    packet.get(1..3).unwrap_or("")
}

pub(super) fn stop_watch_address(packet: &str) -> Option<(String, u64)> {
    let fields = if packet.starts_with('T') && packet.len() >= 3 {
        &packet[3..]
    } else {
        packet
    };
    for field in fields.split(';') {
        for kind in ["watch", "rwatch", "awatch"] {
            if let Some(raw) = field.strip_prefix(&format!("{kind}:")) {
                if let Ok(address) = u64::from_str_radix(raw, 16) {
                    return Some((kind.into(), address));
                }
            }
        }
    }
    None
}

pub(super) fn parse_i386_registers(raw: &str) -> XemuResult<Value> {
    let bytes = hex::decode(raw)
        .map_err(|_| XemuBridgeError::Emulator("GDB returned invalid register hex".into()))?;
    const NAMES: [&str; 16] = [
        "eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi", "eip", "eflags", "cs", "ss", "ds",
        "es", "fs", "gs",
    ];
    if bytes.len() < NAMES.len() * 4 {
        return Err(XemuBridgeError::Emulator(format!(
            "short i386 register packet: expected at least {} bytes, got {}",
            NAMES.len() * 4,
            bytes.len()
        )));
    }
    let mut state = serde_json::Map::new();
    for (index, name) in NAMES.iter().enumerate() {
        let start = index * 4;
        let value = u32::from_le_bytes(bytes[start..start + 4].try_into().expect("four bytes"));
        state.insert(format!("cpu.{name}"), json!(value));
    }
    Ok(Value::Object(state))
}

pub(super) fn region_address(params: &Value, length: u64) -> XemuResult<(String, u64, u64)> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("main");
    let offset = params
        .get("address")
        .or_else(|| params.get("start"))
        .and_then(parse_num)
        .unwrap_or(0);
    match memory_type {
        "main" => {
            let end = offset
                .checked_add(length)
                .ok_or_else(|| XemuBridgeError::BadParams("main memory range overflow".into()))?;
            if end > XBOX_RAM_SIZE {
                return Err(XemuBridgeError::BadParams(format!(
                    "main memory range [{offset:#x}, {end:#x}) exceeds [0, {XBOX_RAM_SIZE:#x})"
                )));
            }
            Ok((memory_type.into(), XBOX_RAM_CPU_ALIAS + offset, offset))
        }
        "cpu" => {
            let end = offset
                .checked_add(length)
                .ok_or_else(|| XemuBridgeError::BadParams("CPU address range overflow".into()))?;
            if end > 0x1_0000_0000 {
                return Err(XemuBridgeError::BadParams(
                    "CPU address range exceeds the 32-bit address space".into(),
                ));
            }
            Ok((memory_type.into(), offset, offset))
        }
        other => Err(XemuBridgeError::BadParams(format!(
            "unsupported Xbox memory_type: {other}; valid: main, cpu"
        ))),
    }
}

pub(super) fn find_subslice(buffer: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > buffer.len() {
        return None;
    }
    buffer
        .windows(needle.len())
        .position(|window| window == needle)
}
