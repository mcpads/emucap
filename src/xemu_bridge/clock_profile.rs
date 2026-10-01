//! Fixed guest timing identity, independent of the generation's host pacing policy.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct XemuClockProfile {
    scheduler: String,
    instruction_ns: u64,
    multi_thread: bool,
    sleep: bool,
    align: bool,
    devices: String,
    rtc_clock: String,
}

impl XemuClockProfile {
    pub fn candidate(shift: u8) -> Result<Self, String> {
        if shift > 3 {
            return Err("Xbox qualification shift must be in 0..=3".into());
        }
        Ok(Self {
            scheduler: "precise-icount".into(),
            instruction_ns: 1 << shift,
            multi_thread: false,
            sleep: false,
            align: false,
            devices: "nv2a-apu-virtual".into(),
            rtc_clock: "virtual".into(),
        })
    }
}

/// Internal qualification override; absence selects the fixed eight-nanosecond profile.
pub fn qualification_shift(value: Option<&str>) -> Result<u8, String> {
    let shift = match value {
        None => 3,
        Some(value) => value
            .parse::<u8>()
            .map_err(|_| "Xbox qualification shift must be an integer in 0..=3".to_string())?,
    };
    XemuClockProfile::candidate(shift)?;
    Ok(shift)
}

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn verify_clock_profile(&mut self, reply: &Value) -> XemuResult<()> {
        let observed = reply
            .get("clock-profile")
            .cloned()
            .ok_or_else(|| "native host omitted clock-profile".to_string())
            .and_then(|value| {
                serde_json::from_value::<XemuClockProfile>(value)
                    .map_err(|error| format!("invalid native clock-profile: {error}"))
            });
        match observed {
            Ok(profile) if profile == self.state_environment.clock_profile => Ok(()),
            observed => {
                self.control_unverified = true;
                Err(XemuBridgeError::Emulator(format!(
                    "Xbox guest timing profile does not match this generation: expected {:?}, observed {:?}",
                    self.state_environment.clock_profile, observed
                )))
            }
        }
    }
}
