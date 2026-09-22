use std::fs;

use serde_json::{json, Value};

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
        self.require_runtime_identity("save_state")?;
        if self.session.media.kind == super::MediaKind::Disk {
            let scratch = super::disk_state::StateScratch::new(&self.session)?;
            let machine = tcl_utf8_value(&scratch.machine())?;
            let disk = tcl_utf8_value(&scratch.disk())?;
            self.control.command(&format!(
                "store_machine [machine] {machine}; diskmanipulator savedsk diska {disk}"
            ))?;
            super::disk_state::publish(&path, &self.session, &scratch)?;
        } else {
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
        // Validate payloads before changing anything in the native machine.
        let mut scratch = if self.session.media.kind == super::MediaKind::Disk {
            Some(super::disk_state::prepare(&path, &self.session)?)
        } else {
            None
        };
        if let Err(error) = self.require_runtime_identity("load_state original") {
            return self.fail_debugger(error.to_string());
        }
        self.reconcile_breakpoints("load_state original")?;
        self.reconcile_frame_monitor()?;
        let native_path = scratch
            .as_ref()
            .map(|s| s.machine())
            .unwrap_or_else(|| path.clone());
        let path_var = tcl_utf8_value(&native_path)?;
        let disk_arg = scratch
            .as_ref()
            .map(|s| tcl_utf8_value(&s.disk()))
            .transpose()?;
        let original_media = self.session.media.mounted_path.clone();
        let original_probe = self.frame_probe_native_id.clone();
        self.control.command(&format!(
            "set emucap_path {path_var}; set oldID [machine]; \
             set newID [restore_machine $emucap_path -emucap{}]",
            disk_arg
                .map(|value| format!(" {value}"))
                .unwrap_or_default()
        ))?;
        if let Some(scratch) = &scratch {
            self.session.media.mounted_path = scratch.disk();
        }
        let restored = (|| {
            self.control
                .command("activate_machine $newID; set pause on")?;
            self.control.command("debug break")?;
            self.require_stop_conjunction("load_state")?;
            self.require_runtime_identity("load_state")?;
            self.restore_input_owners()?;
            self.reconcile_breakpoints("load_state")?;
            self.rebind_frame_monitor_after_machine_load()?;
            self.current_frame()
        })();
        let frame = match restored {
            Ok(frame) => frame,
            Err(primary) => {
                self.session.media.mounted_path = original_media;
                self.frame_probe_native_id = original_probe;
                self.debugger_fatal = None;
                let rollback: BridgeResult<()> = (|| {
                    self.control
                        .command("activate_machine $oldID; set pause on; debug break")?;
                    self.require_stop_conjunction("load_state rollback")?;
                    self.require_runtime_identity("load_state rollback")?;
                    self.restore_input_owners()?;
                    self.reconcile_breakpoints("load_state rollback")?;
                    self.reconcile_frame_monitor()?;
                    self.control.command("delete_machine $newID")?;
                    Ok(())
                })();
                if let Err(rollback) = rollback {
                    // The native owner may still be alive until terminal shutdown.
                    if let Some(scratch) = &mut scratch {
                        scratch.retain();
                    }
                    return self.fail_debugger(format!(
                        "load_state failed ({primary}); rollback could not be verified ({rollback})"
                    ));
                }
                return Err(OpenMsxBridgeError::Emulator(format!(
                    "load_state rejected; original frozen machine retained: {primary}"
                )));
            }
        };
        if let Err(error) = self.control.command("delete_machine $oldID") {
            if let Some(scratch) = &mut scratch {
                scratch.retain();
            }
            return self.fail_debugger(format!(
                "load_state commit could not delete original machine: {error}"
            ));
        }
        self.restored_state = scratch;
        self.capture_epoch = ulid::Ulid::generate().to_string();
        Ok(json!({
            "status": "completed", "loaded": path.display().to_string(),
            "state": "frozen", "frame": frame,
        }))
    }

    fn restore_input_owners(&mut self) -> BridgeResult<()> {
        let held = self.held_buttons.clone();
        self.release_supported_keys()?;
        self.press_key_set(&held)?;
        self.reapply_joystick_owners()
    }

    pub(super) fn reset(&mut self) -> BridgeResult<Value> {
        self.capture_epoch = ulid::Ulid::generate().to_string();
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
}
