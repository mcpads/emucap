use super::*;
use std::io::Read;

pub(super) const INPUT_BUTTONS: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "up",
    "down",
    "left",
    "right",
    "enter",
    "escape",
    "space",
    "backspace",
    "tab",
    "kp0",
    "kp1",
    "kp2",
    "kp3",
    "kp4",
    "kp5",
    "kp6",
    "kp7",
    "kp8",
    "kp9",
    "kp_period",
    "kp_divide",
    "kp_multiply",
    "kp_minus",
    "kp_plus",
    "kp_enter",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "shift",
    "ctrl",
    "alt",
    "insert",
    "delete",
    "home",
    "end",
    "page_up",
    "page_down",
    "mouse_left",
    "mouse_right",
];

const FIRMWARE_FILES: &[&str] = &[
    "bios.rom",
    "font.rom",
    "font.bmp",
    "itf.rom",
    "sound.rom",
    "bios9821.rom",
    "d8000.rom",
    "2608_bd.wav",
    "2608_sd.wav",
    "2608_top.wav",
    "2608_hh.wav",
    "2608_tom.wav",
    "2608_rim.wav",
];

pub(super) fn validate_content(path: &Path) -> Np2kaiResult<()> {
    if !path.is_file() {
        return Err(Np2kaiError::BadParams(format!(
            "PC-98 content not found: {}",
            path.display()
        )));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if !extension.eq_ignore_ascii_case("hdi") {
        return Err(Np2kaiError::BadParams(
            "the NP2kai backend currently accepts .hdi hard-disk images only".into(),
        ));
    }
    Ok(())
}

pub(super) fn ensure_plain_directory(path: &Path) -> Np2kaiResult<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Np2kaiError::BadState(format!(
            "managed path is not a plain directory: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn copy_file_exclusive(source: &Path, destination: &Path) -> Np2kaiResult<()> {
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let result = (|| -> Np2kaiResult<()> {
        let mut input = fs::File::open(source)?;
        std::io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

pub(super) fn stage_firmware(source: &Path, destination: &Path) -> Np2kaiResult<String> {
    if !source.is_dir() {
        return Err(Np2kaiError::BadParams(format!(
            "NP2kai firmware directory not found: {}",
            source.display()
        )));
    }
    let mut available = BTreeMap::<String, PathBuf>::new();
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if FIRMWARE_FILES.contains(&name.as_str())
            && available.insert(name.clone(), entry.path()).is_some()
        {
            return Err(Np2kaiError::BadParams(format!(
                "firmware directory contains duplicate case variants for {name}"
            )));
        }
    }
    if !available.contains_key("bios.rom") {
        return Err(Np2kaiError::BadParams(
            "NP2kai requires bios.rom in the selected firmware directory".into(),
        ));
    }
    if !available.contains_key("font.rom") && !available.contains_key("font.bmp") {
        return Err(Np2kaiError::BadParams(
            "NP2kai requires font.rom or font.bmp in the selected firmware directory".into(),
        ));
    }
    fs::create_dir_all(destination)?;
    for name in FIRMWARE_FILES {
        let staged = destination.join(name);
        if staged.is_file() {
            fs::remove_file(staged)?;
        }
    }
    let mut manifest = Vec::new();
    for (name, source_path) in available {
        let staged = destination.join(&name);
        fs::copy(&source_path, &staged)?;
        let digest = sha256_file(&staged)?;
        if digest != sha256_file(&source_path)? {
            return Err(Np2kaiError::Core(format!(
                "staged firmware digest changed while copying {name}"
            )));
        }
        manifest.extend_from_slice(name.as_bytes());
        manifest.push(0);
        manifest.extend_from_slice(digest.as_bytes());
        manifest.push(b'\n');
    }
    Ok(hex::encode(Sha256::digest(&manifest)))
}

pub(super) fn normalize_buttons(value: Option<&Value>) -> Np2kaiResult<BTreeSet<String>> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| Np2kaiError::BadParams("buttons must be an array".into()))?;
    let mut buttons = BTreeSet::new();
    for value in values {
        let raw = value
            .as_str()
            .ok_or_else(|| Np2kaiError::BadParams("button names must be strings".into()))?;
        let normalized = canonical_button(raw)
            .ok_or_else(|| Np2kaiError::BadParams(format!("unsupported NP2kai key: {raw}")))?;
        buttons.insert(normalized.to_string());
    }
    Ok(buttons)
}

pub(super) fn canonical_button(raw: &str) -> Option<&'static str> {
    let lowered = raw.trim().to_ascii_lowercase().replace('-', "_");
    let alias = match lowered.as_str() {
        "return" | "start" => "enter",
        "esc" => "escape",
        "control" | "lctrl" | "rctrl" => "ctrl",
        "lshift" | "rshift" => "shift",
        "lalt" | "ralt" => "alt",
        "pgup" | "pageup" => "page_up",
        "pgdn" | "pagedown" => "page_down",
        "left_click" => "mouse_left",
        "right_click" => "mouse_right",
        "numpad0" | "kp_0" | "0 (pad)" => "kp0",
        "numpad1" | "kp_1" | "1 (pad)" => "kp1",
        "numpad2" | "kp_2" | "2 (pad)" => "kp2",
        "numpad3" | "kp_3" | "3 (pad)" => "kp3",
        "numpad4" | "kp_4" | "4 (pad)" => "kp4",
        "numpad5" | "kp_5" | "5 (pad)" => "kp5",
        "numpad6" | "kp_6" | "6 (pad)" => "kp6",
        "numpad7" | "kp_7" | "7 (pad)" => "kp7",
        "numpad8" | "kp_8" | "8 (pad)" => "kp8",
        "numpad9" | "kp_9" | "9 (pad)" => "kp9",
        other => other,
    };
    INPUT_BUTTONS
        .iter()
        .copied()
        .find(|button| *button == alias)
}

pub(super) fn key_ids(buttons: &BTreeSet<String>) -> BTreeSet<u32> {
    buttons.iter().filter_map(|button| key_id(button)).collect()
}

pub(super) fn mouse_button_mask(buttons: &BTreeSet<String>) -> u8 {
    u8::from(buttons.contains("mouse_left")) | (u8::from(buttons.contains("mouse_right")) << 1)
}

pub(super) fn key_id(button: &str) -> Option<u32> {
    let byte = button.as_bytes();
    if byte.len() == 1 && (byte[0].is_ascii_lowercase() || byte[0].is_ascii_digit()) {
        return Some(u32::from(byte[0]));
    }
    match button {
        "backspace" => Some(8),
        "tab" => Some(9),
        "enter" => Some(13),
        "escape" => Some(27),
        "space" => Some(32),
        "delete" => Some(127),
        "kp0" => Some(256),
        "kp1" => Some(257),
        "kp2" => Some(258),
        "kp3" => Some(259),
        "kp4" => Some(260),
        "kp5" => Some(261),
        "kp6" => Some(262),
        "kp7" => Some(263),
        "kp8" => Some(264),
        "kp9" => Some(265),
        "kp_period" => Some(266),
        "kp_divide" => Some(267),
        "kp_multiply" => Some(268),
        "kp_minus" => Some(269),
        "kp_plus" => Some(270),
        "kp_enter" => Some(271),
        "up" => Some(273),
        "down" => Some(274),
        "right" => Some(275),
        "left" => Some(276),
        "insert" => Some(277),
        "home" => Some(278),
        "end" => Some(279),
        "page_up" => Some(280),
        "page_down" => Some(281),
        "f1" => Some(282),
        "f2" => Some(283),
        "f3" => Some(284),
        "f4" => Some(285),
        "f5" => Some(286),
        "f6" => Some(287),
        "f7" => Some(288),
        "f8" => Some(289),
        "f9" => Some(290),
        "f10" => Some(291),
        "f11" => Some(292),
        "f12" => Some(293),
        "shift" => Some(304),
        "ctrl" => Some(306),
        "alt" => Some(308),
        _ => None,
    }
}

pub(super) fn require_port_zero(params: &Value) -> Np2kaiResult<()> {
    match optional_num(params, "port")? {
        None | Some(0) => Ok(()),
        Some(port) => Err(Np2kaiError::BadParams(format!(
            "NP2kai input supports port 0 only, got {port}"
        ))),
    }
}

pub(super) fn require_frame_unit(params: &Value) -> Np2kaiResult<()> {
    match params.get("unit").and_then(Value::as_str) {
        None | Some("frames" | "frame") => Ok(()),
        Some(unit) => Err(Np2kaiError::BadParams(format!(
            "NP2kai execution control supports frame units only, got {unit}"
        ))),
    }
}

pub(super) fn frame_count(params: &Value) -> Np2kaiResult<u64> {
    let count = optional_num(params, "n")?
        .or(optional_num(params, "count")?)
        .or(optional_num(params, "frames")?)
        .unwrap_or(1);
    if (1..=MAX_SYNC_FRAMES).contains(&count) {
        Ok(count)
    } else {
        Err(Np2kaiError::BadParams(format!(
            "frame count must be in 1..={MAX_SYNC_FRAMES}, got {count}"
        )))
    }
}

fn parse_num(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(value) => crate::numparse::parse_num_str(value).ok(),
        _ => None,
    }
}

pub(super) fn optional_num(params: &Value, key: &str) -> Np2kaiResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| Np2kaiError::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

pub(super) fn required_signed_num(params: &Value, key: &str) -> Np2kaiResult<i64> {
    let value = params
        .get(key)
        .ok_or_else(|| Np2kaiError::BadParams(format!("missing required param: {key}")))?;
    let parsed = match value {
        Value::Number(number) => number.as_i64(),
        Value::String(raw) => raw.parse::<i64>().ok(),
        _ => None,
    };
    parsed.ok_or_else(|| Np2kaiError::BadParams(format!("invalid signed numeric param: {key}")))
}

pub(super) fn absolute_path_param(params: &Value, key: &str) -> Np2kaiResult<PathBuf> {
    let raw = params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Np2kaiError::BadParams(format!("missing required param: {key}")))?;
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(Np2kaiError::BadParams(format!(
            "{key} must be an absolute path"
        )));
    }
    Ok(path)
}

pub(super) fn path_cstring(path: &Path) -> Np2kaiResult<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| Np2kaiError::BadParams(format!("path contains NUL: {}", path.display())))
}

pub(super) fn sha256_file(path: &Path) -> Np2kaiResult<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(super) fn atomic_write(path: &Path, data: &[u8]) -> Np2kaiResult<()> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            Np2kaiError::BadParams(format!("invalid output path: {}", path.display()))
        })?;
    let partial = path.with_file_name(format!(".{name}.partial-{}", std::process::id()));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let result = (|| -> Np2kaiResult<()> {
        let mut file = fs::File::create(&partial)?;
        file.write_all(data)?;
        file.sync_all()?;
        if path.is_file() {
            fs::remove_file(path)?;
        } else if path.exists() {
            return Err(Np2kaiError::BadParams(format!(
                "output path is not a regular file: {}",
                path.display()
            )));
        }
        fs::rename(&partial, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

pub(super) fn state_sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".emucap.json");
    PathBuf::from(name)
}

pub(super) fn error_kind(error: &Np2kaiError) -> &'static str {
    match error {
        Np2kaiError::BadParams(_) => "bad_params",
        Np2kaiError::BadState(_) => "bad_state",
        Np2kaiError::Unsupported(_) => "unsupported",
        Np2kaiError::Core(_) => "emulator_error",
        Np2kaiError::Dynamic(_) | Np2kaiError::Io(_) | Np2kaiError::Json(_) => "adapter_error",
    }
}
