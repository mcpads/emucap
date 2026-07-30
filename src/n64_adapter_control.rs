//! Input, screenshot, and native state operations for the N64 frontend.

use std::collections::BTreeSet;
use std::ffi::{c_int, c_void, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::*;

impl Mupen64PlusHost {
    pub(super) fn set_input(&mut self, params: &Value) -> N64Result<Value> {
        require_port_zero(params)?;
        self.require_connected()?;
        let buttons = normalize_buttons(params.get("buttons"))?;
        self.apply_input_buttons(&buttons)?;
        Ok(json!({
            "status": "completed",
            "buttons": self.held_buttons.iter().collect::<Vec<_>>(),
            "override_engaged": !self.held_buttons.is_empty(),
            "ownership": if self.held_buttons.is_empty() {
                "native"
            } else {
                "shared_with_native"
            }
        }))
    }

    pub(super) fn press_buttons(&mut self, params: &Value) -> N64Result<Value> {
        require_port_zero(params)?;
        self.require_connected()?;
        let pulse = normalize_buttons(params.get("buttons"))?;
        if pulse.is_empty() {
            return Err(N64Error::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1);
        if !(1..=MAX_INPUT_PULSE_FRAMES).contains(&frames) {
            return Err(N64Error::BadParams(format!(
                "press_buttons frames must be in 1..={MAX_INPUT_PULSE_FRAMES}, got {frames}"
            )));
        }

        let was_frozen = self.is_frozen_boundary();
        if !was_frozen {
            self.pause(&json!({"cpu":"r4300"}))?;
        }
        let persistent = self.held_buttons.clone();
        let active = persistent.union(&pulse).cloned().collect::<BTreeSet<_>>();
        let outcome = (|| {
            self.apply_input_buttons(&active)?;
            self.step(&json!({"frames":frames, "cpu":"r4300"}))
        })();
        let release = self.apply_input_buttons(&persistent);
        if let Err(error) = release {
            return Err(self.stop_generation_with_unresolved_effect("input-pulse cleanup", &error));
        }
        let stepped = match outcome {
            Ok(stepped) => stepped,
            Err(error) => {
                if !was_frozen {
                    self.resume(&json!({"cpu":"r4300"}))?;
                }
                return Err(error);
            }
        };
        if stepped.get("status").and_then(Value::as_str) == Some("interrupted") {
            return Ok(json!({
                "status":"interrupted",
                "reason":"breakpoint",
                "breakpoint_id":stepped["breakpoint_id"],
                "event":stepped["event"],
                "buttons":pulse.iter().collect::<Vec<_>>(),
                "frames":frames,
                "persistent_buttons":self.held_buttons.iter().collect::<Vec<_>>(),
                "transient_override_engaged":false,
                "state":"frozen",
            }));
        }
        if !was_frozen {
            self.resume(&json!({"cpu":"r4300"}))?;
        }
        Ok(json!({
            "status": "completed",
            "buttons": pulse.iter().collect::<Vec<_>>(),
            "frames": frames,
            "state": if was_frozen { "frozen" } else { "running" },
            "persistent_buttons": self.held_buttons.iter().collect::<Vec<_>>(),
            "transient_override_engaged": false,
            "frame": stepped["frame"],
        }))
    }

    pub(super) fn screenshot(&mut self) -> N64Result<Value> {
        self.require_frozen("screenshot")?;
        let directory = self.runtime_home.join("screens");
        let before_paths = png_paths(&directory)?;
        let frame_before = self.public_frame();
        SCREENSHOT_RESULT.store(-1, Ordering::Release);
        check_core("TAKE_NEXT_SCREENSHOT", unsafe {
            (self.api.core_do_command)(M64CMD_TAKE_NEXT_SCREENSHOT, 0, ptr::null_mut())
        })?;
        let stepped = match self.step_with_frame_gate(
            &json!({"frames":1u64, "cpu":"r4300"}),
            FrameGateTrigger::ScreenshotCompleted,
        ) {
            Ok(stepped) => stepped,
            Err(error) => {
                return Err(self.stop_generation_with_unresolved_effect("screenshot", &error))
            }
        };
        if let Err(error) = wait_operation_result(&SCREENSHOT_RESULT, "screenshot completion") {
            if matches!(error, N64Error::Timeout(_)) {
                return Err(self.stop_generation_with_unresolved_effect("screenshot", &error));
            }
            return Err(error);
        }
        let after_paths = png_paths(&directory)?;
        let created = after_paths
            .difference(&before_paths)
            .cloned()
            .collect::<Vec<_>>();
        if created.len() != 1 {
            return Err(N64Error::BadState(format!(
                "Mupen64Plus screenshot completed but produced {} new PNG files",
                created.len()
            )));
        }
        let path = &created[0];
        let data = fs::read(path)?;
        let remove_result = fs::remove_file(path);
        let (width, height) = png_dimensions(&data)?;
        remove_result?;
        let sha256 = hex::encode(Sha256::digest(&data));
        Ok(json!({
            "png_base64": base64::engine::general_purpose::STANDARD.encode(&data),
            "sha256": sha256,
            "byte_len": data.len(),
            "width": width,
            "height": height,
            "frame_before": frame_before,
            "frame_after": stepped["frame"],
            "frame_stable": false,
            "state": "frozen",
            "freshness": "current",
            "capture_boundary": "first_callback_after_screenshot_completion",
        }))
    }

    pub(super) fn save_state(&mut self, params: &Value) -> N64Result<Value> {
        self.require_frozen("save_state")?;
        let path = absolute_requested_path(params, "path")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let partial = state_partial_sibling(&path)?;
        let _ = fs::remove_file(&partial);
        let frame_before = self.public_frame();
        let result = (|| {
            STATE_SAVE_RESULT.store(-1, Ordering::Release);
            let path_c = path_cstring(&partial)?;
            check_core("STATE_SAVE", unsafe {
                (self.api.core_do_command)(M64CMD_STATE_SAVE, 1, path_c.as_ptr() as *mut c_void)
            })?;
            let stepped = match self.step(&json!({"frames":1u64, "cpu":"r4300"})) {
                Ok(stepped) => stepped,
                Err(error) => {
                    return Err(self.stop_generation_with_unresolved_effect("save_state", &error))
                }
            };
            if let Err(error) = wait_operation_result(&STATE_SAVE_RESULT, "save-state completion") {
                if matches!(error, N64Error::Timeout(_)) {
                    return Err(self.stop_generation_with_unresolved_effect("save_state", &error));
                }
                return Err(error);
            }
            let metadata = fs::metadata(&partial)?;
            if metadata.len() == 0 {
                return Err(N64Error::BadState(
                    "Mupen64Plus save completed with an empty state file".into(),
                ));
            }
            crate::launch::copy_file_replace(&partial, &path)?;
            fs::remove_file(&partial)?;
            let data = fs::read(&path)?;
            Ok(json!({
                "status": "completed",
                "path": path.display().to_string(),
                "format": "mupen64plus-native",
                "bytes": data.len(),
                "sha256": hex::encode(Sha256::digest(&data)),
                "state": "frozen",
                "frame_before": frame_before,
                "frame": stepped["frame"],
            }))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result
    }

    pub(super) fn load_state(&mut self, params: &Value) -> N64Result<Value> {
        self.require_frozen("load_state")?;
        let path = absolute_requested_path(params, "path")?;
        if !path.is_file() || fs::metadata(&path)?.len() == 0 {
            return Err(N64Error::BadParams(format!(
                "N64 save state not found or empty: {}",
                path.display()
            )));
        }
        let frame_before = self.public_frame();
        STATE_LOAD_RESULT.store(-1, Ordering::Release);
        self.frame_clock_synchronized = false;
        let path_c = path_cstring(&path)?;
        check_core("STATE_LOAD", unsafe {
            (self.api.core_do_command)(M64CMD_STATE_LOAD, 0, path_c.as_ptr() as *mut c_void)
        })?;
        let stepped = match self.step(&json!({"frames":1u64, "cpu":"r4300"})) {
            Ok(stepped) => stepped,
            Err(error) => {
                return Err(self.stop_generation_with_unresolved_effect("load_state", &error))
            }
        };
        if let Err(error) = wait_operation_result(&STATE_LOAD_RESULT, "load-state completion") {
            if matches!(error, N64Error::Timeout(_)) {
                return Err(self.stop_generation_with_unresolved_effect("load_state", &error));
            }
            return Err(error);
        }
        Ok(json!({
            "status": "completed",
            "path": path.display().to_string(),
            "format": "mupen64plus-native",
            "state": "frozen",
            "frame_before": frame_before,
            "frame": stepped["frame"],
            "frame_counter_continuous": false,
            "pc": LAST_PC.load(Ordering::Acquire),
        }))
    }

    fn apply_input_buttons(&mut self, desired: &BTreeSet<String>) -> N64Result<()> {
        let releases = self
            .held_buttons
            .difference(desired)
            .cloned()
            .collect::<Vec<_>>();
        let presses = desired
            .difference(&self.held_buttons)
            .cloned()
            .collect::<Vec<_>>();
        for button in releases {
            self.send_input_button(&button, false)?;
            self.held_buttons.remove(&button);
        }
        for button in presses {
            self.send_input_button(&button, true)?;
            self.held_buttons.insert(button);
        }
        Ok(())
    }

    pub(super) fn reapply_held_buttons_after_reset(&self) -> N64Result<()> {
        for button in &self.held_buttons {
            self.send_input_button(button, true)?;
        }
        Ok(())
    }

    fn send_input_button(&self, button: &str, pressed: bool) -> N64Result<()> {
        let key = input_key(button).ok_or_else(|| {
            N64Error::BadParams(format!("unsupported N64 input button: {button}"))
        })?;
        check_core(
            if pressed {
                "SEND_SDL_KEYDOWN"
            } else {
                "SEND_SDL_KEYUP"
            },
            unsafe {
                (self.api.core_do_command)(
                    if pressed {
                        M64CMD_SEND_SDL_KEYDOWN
                    } else {
                        M64CMD_SEND_SDL_KEYUP
                    },
                    key,
                    ptr::null_mut(),
                )
            },
        )
    }

    pub(super) fn is_frozen_boundary(&self) -> bool {
        let debugger_paused =
            unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) } == M64P_DBG_RUNSTATE_PAUSED;
        let frame_barrier_paused = self.frame_paused && frame_gate_is_blocked();
        self.frozen && (debugger_paused || frame_barrier_paused)
    }
}

pub(super) fn configure_input(api: &Api) -> N64Result<()> {
    let mut control = ptr::null_mut();
    check_core("ConfigOpenSection(Input-SDL-Control1)", unsafe {
        (api.config_open_section)(cstr(b"Input-SDL-Control1\0").as_ptr(), &mut control)
    })?;
    set_config_int(api, control, b"mode\0", M64TYPE_INT, 0)?;
    set_config_int(api, control, b"device\0", M64TYPE_INT, -1)?;
    set_config_text(api, control, b"name\0", "Keyboard")?;
    set_config_int(api, control, b"plugged\0", M64TYPE_BOOL, 1)?;
    set_config_int(api, control, b"plugin\0", M64TYPE_INT, 2)?;
    set_config_int(api, control, b"mouse\0", M64TYPE_BOOL, 0)?;
    for (name, value) in [
        (b"DPad R\0".as_slice(), "key(275)"),
        (b"DPad L\0".as_slice(), "key(276)"),
        (b"DPad D\0".as_slice(), "key(274)"),
        (b"DPad U\0".as_slice(), "key(273)"),
        (b"Start\0".as_slice(), "key(13)"),
        (b"Z Trig\0".as_slice(), "key(122)"),
        (b"B Button\0".as_slice(), "key(99)"),
        (b"A Button\0".as_slice(), "key(120)"),
        (b"C Button R\0".as_slice(), "key(108)"),
        (b"C Button L\0".as_slice(), "key(106)"),
        (b"C Button D\0".as_slice(), "key(107)"),
        (b"C Button U\0".as_slice(), "key(105)"),
        (b"R Trig\0".as_slice(), "key(101)"),
        (b"L Trig\0".as_slice(), "key(113)"),
        (b"Mempak switch\0".as_slice(), ""),
        (b"Rumblepak switch\0".as_slice(), ""),
        (b"X Axis\0".as_slice(), "key(97,100)"),
        (b"Y Axis\0".as_slice(), "key(119,115)"),
    ] {
        set_config_text(api, control, name, value)?;
    }

    for section_name in [
        b"Input-SDL-Control2\0".as_slice(),
        b"Input-SDL-Control3\0".as_slice(),
        b"Input-SDL-Control4\0".as_slice(),
    ] {
        let mut section = ptr::null_mut();
        check_core("ConfigOpenSection(Input-SDL-ControlN)", unsafe {
            (api.config_open_section)(cstr(section_name).as_ptr(), &mut section)
        })?;
        set_config_int(api, section, b"mode\0", M64TYPE_INT, 0)?;
        set_config_int(api, section, b"device\0", M64TYPE_INT, -1)?;
        set_config_int(api, section, b"plugged\0", M64TYPE_BOOL, 0)?;
        set_config_int(api, section, b"plugin\0", M64TYPE_INT, 2)?;
        set_config_int(api, section, b"mouse\0", M64TYPE_BOOL, 0)?;
    }
    Ok(())
}

fn set_config_text(
    api: &Api,
    section: *mut c_void,
    name: &'static [u8],
    value: &str,
) -> N64Result<()> {
    let value = CString::new(value)
        .map_err(|_| N64Error::BadParams("configuration value contains NUL".into()))?;
    check_core("ConfigSetParameter", unsafe {
        (api.config_set_parameter)(
            section,
            cstr(name).as_ptr(),
            M64TYPE_STRING,
            value.as_ptr() as *const c_void,
        )
    })
}

pub(super) fn operation_result(result: &AtomicI32, operation: &'static str) -> N64Result<()> {
    if result.load(Ordering::Acquire) == 1 {
        Ok(())
    } else {
        Err(N64Error::BadState(format!(
            "Mupen64Plus reported that {operation} failed"
        )))
    }
}

pub(super) fn wait_operation_result(result: &AtomicI32, operation: &'static str) -> N64Result<()> {
    wait_until(operation, COMPLETION_DEADLINE, || {
        result.load(Ordering::Acquire) != -1
    })?;
    operation_result(result, operation)
}

pub(super) fn require_port_zero(params: &Value) -> N64Result<()> {
    match optional_num(params, "port")? {
        None | Some(0) => Ok(()),
        Some(port) => Err(N64Error::BadParams(format!(
            "N64 input currently supports port 0 only, got {port}"
        ))),
    }
}

pub(super) fn normalize_buttons(value: Option<&Value>) -> N64Result<BTreeSet<String>> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| N64Error::BadParams("buttons must be an array".into()))?;
    values
        .iter()
        .map(|value| {
            let raw = value
                .as_str()
                .ok_or_else(|| N64Error::BadParams("button names must be strings".into()))?;
            let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
            let canonical = match normalized.as_str() {
                "shoulder_l" => "l",
                "shoulder_r" => "r",
                "analog_up" => "up",
                "analog_down" => "down",
                "analog_left" => "left",
                "analog_right" => "right",
                other => other,
            };
            if input_key(canonical).is_none() {
                return Err(N64Error::BadParams(format!(
                    "unsupported N64 input button: {raw}"
                )));
            }
            Ok(canonical.to_string())
        })
        .collect()
}

pub(super) fn input_key(button: &str) -> Option<c_int> {
    Some(match button {
        "a" => 27,
        "b" => 6,
        "z" => 29,
        "start" => 40,
        "l" => 20,
        "r" => 8,
        "up" => 26,
        "down" => 22,
        "left" => 4,
        "right" => 7,
        "dpad_up" => 82,
        "dpad_down" => 81,
        "dpad_left" => 80,
        "dpad_right" => 79,
        "c_up" => 12,
        "c_down" => 14,
        "c_left" => 13,
        "c_right" => 15,
        _ => return None,
    })
}

fn png_paths(directory: &Path) -> N64Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("png"))
        {
            paths.insert(path);
        }
    }
    Ok(paths)
}

pub(super) fn png_dimensions(data: &[u8]) -> N64Result<(u32, u32)> {
    if data.len() < 24
        || !data.starts_with(b"\x89PNG\r\n\x1a\n")
        || data.get(12..16) != Some(b"IHDR")
    {
        return Err(N64Error::BadState(
            "Mupen64Plus screenshot is not a valid PNG header".into(),
        ));
    }
    let width = u32::from_be_bytes(data[16..20].try_into().expect("PNG width"));
    let height = u32::from_be_bytes(data[20..24].try_into().expect("PNG height"));
    if width == 0 || height == 0 {
        return Err(N64Error::BadState(
            "Mupen64Plus screenshot has zero dimensions".into(),
        ));
    }
    Ok((width, height))
}

fn absolute_requested_path(params: &Value, key: &str) -> N64Result<PathBuf> {
    let requested = params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| N64Error::BadParams(format!("missing or invalid param: {key}")))?;
    if requested.is_absolute() {
        Ok(requested)
    } else {
        Ok(std::env::current_dir()?.join(requested))
    }
}

pub(super) fn state_partial_sibling(path: &Path) -> N64Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        N64Error::BadParams(format!(
            "save path has no parent directory: {}",
            path.display()
        ))
    })?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    Ok(parent.join(format!(".{name}.partial.{}.{nanos}", std::process::id())))
}
