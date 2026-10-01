use serde_json::{json, Value};

use super::{BridgeResult, OpenMsxBridge, OpenMsxBridgeError, OpenMsxControl};

const FRAME_PROBE: &str = "emucap.vdp_frame_boundary";
const FRAME_CALLBACK: &str = "::emucap::frame_tick";

const FRAME_TCL: &str = r#"namespace eval ::emucap {
    variable frame_seq 0
    variable frame_target {}
    variable frame_generation 0

    proc next_frame {count} {
        variable frame_generation
        incr frame_generation
        after realtime 0 [list ::emucap::start_frame $count $frame_generation]
        return {}
    }

    proc start_frame {count generation} {
        variable frame_generation
        if {$generation != $frame_generation} { return {} }
        variable frame_seq
        variable frame_target
        if {$frame_target ne {}} {
            error "an emucap frame target is already armed"
        }
        set frame_target [expr {$frame_seq + $count}]
        set ::pause off
        debug cont
    }

    proc frame_tick {} {
        variable frame_seq
        variable frame_target
        variable frame_generation
        incr frame_seq
        if {$frame_target ne {} && $frame_seq >= $frame_target} {
            incr frame_generation
            set frame_target {}
            set ::pause on
        }
    }

    proc cancel_frame {} {
        variable frame_target
        variable frame_generation
        incr frame_generation
        set frame_target {}
        return {}
    }

    proc frame_seq {} {
        variable frame_seq
        return $frame_seq
    }

    proc frame_probe_inventory {} {
        set result {}
        foreach row [split [debug probe list_bp] "\n"] {
            if {$row eq {}} {
                continue
            }
            if {[lindex $row 1] eq {emucap.vdp_frame_boundary}} {
                append result [join $row |] "\n"
            }
        }
        return [binary encode hex $result]
    }

    proc frame_debug {} {
        variable frame_seq
        variable frame_target
        return [list $frame_seq $frame_target [set ::pause] [debug breaked] \
            [::emucap::frame_probe_inventory]]
    }
}"#;

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub(super) fn frame_step(&mut self, count: u64) -> BridgeResult<Value> {
        self.require_frozen("frame step")?;
        self.prepare_temporal_request("frame step")?;
        let before = self.current_frame()?;
        if let Err(primary) = self
            .control
            .advance_frames_cancellable(count, &self.request_cancellation)
        {
            if matches!(primary, OpenMsxBridgeError::HostDeadline(_)) {
                return self.finish_interrupted_frame_step(before, count, "host_deadline");
            }
            if matches!(primary, OpenMsxBridgeError::Cancelled) {
                return self.finish_interrupted_frame_step(before, count, "cancelled");
            }
            let diagnostic = self
                .control
                .command("::emucap::frame_debug")
                .unwrap_or_else(|error| format!("unavailable: {error}"));
            let cleanup = self
                .control
                .command("::emucap::cancel_frame")
                .and_then(|_| self.control.command("set pause on"))
                .and_then(|_| self.control.command("debug break"))
                .and_then(|_| self.require_stop_conjunction("failed frame step cleanup"));
            return match cleanup {
                Ok(()) => Err(OpenMsxBridgeError::Emulator(format!(
                    "{primary}; frame diagnostic: {diagnostic}"
                ))),
                Err(cleanup) => self.fail_debugger(format!(
                    "{primary}; frame diagnostic: {diagnostic}; \
                     frame target cleanup also failed: {cleanup}"
                )),
            };
        }
        let after = self.current_frame()?;
        let dropped = self.drain_debug_events()?;
        if !self.debug_events.is_empty() || dropped != 0 {
            if let Err(error) = self.control.command("::emucap::cancel_frame") {
                return self.fail_debugger(format!(
                    "breakpoint interrupted frame step but target cleanup failed: {error}"
                ));
            }
            self.require_stop_conjunction("breakpoint-interrupted frame step")?;
            return Ok(json!({
                "status":"interrupted",
                "reason":"breakpoint",
                "unit":"frames",
                "count":after.saturating_sub(before),
                "requested":count,
                "frame_before":before,
                "frame":after,
                "state":"frozen",
                "event_pending":!self.debug_events.is_empty(),
            }));
        }
        if self.control.command("debug breaked")?.trim() == "1" {
            return self.fail_debugger(
                "openMSX entered CPU debug break without a valid callback event".into(),
            );
        }
        self.control.command("debug break")?;
        self.control.command("set pause on")?;
        self.require_stop_conjunction("frame step")?;
        if after != before + count {
            return Err(OpenMsxBridgeError::Emulator(format!(
                "openMSX frame step mismatch: expected {}, observed {after}",
                before + count
            )));
        }
        Ok(json!({
            "status": "completed",
            "unit": "frames",
            "count": count,
            "frame_before": before,
            "frame": after,
            "state": "frozen",
        }))
    }

    /// Slow pacing can make a frame target outlast the host budget. Stop at the reached boundary
    /// and report partial progress; an unverified stop is a debugger failure, not a result.
    fn finish_interrupted_frame_step(
        &mut self,
        before: u64,
        count: u64,
        reason: &str,
    ) -> BridgeResult<Value> {
        self.control.set_command_deadline(Some(
            std::time::Instant::now() + std::time::Duration::from_millis(500),
        ));
        let result: BridgeResult<Value> = (|| {
            self.control.command("::emucap::cancel_frame")?;
            self.control.command("set pause on")?;
            self.control.command("debug break")?;
            self.require_stop_conjunction("interrupted frame step")?;
            let after = self.current_frame()?;
            self.drain_debug_events()?;
            Ok(json!({
                "status": "interrupted",
                "reason": if self.debug_events.is_empty() { reason } else { "breakpoint" },
                "unit": "frames",
                "count": after.saturating_sub(before),
                "requested": count,
                "frame_before": before,
                "frame": after,
                "state": "frozen",
                "event_pending": !self.debug_events.is_empty(),
            }))
        })();
        self.control.set_command_deadline(None);
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail_debugger(format!(
                "frame step interrupted by {reason} and stop evidence is incomplete: {error}"
            )),
        }
    }

    pub(super) fn initialize_frame_monitor(&mut self) -> BridgeResult<()> {
        self.control.command(FRAME_TCL)?;
        self.require_frame_probe()?;
        let inventory = self.frame_probe_inventory()?;
        if !inventory.is_empty() {
            return Err(OpenMsxBridgeError::BadState(
                "openMSX already has a VDP frame-boundary probe breakpoint".into(),
            ));
        }
        self.install_frame_probe()
    }

    pub(super) fn rebind_frame_monitor_after_machine_load(&mut self) -> BridgeResult<()> {
        self.require_frame_probe()?;
        let inventory = self.frame_probe_inventory()?;
        if inventory.is_empty() {
            return self.install_frame_probe();
        }
        self.reconcile_frame_monitor()
    }

    fn require_frame_probe(&mut self) -> BridgeResult<()> {
        let probes = self.control.command("debug probe list")?;
        if probes.split_whitespace().any(|probe| probe == FRAME_PROBE) {
            Ok(())
        } else {
            Err(OpenMsxBridgeError::Unsupported(
                "the pinned openMSX host does not expose its VDP frame-boundary probe".into(),
            ))
        }
    }

    fn install_frame_probe(&mut self) -> BridgeResult<()> {
        let native_id = self.control.command(&format!(
            "debug probe set_bp {{{FRAME_PROBE}}} {{}} {{{FRAME_CALLBACK}}}"
        ))?;
        if !valid_frame_probe_id(&native_id) {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "openMSX returned an invalid frame probe breakpoint ID: {native_id:?}"
            )));
        }
        self.frame_probe_native_id = Some(native_id);
        self.reconcile_frame_monitor()
    }

    pub(super) fn reconcile_frame_monitor(&mut self) -> BridgeResult<()> {
        let expected_id = self.frame_probe_native_id.clone().ok_or_else(|| {
            OpenMsxBridgeError::BadState("MSX frame monitor has no native identity".into())
        })?;
        let inventory = self.frame_probe_inventory()?;
        if inventory.len() != 1 {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "expected one native frame probe breakpoint, observed {}",
                inventory.len()
            )));
        }
        let observed = &inventory[0];
        if observed
            != &[
                expected_id,
                FRAME_PROBE.to_string(),
                String::new(),
                FRAME_CALLBACK.to_string(),
            ]
        {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "native frame probe breakpoint drifted: {observed:?}"
            )));
        }
        Ok(())
    }

    fn frame_probe_inventory(&mut self) -> BridgeResult<Vec<[String; 4]>> {
        let encoded = self.control.command("::emucap::frame_probe_inventory")?;
        let bytes = hex::decode(encoded.trim()).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!(
                "openMSX returned invalid frame probe inventory hex: {error}"
            ))
        })?;
        let payload = String::from_utf8(bytes).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!(
                "openMSX frame probe inventory was not UTF-8: {error}"
            ))
        })?;
        payload
            .lines()
            .map(|line| {
                let fields = line.split('|').map(str::to_owned).collect::<Vec<_>>();
                fields.try_into().map_err(|fields: Vec<String>| {
                    OpenMsxBridgeError::Protocol(format!(
                        "openMSX frame probe inventory row has {} fields instead of 4",
                        fields.len()
                    ))
                })
            })
            .collect()
    }
}

fn valid_frame_probe_id(id: &str) -> bool {
    id.strip_prefix("pp#").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}
