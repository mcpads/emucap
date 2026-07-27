use super::{BridgeResult, OpenMsxBridge, OpenMsxBridgeError, OpenMsxControl};

const FRAME_PROBE: &str = "emucap.vdp_frame_boundary";
const FRAME_CALLBACK: &str = "::emucap::frame_tick";

const FRAME_TCL: &str = r#"namespace eval ::emucap {
    variable frame_seq 0
    variable frame_target {}

    proc next_frame {count} {
        after realtime 0 [list ::emucap::start_frame $count]
        return {}
    }

    proc start_frame {count} {
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
        incr frame_seq
        if {$frame_target ne {} && $frame_seq >= $frame_target} {
            set frame_target {}
            set ::pause on
        }
    }

    proc cancel_frame {} {
        variable frame_target
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
