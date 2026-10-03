//! Frozen memory batches and MAME-native pacing through the shared Lua plugin.
use std::collections::BTreeSet;

use super::*;
use crate::live::memory_batch::{BatchRange, MemoryBatchCapability, MemoryWindow};
use crate::live::pacing;
use crate::mame_observation as mame;

impl<G: GdbTransport> NeoGeoBridge<G> {
    fn mame_features(&mut self) -> BTreeSet<String> {
        if let Some(features) = &self.mame_features {
            return features.clone();
        }
        let features = self
            .lua_cmd("features", None)
            .map(|raw| mame::parse_features(&raw))
            .unwrap_or_default();
        self.mame_features = Some(features.clone());
        features
    }

    pub(super) fn supports_state_io(&mut self) -> bool {
        let features = self.mame_features();
        features.contains("settled_state_io") && features.contains("native_raster_state")
    }

    /// Profile methods plus the batch and pacing methods the connected host proved it supports.
    pub(super) fn advertised_methods(&mut self) -> Vec<&'static str> {
        let features = self.mame_features();
        let mut methods = METHODS.to_vec();
        if !features.contains("settled_state_io") || !features.contains("native_raster_state") {
            methods.retain(|method| !matches!(*method, "save_state" | "load_state"));
        }
        if mame::supports_batch(&features) {
            methods.push("read_memory_batch");
        }
        if mame::supports_pacing(&features) {
            methods.push("execution_speed");
        }
        methods
    }

    pub(super) fn feature_capabilities(&mut self, methods: &[&str], target: &mut Value) {
        let object = target.as_object_mut().expect("capability target object");
        if methods.contains(&"read_memory_batch") {
            object.insert(
                "memory_batch_capability".into(),
                json!(self.batch_capability()),
            );
        }
        if methods.contains(&"execution_speed") {
            object.insert(
                "execution_speed_capability".into(),
                json!(mame::speed_capability()),
            );
        }
    }

    fn batch_capability(&self) -> MemoryBatchCapability {
        let (_, size) = self.profile.ram();
        let mut capability = mame::batch_capability(vec![MemoryWindow {
            memory_type: "ram".into(),
            address: 0,
            length: size,
        }]);
        if self
            .mame_features
            .as_ref()
            .is_some_and(|features| features.contains("settled_state_io"))
        {
            capability.halt_kinds.push("settled_scheduler".into());
        }
        capability
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
        self.batch_capability()
            .admit(&ranges)
            .map_err(BridgeError::BadParams)?;
        self.drain_breakpoint_packets()?;
        self.require_frozen("read_memory_batch")?;
        let (base, _) = self.profile.ram();
        let absolute: Vec<_> = ranges
            .iter()
            .map(|range| (base + range.address, range.length))
            .collect();
        let raw = self.lua_cmd("peekbatch", Some(&mame::peek_spec(&absolute)))?;
        let peek = mame::parse_peek_reply(&raw, &ranges).ok_or_else(|| {
            BridgeError::Emulator(format!(
                "MAME returned an unverifiable memory batch: {raw:.80}"
            ))
        })?;
        let generation = self
            .env
            .launch_id
            .clone()
            .unwrap_or_else(|| format!("unmanaged-pid:{}", std::process::id()));
        Ok(mame::batch_reply(&ranges, peek, generation))
    }

    fn native_pacing(&mut self) -> BridgeResult<mame::MamePacing> {
        let raw = self.lua_cmd("pacing", None)?;
        mame::MamePacing::parse(&raw)
            .ok_or_else(|| BridgeError::Emulator(format!("invalid MAME pacing readback: {raw}")))
    }

    pub(super) fn execution_speed_value(&mut self) -> BridgeResult<Value> {
        Ok(self.native_pacing()?.public(&mame::speed_capability()))
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> BridgeResult<Value> {
        let capability = mame::speed_capability();
        let text = |name: &str| -> BridgeResult<Option<&str>> {
            params
                .get(name)
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| BridgeError::BadParams(format!("{name} must be a string")))
                })
                .transpose()
        };
        let mode = text("mode")?;
        let percent = params
            .get("percent")
            .map(|value| {
                value
                    .as_f64()
                    .ok_or_else(|| BridgeError::BadParams("percent must be a number".into()))
            })
            .transpose()?;
        let request = pacing::parse_request(mode, percent).map_err(BridgeError::BadParams)?;
        capability.admit(request).map_err(BridgeError::BadParams)?;
        let Some(spec) = mame::set_spec(request) else {
            return self.execution_speed_value();
        };
        self.drain_breakpoint_packets()?;
        let was_frozen = self.frozen;
        let raw = match self.lua_cmd("setpacing", Some(&spec)) {
            Ok(raw) => raw,
            Err(BridgeError::Emulator(message)) if message.contains("E1D:restored") => {
                return Err(BridgeError::Emulator(format!(
                    "execution_speed failed_restored: {message}"
                )))
            }
            Err(BridgeError::Emulator(message)) if message.contains("E1D") => {
                return self.fail_control(format!("execution_speed unverified: {message}"))
            }
            Err(error) => return self.fail_control(format!("execution_speed unverified: {error}")),
        };
        // Native settings may already have changed; malformed evidence cannot
        // supply a verified previous policy or a safe rollback revision.
        let Some(reply) = mame::parse_set_reply(&raw) else {
            return self.fail_control(format!(
                "execution_speed unverified: invalid MAME pacing transaction reply: {raw}"
            ));
        };
        // Preserve stops and public breakpoint rearming before publishing state.
        if let Err(error) = self.drain_breakpoint_packets() {
            return self.fail_control(format!("execution_speed unverified: {error}"));
        }
        let previous = reply.previous.public(&capability);
        let applied = reply.applied.public(&capability);
        let confirmed = capability.verify_change(
            request,
            &json!({"status":"completed", "state":"running", "previous": previous,
                "execution_speed": applied}),
        );
        if confirmed.is_ok() && (!was_frozen || reply.boundary_kept) {
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
        match self.lua_cmd("restorepacing", Some(&restore)) {
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

    /// Arms a plugin host deadline for a paced frame wait and returns the socket timeout covering
    /// it; without pacing support the fixed per-frame estimate applies.
    pub(super) fn frame_wait_timeout(&mut self, frames: u64) -> BridgeResult<Duration> {
        let fixed = FRAME_OPERATION_STARTUP_MS
            .saturating_add(frames.saturating_mul(FRAME_OPERATION_BUDGET_MS));
        if !mame::supports_pacing(&self.mame_features()) {
            return Ok(Duration::from_millis(fixed));
        }
        let per_frame = self
            .native_pacing()?
            .frame_budget_ms(FRAME_OPERATION_BUDGET_MS);
        let budget = crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64 - 5_000;
        let deadline = FRAME_OPERATION_STARTUP_MS
            .saturating_add(frames.saturating_mul(per_frame))
            .min(budget);
        let reply = self.lua_cmd("opdeadline", Some(&deadline.to_string()))?;
        if reply != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME opdeadline failed: {reply}"
            )));
        }
        Ok(Duration::from_millis(deadline + 5_000))
    }

    pub(super) fn fail_control<T>(&mut self, message: String) -> BridgeResult<T> {
        self.control_fatal = Some(message.clone());
        Err(BridgeError::Emulator(message))
    }
}
