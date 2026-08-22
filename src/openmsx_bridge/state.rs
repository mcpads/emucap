use std::fs;

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    state_path, tcl_utf8_value, BridgeResult, OpenMsxBridge, OpenMsxBridgeError, OpenMsxControl,
};

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub(super) fn save_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("save_state")?;
        let path = state_path(params)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let path_var = tcl_utf8_value(&path)?;
        self.control.command(&format!(
            "set emucap_path {path_var}; store_machine [machine] $emucap_path"
        ))?;
        if !path.is_file() {
            return Err(OpenMsxBridgeError::Emulator(format!(
                "openMSX did not create savestate {}",
                path.display()
            )));
        }
        Ok(json!({
            "status": "completed",
            "saved": path.display().to_string(),
            "state": "frozen",
        }))
    }

    pub(super) fn load_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("load_state")?;
        let path = state_path(params)?;
        if !path.is_file() {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "savestate does not exist: {}",
                path.display()
            )));
        }
        let path_var = tcl_utf8_value(&path)?;
        self.control.command(&format!(
            "set emucap_path {path_var}; set newID [restore_machine $emucap_path]; \
             set oldID [machine]; if {{$oldID ne \"\"}} {{delete_machine $oldID}}; \
             activate_machine $newID; set pause on"
        ))?;
        self.control.command("debug break")?;
        self.control.command("set pause on")?;
        self.require_stop_conjunction("load_state")?;
        if let Err(error) = self.require_runtime_identity("load_state") {
            return self.fail_debugger(error.to_string());
        }
        let held = self.held_buttons.clone();
        self.release_supported_keys()?;
        self.press_key_set(&held)?;
        self.reapply_joystick_owners()?;
        self.reconcile_breakpoints("load_state")?;
        if let Err(error) = self.rebind_frame_monitor_after_machine_load() {
            return self.fail_debugger(format!(
                "MSX native frame monitor identity changed during load_state: {error}"
            ));
        }
        Ok(json!({
            "status": "completed",
            "loaded": path.display().to_string(),
            "state": "frozen",
            "frame": self.current_frame()?,
        }))
    }

    pub(super) fn reset(&mut self) -> BridgeResult<Value> {
        let held = self.held_buttons.clone();
        self.release_supported_keys()?;
        self.control.command("reset; set pause on")?;
        self.control.command("debug break")?;
        self.control.command("set pause on")?;
        self.require_stop_conjunction("reset")?;
        if let Err(error) = self.require_runtime_identity("reset") {
            return self.fail_debugger(error.to_string());
        }
        self.press_key_set(&held)?;
        self.reapply_joystick_owners()?;
        self.reconcile_breakpoints("reset")?;
        if let Err(error) = self.reconcile_frame_monitor() {
            return self.fail_debugger(format!(
                "MSX native frame monitor identity changed during reset: {error}"
            ));
        }
        Ok(json!({
            "status": "completed",
            "state": "frozen",
            "frame": self.current_frame()?,
        }))
    }

    pub(super) fn screenshot(&mut self) -> BridgeResult<Value> {
        self.require_frozen("screenshot")?;
        let before = self.current_frame()?;
        self.screenshot_sequence += 1;
        let directory = self.runtime_home.join("screenshots");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!(
            "capture-{}-{}.png",
            std::process::id(),
            self.screenshot_sequence
        ));
        let result = (|| {
            let path_var = tcl_utf8_value(&path)?;
            self.control.command(&format!(
                "set emucap_path {path_var}; screenshot -raw -size 320 $emucap_path"
            ))?;
            let png = crate::path_safety::read_bounded_regular_file_no_follow(
                &path,
                crate::live::protocol::MAX_INLINE_SCREENSHOT_BYTES,
            )?;
            if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
                return Err(OpenMsxBridgeError::Protocol(
                    "openMSX screenshot is not a complete PNG".into(),
                ));
            }
            let after = self.current_frame()?;
            if after != before {
                return Err(OpenMsxBridgeError::Emulator(format!(
                    "openMSX screenshot advanced guest time: {before} -> {after}"
                )));
            }
            let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
            let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
            let sha256 = hex::encode(Sha256::digest(&png));
            Ok(json!({
                "png_base64": base64::engine::general_purpose::STANDARD.encode(&png),
                "sha256": sha256,
                "byte_len": png.len(),
                "width": width,
                "height": height,
                "frame": before,
                "frame_before": before,
                "frame_after": after,
                "frame_stable": true,
                "state": "frozen",
                "freshness": "current_screen",
            }))
        })();
        let _ = fs::remove_file(path);
        result
    }
}
