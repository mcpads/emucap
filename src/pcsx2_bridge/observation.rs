//! Frozen memory batches and PCSX2-native pacing through the emucap PINE opcodes.
use super::*;
use crate::live::memory_batch::{
    BatchRange, MemoryBatchCapability, MemoryWindow, CONSISTENCY_FROZEN_BOUNDARY,
};
use crate::live::pacing::{self, ExecutionSpeedCapability, PercentDomain, SpeedRequest};

const MSG_EMUCAP_PACING: u8 = 0x95;
const MSG_EMUCAP_SET_PACING: u8 = 0x96;
const PACING_MIN_PERCENT: u64 = 1;
const PACING_MAX_PERCENT: u64 = 10_000;
const BATCH_MAX_RANGES: u64 = 64;
const BATCH_MAX_BYTES: u64 = 65_536;

/// PCSX2 `LimiterModeType`.
const LIMITER_NOMINAL: u32 = 0;
const LIMITER_UNLIMITED: u32 = 3;

/// Native pacing readback: limiter mode, nominal speed in hundredths of a percent, the effective
/// target speed, a native policy revision, and the frame counter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Pcsx2Pacing {
    pub(super) mode: u32,
    pub(super) nominal_centi: u32,
    pub(super) target: f32,
    pub(super) revision: u64,
    pub(super) frame: u64,
}

impl Pcsx2Pacing {
    pub(super) fn parse(payload: &[u8]) -> Option<Self> {
        if payload.len() != 28 {
            return None;
        }
        let u32_at = |at: usize| u32::from_le_bytes(payload[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(payload[at..at + 8].try_into().unwrap());
        Some(Self {
            mode: u32_at(0),
            nominal_centi: u32_at(4),
            target: f32::from_bits(u32_at(8)),
            revision: u64_at(12),
            frame: u64_at(20),
        })
    }

    fn transaction(payload: &[u8]) -> BridgeResult<(u32, Self, Self)> {
        let invalid = || Pcsx2BridgeError::Protocol("invalid pacing transaction".into());
        if payload.len() != 60 {
            return Err(invalid());
        }
        let outcome = u32::from_le_bytes(payload[..4].try_into().unwrap());
        let before = Self::parse(&payload[4..32]).ok_or_else(invalid)?;
        let after = Self::parse(&payload[32..]).ok_or_else(invalid)?;
        let changed = before.mode != after.mode
            || before.nominal_centi != after.nominal_centi
            || before.target != after.target;
        if before.mode > LIMITER_UNLIMITED
            || after.mode > LIMITER_UNLIMITED
            || !before.target.is_finite()
            || !after.target.is_finite()
            || before.target < 0.0
            || after.target < 0.0
            || before.frame != after.frame
            || outcome > 1
            || (outcome == 0 && after.revision != before.revision.wrapping_add(u64::from(changed)))
            || (outcome == 1 && (changed || after.revision.wrapping_sub(before.revision) > 2))
        {
            return Err(invalid());
        }
        Ok((outcome, before, after))
    }

    /// Nominal is limited at its configured percent; turbo, slow motion and a fast-boot override
    /// replace it and read back as `custom`.
    pub(super) fn public(&self) -> Value {
        let percent_whole = self
            .nominal_centi
            .is_multiple_of(100)
            .then_some(self.nominal_centi as u64 / 100);
        let (mode, percent) = match self.mode {
            LIMITER_UNLIMITED if self.target == 0.0 => ("unlimited", Value::Null),
            LIMITER_NOMINAL
                if self.target.is_finite()
                    && self.target > 0.0
                    && (f64::from(self.target) * 10_000.0).round()
                        == f64::from(self.nominal_centi) =>
            {
                match percent_whole {
                    Some(percent)
                        if (PACING_MIN_PERCENT..=PACING_MAX_PERCENT).contains(&percent) =>
                    {
                        ("limited", json!(percent))
                    }
                    _ => ("custom", Value::Null),
                }
            }
            _ => ("custom", Value::Null),
        };
        json!({
            "mode": mode,
            "percent": percent,
            "source": "native",
            "policy_revision": self.revision.to_string(),
            "host_constraints": [],
            "diagnostics": {
                "limiter_mode": self.mode,
                "nominal_speed": self.nominal_centi as f64 / 10_000.0,
                "target_speed": self.target,
            },
        })
    }
}

impl<T: PineTransport> Pcsx2Bridge<T> {
    pub(super) fn memory_batch_capability() -> MemoryBatchCapability {
        MemoryBatchCapability {
            max_ranges: BATCH_MAX_RANGES,
            max_range_bytes: BATCH_MAX_BYTES,
            max_total_bytes: BATCH_MAX_BYTES,
            consistency: CONSISTENCY_FROZEN_BOUNDARY.into(),
            halt_kinds: vec!["cpu_thread_pause".into()],
            windows: vec![MemoryWindow {
                memory_type: "ee".into(),
                address: 0,
                length: PCSX2_EE_RAM_SIZE,
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

    pub(super) fn native_pacing(&mut self) -> BridgeResult<Pcsx2Pacing> {
        let payload = self.command(MSG_EMUCAP_PACING, &[])?;
        Pcsx2Pacing::parse(&payload)
            .ok_or_else(|| Pcsx2BridgeError::Protocol("invalid pacing readback".into()))
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> BridgeResult<Value> {
        let capability = Self::execution_speed_capability();
        let mode = match params.get("mode") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| Pcsx2BridgeError::BadParams("mode must be a string".into()))?,
            ),
        };
        let percent =
            match params.get("percent") {
                None => None,
                Some(value) => Some(value.as_f64().ok_or_else(|| {
                    Pcsx2BridgeError::BadParams("percent must be a number".into())
                })?),
            };
        let request = pacing::parse_request(mode, percent).map_err(Pcsx2BridgeError::BadParams)?;
        capability
            .admit(request)
            .map_err(Pcsx2BridgeError::BadParams)?;
        let target = match request {
            SpeedRequest::Query => return Ok(self.native_pacing()?.public()),
            SpeedRequest::Unlimited => 0,
            SpeedRequest::Limited { centi_percent } => (centi_percent / 100) as u32,
        };
        let observed = self.native_pacing()?;
        let mut restored = false;
        let mut native_result = None;
        let outcome: BridgeResult<Value> = (|| {
            let payload = self.command(MSG_EMUCAP_SET_PACING, &target.to_le_bytes())?;
            native_result = Some(hex::encode(&payload));
            let (outcome, before, after) = Pcsx2Pacing::transaction(&payload)?;
            if outcome == 1 {
                restored = true;
                return Err(Pcsx2BridgeError::Emulator(format!(
                    "execution_speed failed_restored: native effective policy rejected target; previous={}; restored={}; clock_domain=pcsx2.frame; frame_interval=[{},{}]",
                    before.public(), after.public(), before.frame, after.frame
                )));
            }
            let reply = json!({
                "status": "completed",
                "state": self.emulator_state()?,
                "previous": before.public(),
                "execution_speed": after.public(),
                "frame": after.frame,
                "clock_domain": "pcsx2.frame",
            });
            capability
                .verify_change(request, &reply)
                .map_err(Pcsx2BridgeError::Emulator)?;
            Ok(reply)
        })();
        outcome.map_err(|error| {
            if restored {
                return error;
            }
            // A pre-command observation is diagnostic, never transaction previous or rollback authority.
            self.control_unverified = true;
            Pcsx2BridgeError::Emulator(format!("execution_speed unverified: {error}; pre-command observation: {}; native_result={native_result:?}; clock_domain=pcsx2.frame; guest progress may have occurred", observed.public()))
        })
    }

    pub(super) fn read_memory_batch(&mut self, params: &Value) -> BridgeResult<Value> {
        let ranges = params
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or_else(|| Pcsx2BridgeError::BadParams("ranges must be an array".into()))?
            .iter()
            .map(|range| {
                Ok(BatchRange {
                    memory_type: range
                        .get("memory_type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            Pcsx2BridgeError::BadParams("memory_type is required".into())
                        })?
                        .to_string(),
                    address: required_num(range, "address")?,
                    length: required_num(range, "length")?,
                })
            })
            .collect::<BridgeResult<Vec<_>>>()?;
        Self::memory_batch_capability()
            .admit(&ranges)
            .map_err(Pcsx2BridgeError::BadParams)?;
        self.require_frozen("read_memory_batch")?;
        let total = ranges
            .iter()
            .map(|range| range.length as usize)
            .sum::<usize>();
        let mut body = (ranges.len() as u32).to_le_bytes().to_vec();
        for range in &ranges {
            body.extend_from_slice(&(range.address as u32).to_le_bytes());
            body.extend_from_slice(&(range.length as u32).to_le_bytes());
        }
        let reply = self.command(0x97, &body).inspect_err(|_| {
            self.control_unverified = true;
        })?;
        if reply.len() != 16 + total {
            self.control_unverified = true;
            return Err(Pcsx2BridgeError::Protocol(
                "unverified native memory batch".into(),
            ));
        }
        let native_epoch = u64::from_le_bytes(reply[..8].try_into().unwrap());
        let frame = u64::from_le_bytes(reply[8..16].try_into().unwrap());
        let payload = &reply[16..];
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
                "clocks": [{"domain": "pcsx2.frame", "value": frame}],
            },
            "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
            "reads": reads,
        }))
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
