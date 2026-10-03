//! Frozen memory batches and fork-owned pacing through the `emucap.pacing` debugger command.
use super::*;
use crate::live::memory_batch::{
    BatchRange, MemoryBatchCapability, MemoryWindow, CONSISTENCY_FROZEN_BOUNDARY,
};
use crate::live::pacing::{self, ExecutionSpeedCapability, PercentDomain, SpeedRequest};

const PACING_MIN_PERCENT: u64 = 1;
const PACING_MAX_PERCENT: u64 = 10_000;
const BATCH_MAX_RANGES: u64 = 64;
const BATCH_MAX_BYTES: u64 = 65_536;

/// `emucap.pacing` readback. `percent` 0 is the agent's unlimited mode; fast-forward, a custom or
/// analog limit and netplay's forced 60 FPS each replace the agent speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PpssppPacing {
    pub(super) percent: u64,
    pub(super) fast_forward: bool,
    pub(super) fps_limit: u64,
    pub(super) network_forced: bool,
    pub(super) revision: u64,
    pub(super) vblank: u64,
}

impl PpssppPacing {
    pub(super) fn parse(reply: &Value) -> Option<Self> {
        Some(Self {
            percent: reply.get("percent")?.as_u64()?,
            fast_forward: reply.get("fast_forward")?.as_bool()?,
            fps_limit: reply.get("fps_limit")?.as_u64()?,
            network_forced: reply.get("network_forced")?.as_bool()?,
            revision: reply.get("revision")?.as_u64()?,
            vblank: reply.get("vblank")?.as_u64()?,
        })
    }

    pub(super) fn public(&self) -> Value {
        let (mode, percent) = if self.network_forced {
            ("custom", Value::Null)
        } else if self.fast_forward || (self.fps_limit == 0 && self.percent == 0) {
            ("unlimited", Value::Null)
        } else if self.fps_limit != 0
            || self.network_forced
            || !(PACING_MIN_PERCENT..=PACING_MAX_PERCENT).contains(&self.percent)
        {
            ("custom", Value::Null)
        } else {
            ("limited", json!(self.percent))
        };
        json!({
            "mode": mode,
            "percent": percent,
            "source": "native",
            "policy_revision": self.revision.to_string(),
            "host_constraints": [],
            "diagnostics": {
                "agent_percent": self.percent,
                "fast_forward": self.fast_forward,
                "fps_limit": self.fps_limit,
                "network_forced": self.network_forced,
            },
        })
    }
}

impl<T: WsTransport> PpssppBridge<T> {
    pub(super) fn memory_batch_capability() -> MemoryBatchCapability {
        MemoryBatchCapability {
            max_ranges: BATCH_MAX_RANGES,
            max_range_bytes: BATCH_MAX_BYTES,
            max_total_bytes: BATCH_MAX_BYTES,
            consistency: CONSISTENCY_FROZEN_BOUNDARY.into(),
            halt_kinds: vec!["cpu_stepping".into()],
            windows: vec![MemoryWindow {
                memory_type: "main".into(),
                address: 0,
                length: PSP_MAIN_RAM_SIZE,
            }],
        }
    }

    pub(super) fn execution_speed_capability() -> ExecutionSpeedCapability {
        ExecutionSpeedCapability {
            modes: vec!["limited".into(), "unlimited".into()],
            percent: PercentDomain {
                min: Some(PACING_MIN_PERCENT as f64),
                max: Some(PACING_MAX_PERCENT as f64),
                quantum: Some(1.0),
                values: None,
            },
            states: vec!["running".into(), "frozen".into()],
            scope: pacing::SCOPE_HOST_PACING.into(),
            source: "native".into(),
            control_service_ms: 20,
            host_constraints: vec![],
        }
    }

    pub(super) fn native_pacing(&mut self, params: Value) -> BridgeResult<PpssppPacing> {
        let reply = self.ws.call("emucap.pacing", params)?;
        PpssppPacing::parse(&reply).ok_or_else(|| {
            BridgeError::Emulator(format!("invalid emucap.pacing readback: {reply}"))
        })
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
        let set = match request {
            SpeedRequest::Query => return Ok(self.native_pacing(json!({}))?.public()),
            SpeedRequest::Unlimited => json!({ "unlimited": true }),
            SpeedRequest::Limited { centi_percent } => json!({ "percent": centi_percent / 100 }),
        };
        // Probe before mutation: an older host must not accept a set whose transaction
        // semantics this bridge cannot verify. This observation is not `previous`.
        let probe = self.ws.call("emucap.pacing", json!({}))?;
        let observed = PpssppPacing::parse(&probe).ok_or_else(|| {
            BridgeError::Emulator("invalid native pacing capability probe".into())
        })?;
        if probe.get("transaction_version").and_then(Value::as_u64) != Some(1) {
            return Err(BridgeError::Unsupported(
                "native pacing transaction version 1 is required; rebuild the PSP host".into(),
            ));
        }
        let mut native_result = None;
        let outcome: BridgeResult<Option<Value>> = (|| {
            let raw = self.ws.call("emucap.pacing", set)?;
            native_result = Some(raw.clone());
            let invalid =
                || BridgeError::Emulator("invalid native pacing transaction result".into());
            if raw.get("transaction_version").and_then(Value::as_u64) != Some(1)
                || raw.get("clock_domain").and_then(Value::as_str) != Some("psp_vblank")
            {
                return Err(invalid());
            }
            let before = raw
                .get("previous")
                .and_then(PpssppPacing::parse)
                .ok_or_else(invalid)?;
            let after = PpssppPacing::parse(&raw).ok_or_else(invalid)?;
            if raw.get("begin_vblank").and_then(Value::as_u64) != Some(before.vblank) {
                return Err(invalid());
            }
            match raw.get("outcome").and_then(Value::as_str) {
                Some("rejected") if before.network_forced && before.public() == after.public() => {
                    return Ok(None);
                }
                Some("completed") if before.revision != after.revision => {}
                _ => return Err(invalid()),
            }
            let state = if self.cpu_is_stepping()? {
                "frozen"
            } else {
                "running"
            };
            let reply = json!({
                "status": "completed",
                "state": state,
                "previous": before.public(),
                "execution_speed": after.public(),
                "frame": after.vblank,
            });
            capability
                .verify_change(request, &reply)
                .map_err(BridgeError::Emulator)?;
            Ok(Some(reply))
        })();
        let verified = outcome.map_err(|error| {
            // A later external write may follow the native transaction. Never overwrite
            // it with the pre-probe policy when response/state verification fails.
            self.control_unverified = true;
            BridgeError::Emulator(format!(
                "execution_speed unverified: {error}; last observed policy: {}; native transaction: {:?}; guest progress may have occurred",
                observed.public(), native_result
            ))
        })?;
        verified.ok_or_else(|| BridgeError::BadState(
            "network policy prevents speed control; native transaction rejected without mutation".into(),
        ))
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
        if !self.cpu_is_stepping()? {
            return Err(BridgeError::BadState(
                "read_memory_batch requires a halted CPU; call pause first".into(),
            ));
        }
        let total = ranges
            .iter()
            .map(|range| range.length as usize)
            .sum::<usize>();
        let descriptors = ranges
            .iter()
            .map(|r| format!("{:x},{:x}", r.address, r.length))
            .collect::<Vec<_>>()
            .join(";");
        let reply = self
            .ws
            .call("emucap.memoryBatch", json!({"ranges": descriptors}))
            .inspect_err(|_| {
                self.control_unverified = true;
            })?;
        let decoded = (|| {
            Some((
                reply.get("epoch")?.as_u64()?,
                reply.get("frame")?.as_u64()?,
                hex::decode(reply.get("hex")?.as_str()?).ok()?,
            ))
        })();
        let Some((native_epoch, vblank, payload)) =
            decoded.filter(|(_, _, bytes)| bytes.len() == total)
        else {
            self.control_unverified = true;
            return Err(BridgeError::Emulator(
                "unverified native memory batch".into(),
            ));
        };
        let mut offset = 0;
        let mut reads = Vec::with_capacity(ranges.len());
        for (index, range) in ranges.iter().enumerate() {
            let end = offset + range.length as usize;
            reads.push(json!({
                "index": index, "memory_type": range.memory_type, "address": range.address,
                "length": range.length, "hex": hex::encode(&payload[offset..end]),
            }));
            offset = end;
        }
        let epoch = format!("native-stop:{native_epoch}#s{}", self.boundary_seq);
        Ok(json!({
            "state": "frozen",
            "consistency": CONSISTENCY_FROZEN_BOUNDARY,
            "boundary": {
                "runtime_generation": self.launch_id.clone()
                    .unwrap_or_else(|| format!("unmanaged-pid:{}", std::process::id())),
                "stop_epoch": epoch,
                "memory_mapping_epoch": epoch,
                "clocks": [{"domain": "psp.vblank_start", "value": vblank}],
            },
            "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
            "reads": reads,
        }))
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
