//! Frozen memory batches and MAME-native pacing through the shared Lua plugin.
use std::collections::BTreeSet;

use super::*;
use crate::live::memory_batch::{BatchRange, MemoryBatchCapability, MemoryWindow};
use crate::live::pacing::{self, ExecutionSpeedCapability};
use crate::mame_observation as mame;

impl<G: GdbTransport> Bridge<G> {
    /// Host features of the connected MAME build and plugin. A method is advertised only when the
    /// native binding it needs answered this probe.
    pub(super) fn mame_features(&mut self) -> BTreeSet<String> {
        if let Some(features) = &self.mame_features {
            return features.clone();
        }
        let features = self
            .lua_data_cmd_reply("features", None)
            .map(|raw| mame::parse_features(&raw))
            .unwrap_or_default();
        self.mame_features = Some(features.clone());
        features
    }

    pub(super) fn memory_batch_capability() -> MemoryBatchCapability {
        mame::batch_capability(
            MEMORY_REGIONS
                .iter()
                .map(|region| MemoryWindow {
                    memory_type: region.name.into(),
                    address: 0,
                    length: u64::from(region.size),
                })
                .collect(),
        )
    }

    pub(super) fn execution_speed_capability() -> ExecutionSpeedCapability {
        mame::speed_capability()
    }

    pub(super) fn read_memory_batch(&mut self, params: &Value) -> BridgeResult<Value> {
        let ranges = params
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or_else(|| BridgeError::BadParams("ranges must be an array".into()))?
            .iter()
            .map(|range| {
                Ok(BatchRange {
                    memory_type: range
                        .get("memory_type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| BridgeError::BadParams("memory_type is required".into()))?
                        .to_string(),
                    address: required_num(range, "address")?,
                    length: required_num(range, "length")?,
                })
            })
            .collect::<BridgeResult<Vec<_>>>()?;
        Self::memory_batch_capability()
            .admit(&ranges)
            .map_err(BridgeError::BadParams)?;
        self.drain_stop()?;
        if !self.frozen {
            return Err(BridgeError::BadState(
                "read_memory_batch requires a frozen PC-98 machine; call pause first".into(),
            ));
        }
        let absolute: Vec<_> = ranges
            .iter()
            .map(|range| {
                let region = memory_region(&range.memory_type).expect("admitted window");
                (u64::from(region.base) + range.address, range.length)
            })
            .collect();
        let raw = self.lua_data_cmd_reply("peekbatch", Some(&mame::peek_spec(&absolute)))?;
        let peek = mame::parse_peek_reply(&raw, &ranges).ok_or_else(|| {
            BridgeError::Emulator(format!(
                "MAME returned an unverifiable memory batch: {raw:.80}"
            ))
        })?;
        Ok(mame::batch_reply(&ranges, peek, self.runtime_generation()))
    }

    fn runtime_generation(&self) -> String {
        self.env
            .launch_id
            .clone()
            .unwrap_or_else(|| format!("unmanaged-pid:{}", std::process::id()))
    }

    fn native_pacing(&mut self) -> BridgeResult<mame::MamePacing> {
        let raw = self.lua_data_cmd_reply("pacing", None)?;
        mame::MamePacing::parse(&raw)
            .ok_or_else(|| BridgeError::Emulator(format!("invalid MAME pacing readback: {raw}")))
    }

    pub(super) fn execution_speed_value(&mut self) -> BridgeResult<Value> {
        Ok(self
            .native_pacing()?
            .public(&Self::execution_speed_capability()))
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> BridgeResult<Value> {
        let capability = Self::execution_speed_capability();
        let mode = match params.get("mode") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| BridgeError::BadParams("mode must be a string".into()))?,
            ),
        };
        let percent = match params.get("percent") {
            None => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .ok_or_else(|| BridgeError::BadParams("percent must be a number".into()))?,
            ),
        };
        let request = pacing::parse_request(mode, percent).map_err(BridgeError::BadParams)?;
        capability.admit(request).map_err(BridgeError::BadParams)?;
        let Some(spec) = mame::set_spec(request) else {
            return self.execution_speed_value();
        };
        self.drain_stop()?;
        let raw = match self.lua_data_cmd_reply("setpacing", Some(&spec)) {
            Ok(raw) => raw,
            Err(BridgeError::Emulator(message)) if message.contains("E1D:restored") => {
                return Err(BridgeError::Emulator(format!(
                    "execution_speed failed_restored: {message}"
                )))
            }
            Err(BridgeError::Emulator(message)) if message.contains("E1D") => {
                return self.fail_control(format!("execution_speed unverified: {message}"))
            }
            Err(error) => return Err(error),
        };
        let reply = mame::parse_set_reply(&raw).ok_or_else(|| {
            BridgeError::Emulator(format!("invalid MAME pacing transaction reply: {raw}"))
        })?;
        let previous = reply.previous.public(&capability);
        let applied = reply.applied.public(&capability);
        let confirmed = capability.verify_change(
            request,
            &json!({"status":"completed", "state":"running", "previous": previous,
                "execution_speed": applied}),
        );
        let boundary_kept = !self.frozen || reply.boundary_kept;
        if confirmed.is_ok() && boundary_kept {
            return Ok(json!({
                "status": "completed",
                "state": if self.frozen { "frozen" } else { "running" },
                "previous": previous,
                "execution_speed": applied,
            }));
        }
        let reason = confirmed
            .err()
            .unwrap_or_else(|| "frozen boundary changed during the pacing transaction".into());
        let restore = reply.previous.restore_spec(reply.applied.revision);
        match self.lua_data_cmd_reply("restorepacing", Some(&restore)) {
            Ok(raw) if raw.starts_with("CONFLICT|") => Err(BridgeError::BadState(format!(
                "execution_speed conflict: {reason}; an external policy change was kept: {raw}"
            ))),
            Ok(raw)
                if mame::MamePacing::parse(&raw)
                    .is_some_and(|restored| restored.same_settings(&reply.previous)) =>
            {
                Err(BridgeError::Emulator(format!(
                    "execution_speed failed_restored: {reason}; previous policy verified"
                )))
            }
            outcome => self.fail_control(format!(
                "execution_speed unverified: {reason}; restoration outcome {outcome:?}"
            )),
        }
    }

    /// Paced frame waits carry a host deadline so they stop at the reached frame instead of
    /// outliving the synchronous operation budget. Returns the plugin deadline in milliseconds.
    pub(super) fn arm_frame_deadline(&mut self, frames: u64) -> BridgeResult<Option<u64>> {
        if !mame::supports_pacing(&self.mame_features()) {
            return Ok(None);
        }
        let per_frame = self
            .native_pacing()?
            .frame_budget_ms(self.frame_operation_budget_ms());
        let budget = crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64 - 5_000;
        let deadline = 5_000u64
            .saturating_add(frames.saturating_mul(per_frame))
            .min(budget);
        self.lua_cmd("opdeadline", Some(&deadline.to_string()))?;
        Ok(Some(deadline))
    }

    fn fail_control<T>(&mut self, message: String) -> BridgeResult<T> {
        self.control_fatal = Some(message.clone());
        Err(BridgeError::Emulator(message))
    }
}
