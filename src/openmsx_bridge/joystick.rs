use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::input::{joystick_mask, joystick_mask_has_opposites};
use super::{
    finish_input_pulse, parse_decimal, BridgeResult, OpenMsxBridge, OpenMsxBridgeError,
    OpenMsxControl,
};

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub(super) fn press_joystick_buttons(
        &mut self,
        index: usize,
        buttons: BTreeSet<String>,
        frames: u64,
    ) -> BridgeResult<Value> {
        if buttons.is_empty() {
            return Err(OpenMsxBridgeError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        self.refresh_execution_state()?;
        let was_running = !self.frozen;
        if was_running {
            self.pause(&json!({}))?;
        }
        let persistent = self.checked_joystick_owner(index)?;
        let pulse_mask = joystick_mask(&buttons);
        let combined = persistent.unwrap_or(0x3f) & pulse_mask;
        if joystick_mask_has_opposites(combined) {
            if was_running {
                self.resume(&json!({}))?;
            }
            return Err(OpenMsxBridgeError::BadParams(
                "joystick pulse conflicts with the persistent direction".into(),
            ));
        }
        if let Err(primary) = self.apply_joystick_owner(index, Some(combined)) {
            if was_running {
                return match self.resume(&json!({})) {
                    Ok(_) => Err(primary),
                    Err(restore) => Err(OpenMsxBridgeError::Emulator(format!(
                        "{primary}; execution-state restore also failed: {restore}"
                    ))),
                };
            }
            return Err(primary);
        }
        let pulse = self.frame_step(frames);
        let interrupted = pulse
            .as_ref()
            .is_ok_and(|value| value.get("status").and_then(Value::as_str) == Some("interrupted"));
        let release = self.apply_joystick_owner(index, persistent);
        let resume = if was_running && !interrupted {
            self.resume(&json!({})).map(|_| ())
        } else {
            Ok(())
        };
        let mut pulse = finish_input_pulse(pulse, release, resume)?;
        if interrupted {
            let object = pulse.as_object_mut().expect("step result is an object");
            object.insert("port".into(), json!(index + 1));
            object.insert("device".into(), json!("joystick"));
            object.insert("buttons".into(), json!(buttons));
            object.insert("input_override".into(), json!(persistent.is_some()));
            object.insert("restored_active_low_mask".into(), json!(persistent));
            return Ok(pulse);
        }
        Ok(json!({
            "status": "completed",
            "port": index + 1,
            "device": "joystick",
            "buttons": buttons,
            "frames": frames,
            "state": if was_running { "running" } else { "frozen" },
            "input_override": persistent.is_some(),
            "restored_active_low_mask": persistent,
        }))
    }

    pub(super) fn read_joystick_owner(&mut self, index: usize) -> BridgeResult<Option<u8>> {
        let encoded = parse_decimal(
            &self
                .control
                .command(&format!("debug read emucap_joystick_override {index}"))?,
            "emucap joystick override",
        )?;
        match encoded {
            0 => Ok(None),
            0x80..=0xbf => Ok(Some((encoded as u8) & 0x3f)),
            value => Err(OpenMsxBridgeError::Protocol(format!(
                "openMSX returned invalid joystick override status {value:#x} for port {}",
                index + 1
            ))),
        }
    }

    pub(super) fn checked_joystick_owner(&mut self, index: usize) -> BridgeResult<Option<u8>> {
        let native = self.read_joystick_owner(index)?;
        if native != self.joystick_owners[index] {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "joystick port {} ownership diverged: bridge={:?}, native={native:?}",
                index + 1,
                self.joystick_owners[index]
            )));
        }
        Ok(native)
    }

    pub(super) fn checked_joystick_owners(&mut self) -> BridgeResult<[Option<u8>; 2]> {
        Ok([
            self.checked_joystick_owner(0)?,
            self.checked_joystick_owner(1)?,
        ])
    }

    pub(super) fn guest_joystick_value(&mut self, index: usize) -> BridgeResult<u8> {
        let value = parse_decimal(
            &self
                .control
                .command(&format!("debug read joystickports {index}"))?,
            "guest joystick value",
        )?;
        u8::try_from(value).map_err(|_| {
            OpenMsxBridgeError::Protocol(format!(
                "openMSX returned invalid guest joystick value {value} for port {}",
                index + 1
            ))
        })
    }

    fn write_joystick_owner(&mut self, index: usize, owner: Option<u8>) -> BridgeResult<()> {
        let encoded = owner.unwrap_or(0xff);
        self.control.command(&format!(
            "debug write emucap_joystick_override {index} {encoded}"
        ))?;
        let observed = self.read_joystick_owner(index)?;
        if observed != owner {
            return Err(OpenMsxBridgeError::Emulator(format!(
                "joystick port {} override readback mismatch: expected {owner:?}, observed {observed:?}",
                index + 1
            )));
        }
        Ok(())
    }

    pub(super) fn apply_joystick_owner(
        &mut self,
        index: usize,
        desired: Option<u8>,
    ) -> BridgeResult<()> {
        let previous = self.checked_joystick_owner(index)?;
        match self.write_joystick_owner(index, desired) {
            Ok(()) => {
                self.joystick_owners[index] = desired;
                Ok(())
            }
            Err(primary) => {
                let cleanup = self.write_joystick_owner(index, previous);
                self.joystick_owners[index] = previous;
                match cleanup {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(OpenMsxBridgeError::Emulator(format!(
                        "{primary}; joystick rollback also failed: {cleanup}"
                    ))),
                }
            }
        }
    }

    pub(super) fn reapply_joystick_owners(&mut self) -> BridgeResult<()> {
        let desired = self.joystick_owners;
        for (index, owner) in desired.into_iter().enumerate() {
            if let Err(primary) = self.write_joystick_owner(index, owner) {
                let cleanup0 = self.write_joystick_owner(0, None);
                let cleanup1 = self.write_joystick_owner(1, None);
                if cleanup0.is_ok() && cleanup1.is_ok() {
                    self.joystick_owners = [None; 2];
                    return Err(primary);
                }
                return Err(OpenMsxBridgeError::Emulator(format!(
                    "{primary}; joystick cleanup failed: port1={cleanup0:?}, port2={cleanup1:?}"
                )));
            }
        }
        Ok(())
    }
}
