use super::*;

use sha2::{Digest, Sha256};

mod container;
mod jobs;
mod probe;

use container::*;
use jobs::*;

pub(super) use container::MediaIdentityCache;

pub(super) const STATE_FORMAT: &str = "emucap-xemu-state";

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn save_state(&mut self, params: &Value) -> XemuResult<Value> {
        self.require_frozen_state_operation("save_state")?;
        let path = self.state_path(params)?;
        self.require_complete_machine_identity()?;
        let layout = self.block_layout()?;
        let media = self.current_media_identity()?;
        let eeprom = self.read_eeprom()?;
        let previous = self.read_replaced_container(&path, &layout)?;
        let snapshot_tag = self.next_snapshot_tag();

        self.run_snapshot_job("snapshot-save", &snapshot_tag, &layout.hdd_node)?;
        if self.is_running()? {
            let cleanup = self.delete_snapshot(&snapshot_tag, &layout.hdd_node);
            return Err(XemuBridgeError::Emulator(format!(
                "xemu snapshot-save resumed a VM that was frozen before the request{}",
                cleanup
                    .err()
                    .map(|error| format!("; snapshot cleanup also failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        let frame = self.extension_status()?["frame-boundary"].clone();
        let container = self.state_container(snapshot_tag.clone(), &layout, media, &eeprom)?;
        let bytes = serde_json::to_vec_pretty(&container)?;
        if let Err(error) = crate::path_safety::atomic_write_file(&path, &bytes) {
            let cleanup = self.delete_snapshot(&snapshot_tag, &layout.hdd_node);
            return Err(XemuBridgeError::Emulator(format!(
                "failed to publish Xbox state container: {error}{}",
                cleanup
                    .err()
                    .map(|cleanup| format!("; snapshot cleanup also failed: {cleanup}"))
                    .unwrap_or_default()
            )));
        }

        if let Some((old, old_bytes)) = previous {
            if old.launch_id == container.launch_id
                && old.storage.hdd_path == container.storage.hdd_path
                && old.storage.hdd_node == container.storage.hdd_node
            {
                if let Err(cleanup_error) =
                    self.delete_snapshot(&old.storage.snapshot_tag, &layout.hdd_node)
                {
                    let restore = crate::path_safety::atomic_write_file(&path, &old_bytes);
                    let discard = self.delete_snapshot(&snapshot_tag, &layout.hdd_node);
                    return Err(XemuBridgeError::Emulator(format!(
                        "failed to replace the prior Xbox internal snapshot: {cleanup_error}; previous container restore={} new snapshot cleanup={}",
                        outcome_label(restore),
                        outcome_label(discard)
                    )));
                }
            }
        }

        Ok(json!({
            "status":"completed",
            "state":"frozen",
            "path":path.display().to_string(),
            "format":STATE_FORMAT,
            "scope":"same_generation",
            "launch_id":container.launch_id,
            "frame":frame,
            "boundary":"frame_boundary",
            "sha256":hex::encode(Sha256::digest(&bytes)),
            "bytes":bytes.len(),
            "media_boundary":"exact_current_disc_required_on_load",
        }))
    }

    pub(super) fn load_state(&mut self, params: &Value) -> XemuResult<Value> {
        self.require_frozen_state_operation("load_state")?;
        let path = self.state_path(params)?;
        self.require_complete_machine_identity()?;
        let bytes = crate::path_safety::read_bounded_regular_file_no_follow(
            &path,
            MAX_STATE_CONTAINER_BYTES,
        )?;
        let container_sha256 = hex::encode(Sha256::digest(&bytes));
        let container: XemuStateContainer = serde_json::from_slice(&bytes).map_err(|error| {
            XemuBridgeError::BadParams(format!(
                "invalid Xbox state container at {}: {error}",
                path.display()
            ))
        })?;
        let layout = self.block_layout()?;
        let saved_eeprom = self.validate_container(&container, &layout)?;
        let previous_eeprom = self.read_eeprom()?;
        let previous_events = self.events.clone();
        let previous_debug_stop = self.debug_stop_observed;
        let rollback_tag = self.next_snapshot_tag();
        self.run_snapshot_job("snapshot-save", &rollback_tag, &layout.hdd_node)?;

        let load_result = (|| {
            self.run_snapshot_job(
                "snapshot-load",
                &container.storage.snapshot_tag,
                &layout.hdd_node,
            )?;
            if self.is_running()? {
                return Err(XemuBridgeError::Emulator(
                    "xemu snapshot-load did not preserve the frozen state".into(),
                ));
            }
            crate::path_safety::atomic_write_file(&self.state_environment.eeprom, &saved_eeprom)?;
            self.reconcile_state_load_debugger()?;
            self.reapply_state_load_input()?;
            let observed_layout = self.block_layout()?;
            if observed_layout != layout {
                return Err(XemuBridgeError::Emulator(
                    "xemu writable block topology changed during state load".into(),
                ));
            }
            if self.current_media_identity()? != container.media {
                return Err(XemuBridgeError::Emulator(
                    "xemu current disc identity changed during state load".into(),
                ));
            }
            let extension = self.extension_status()?;
            let cpu = self.read_cpu_state()?;
            Ok((extension["frame-boundary"].clone(), cpu))
        })();

        let (frame, cpu) = match load_result {
            Ok(result) => result,
            Err(load_error) => {
                let recovery = self.recover_failed_state_load(
                    &rollback_tag,
                    &layout,
                    &previous_eeprom,
                    previous_events,
                    previous_debug_stop,
                );
                return match recovery {
                    Ok(()) => Err(XemuBridgeError::Emulator(format!(
                        "Xbox state load failed and the prior frozen state was restored: {load_error}"
                    ))),
                    Err(recovery_error) => {
                        let reason = format!(
                            "state load failed ({load_error}); rollback also failed ({recovery_error})"
                        );
                        self.state_integrity_error = Some(reason.clone());
                        Err(XemuBridgeError::BadState(reason))
                    }
                };
            }
        };

        let cleanup = match self.delete_snapshot(&rollback_tag, &layout.hdd_node) {
            Ok(()) => json!({"rollback_snapshot":"deleted"}),
            Err(error) => {
                self.pending_state_snapshot_cleanup
                    .push((rollback_tag, layout.hdd_node.clone()));
                json!({
                    "rollback_snapshot":"deferred_until_next_state_operation_or_generation_stop",
                    "warning":error.to_string(),
                })
            }
        };
        self.events.clear();
        self.debug_stop_observed = false;
        Ok(json!({
            "status":"completed",
            "state":"frozen",
            "path":path.display().to_string(),
            "format":STATE_FORMAT,
            "sha256":container_sha256,
            "scope":"same_generation",
            "launch_id":container.launch_id,
            "frame":frame,
            "frame_counter_continuous":false,
            "media_boundary":"exact_current_disc_preserved",
            "backend_round_trip_verified":true,
            "control_serviceable":true,
            "cleanup":cleanup,
            "cpu":cpu,
        }))
    }

    fn require_frozen_state_operation(&mut self, operation: &str) -> XemuResult<()> {
        self.drain_gdb_stops(true)?;
        if self.is_running()? {
            return Err(XemuBridgeError::BadState(format!(
                "Xbox {operation} requires a frozen VM; pause first"
            )));
        }
        self.retry_pending_snapshot_cleanup();
        Ok(())
    }

    fn reconcile_state_load_debugger(&mut self) -> XemuResult<()> {
        for breakpoint in self.breakpoints.values().cloned().collect::<Vec<_>>() {
            let clear = self.gdb_command(&format!(
                "z{},{:x},{:x}",
                breakpoint.ztype, breakpoint.absolute, breakpoint.length
            ))?;
            if clear != "OK" && clear != "E00" {
                return Err(XemuBridgeError::Emulator(format!(
                    "GDB breakpoint reconciliation remove failed: {clear}"
                )));
            }
            let set = self.gdb_command(&format!(
                "Z{},{:x},{:x}",
                breakpoint.ztype, breakpoint.absolute, breakpoint.length
            ))?;
            if set != "OK" {
                return Err(XemuBridgeError::Emulator(format!(
                    "GDB breakpoint reconciliation set failed: {set}"
                )));
            }
        }
        self.drain_gdb_stops(false)?;
        Ok(())
    }

    fn reapply_state_load_input(&mut self) -> XemuResult<()> {
        if let Some(input) = self.held_input.clone() {
            self.send_input(true, &input)
        } else {
            let neutral = InputState::default();
            self.send_input(true, &neutral)?;
            self.send_input(false, &neutral)
        }
    }

    fn recover_failed_state_load(
        &mut self,
        rollback_tag: &str,
        layout: &BlockLayout,
        previous_eeprom: &[u8],
        previous_events: Vec<Value>,
        previous_debug_stop: bool,
    ) -> XemuResult<()> {
        self.run_snapshot_job("snapshot-load", rollback_tag, &layout.hdd_node)?;
        crate::path_safety::atomic_write_file(&self.state_environment.eeprom, previous_eeprom)?;
        self.reconcile_state_load_debugger()?;
        self.reapply_state_load_input()?;
        if self.is_running()? {
            return Err(XemuBridgeError::Emulator(
                "rollback snapshot did not preserve the frozen state".into(),
            ));
        }
        self.read_cpu_state()?;
        self.delete_snapshot(rollback_tag, &layout.hdd_node)?;
        self.events = previous_events;
        self.debug_stop_observed = previous_debug_stop;
        self.state_integrity_error = None;
        Ok(())
    }
}

fn outcome_label<T, E: std::fmt::Display>(outcome: Result<T, E>) -> String {
    match outcome {
        Ok(_) => "completed".into(),
        Err(error) => format!("failed ({error})"),
    }
}
