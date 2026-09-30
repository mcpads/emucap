//! Frozen memory batches and fork-owned pacing for DeSmuME.
use super::*;
use crate::live::memory_batch::{
    BatchRange, MemoryBatchCapability, MemoryWindow, CONSISTENCY_FROZEN_BOUNDARY,
};
use crate::live::pacing::{self, ExecutionSpeedCapability, PercentDomain, SpeedRequest};

const PACING_MIN_PERCENT: u64 = 1;
const PACING_MAX_PERCENT: u64 = 10_000;
const BATCH_MAX_RANGES: u64 = 64;
// 8 KiB payload plus epochs fits the native 32 KiB RSP packet.
const BATCH_MAX_BYTES: u64 = 8_192;

/// Fork pacing readback `<percent>,<native unlimited>,<revision>,<VBlank clock>` in hex. Percent 0
/// is the agent's unlimited mode; native unlimited is the disabled limiter or a held boost key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NdsPacing {
    pub(super) percent: u64,
    pub(super) native_unlimited: bool,
    pub(super) revision: u64,
    pub(super) clock: u64,
}

impl NdsPacing {
    pub(super) fn parse(raw: &str) -> Option<Self> {
        let mut fields = raw.split(',').map(|field| u64::from_str_radix(field, 16));
        let pacing = Self {
            percent: fields.next()?.ok()?,
            native_unlimited: match fields.next()?.ok()? {
                0 => false,
                1 => true,
                _ => return None,
            },
            revision: fields.next()?.ok()?,
            clock: fields.next()?.ok()?,
        };
        fields.next().is_none().then_some(pacing)
    }

    pub(super) fn public(&self) -> Value {
        let limited = (PACING_MIN_PERCENT..=PACING_MAX_PERCENT).contains(&self.percent);
        let (mode, percent) = if self.native_unlimited || self.percent == 0 {
            ("unlimited", Value::Null)
        } else if limited {
            ("limited", json!(self.percent))
        } else {
            ("custom", Value::Null)
        };
        json!({
            "mode": mode,
            "percent": percent,
            "source": "native",
            "policy_revision": self.revision.to_string(),
            "host_constraints": [],
            "diagnostics": {
                "agent_percent": self.percent,
                "native_unlimited": self.native_unlimited,
            },
        })
    }
}

impl<G: GdbTransport> NdsBridge<G> {
    pub(super) fn memory_batch_capability() -> MemoryBatchCapability {
        let main = memory_region("main").expect("main region is defined");
        MemoryBatchCapability {
            max_ranges: BATCH_MAX_RANGES,
            max_range_bytes: BATCH_MAX_BYTES,
            max_total_bytes: BATCH_MAX_BYTES,
            consistency: CONSISTENCY_FROZEN_BOUNDARY.into(),
            halt_kinds: vec!["shared_scheduler_stop".into()],
            // Main RAM only: the full-bus views reach MMIO whose reads have side effects.
            windows: vec![MemoryWindow {
                memory_type: main.name.into(),
                address: 0,
                length: main.size,
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

    pub(super) fn native_pacing(&mut self) -> NdsResult<NdsPacing> {
        let raw = self.arm9.query_running_safe("qEmucap,pacing")?;
        NdsPacing::parse(&raw).ok_or_else(|| {
            NdsBridgeError::Emulator(format!("invalid DeSmuME pacing readback: {raw}"))
        })
    }

    fn set_native_pacing(&mut self, percent: u64) -> NdsResult<()> {
        let reply = self
            .arm9
            .query_running_safe(&format!("QEmucap,pacing:{percent:x}"))?;
        if reply != "OK" {
            return Err(NdsBridgeError::Emulator(format!(
                "DeSmuME refused pacing {percent}: {reply}"
            )));
        }
        Ok(())
    }

    pub(super) fn execution_speed_value(&mut self) -> NdsResult<Value> {
        Ok(self.native_pacing()?.public())
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> NdsResult<Value> {
        let capability = Self::execution_speed_capability();
        let mode = match params.get("mode") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| NdsBridgeError::BadParams("mode must be a string".into()))?,
            ),
        };
        let percent = match params.get("percent") {
            None => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .ok_or_else(|| NdsBridgeError::BadParams("percent must be a number".into()))?,
            ),
        };
        let request = pacing::parse_request(mode, percent).map_err(NdsBridgeError::BadParams)?;
        capability
            .admit(request)
            .map_err(NdsBridgeError::BadParams)?;
        let target = match request {
            SpeedRequest::Query => return self.execution_speed_value(),
            SpeedRequest::Unlimited => 0,
            SpeedRequest::Limited { centi_percent } => centi_percent / 100,
        };
        self.drain_scheduler_stops()?;
        let before = self.native_pacing()?;
        let outcome: NdsResult<Value> = (|| {
            self.set_native_pacing(target)?;
            let after = self.native_pacing()?;
            let state = if self.primary_frozen() {
                "frozen"
            } else {
                "running"
            };
            let reply = json!({
                "status": "completed",
                "state": state,
                "previous": before.public(),
                "execution_speed": after.public(),
                "frame": after.clock,
            });
            capability
                .verify_change(request, &reply)
                .map_err(NdsBridgeError::Emulator)?;
            Ok(reply)
        })();
        outcome.map_err(|error| {
            // Separate native requests cannot exclude a concurrent human policy change.
            // Do not overwrite it with a stale rollback or continue on an unknown policy.
            self.control_unverified = true;
            NdsBridgeError::Emulator(format!("execution_speed unverified: {error}; last verified policy: {}; guest progress may have occurred", before.public()))
        })
    }

    pub(super) fn read_memory_batch(&mut self, params: &Value) -> NdsResult<Value> {
        let ranges = params
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or_else(|| NdsBridgeError::BadParams("ranges must be an array".into()))?
            .iter()
            .map(|range| {
                Ok(BatchRange {
                    memory_type: range
                        .get("memory_type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| NdsBridgeError::BadParams("memory_type is required".into()))?
                        .to_string(),
                    address: required_num(range, "address")?,
                    length: required_num(range, "length")?,
                })
            })
            .collect::<NdsResult<Vec<_>>>()?;
        Self::memory_batch_capability()
            .admit(&ranges)
            .map_err(NdsBridgeError::BadParams)?;
        self.drain_scheduler_stops()?;
        if !self.primary_frozen() {
            return Err(NdsBridgeError::NotFrozen(
                "read_memory_batch requires the frozen shared scheduler; call pause first".into(),
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
        let raw = self
            .arm9
            .query_running_safe(&format!("qEmucap,batch:{descriptors}"))
            .inspect_err(|_| {
                self.control_unverified = true;
            })?;
        let decoded = (|| {
            let (header, data) = raw.split_once(':')?;
            let (epoch, clock) = header.split_once(',')?;
            Some((
                u64::from_str_radix(epoch, 16).ok()?,
                u64::from_str_radix(clock, 16).ok()?,
                hex::decode(data).ok()?,
            ))
        })();
        let Some((native_epoch, clock, payload)) =
            decoded.filter(|(_, _, bytes)| bytes.len() == total)
        else {
            self.control_unverified = true;
            return Err(NdsBridgeError::Emulator(
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
                "runtime_generation": self.env.launch_id.clone()
                    .unwrap_or_else(|| format!("unmanaged-pid:{}", std::process::id())),
                "stop_epoch": epoch,
                "memory_mapping_epoch": epoch,
                "clocks": [{"domain": "nds.vblank_start", "value": clock}],
            },
            "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
            "reads": reads,
        }))
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
