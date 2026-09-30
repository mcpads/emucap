//! Frozen RDRAM batches and Mupen64Plus-native pacing through the core state commands.
use std::sync::Mutex;

use super::*;

pub(super) static CONTROL_UNVERIFIED: AtomicBool = AtomicBool::new(false);
use crate::live::memory_batch::{
    BatchRange, MemoryBatchCapability, MemoryWindow, CONSISTENCY_FROZEN_BOUNDARY,
};
use crate::live::pacing::{self, ExecutionSpeedCapability, PercentDomain, SpeedRequest};

const M64CMD_CORE_STATE_QUERY: c_int = 9;
const M64CMD_CORE_STATE_SET: c_int = 17;
const M64CORE_SPEED_FACTOR: c_int = 4;
const M64CORE_SPEED_LIMITER: c_int = 5;
/// The core's own speed-factor domain.
const PACING_MIN_PERCENT: u64 = 1;
const PACING_MAX_PERCENT: u64 = 1_000;
const BATCH_MAX_RANGES: u64 = 64;
const BATCH_MAX_BYTES: u64 = 65_536;

/// Bumped by every request outside the observation set, since those can change memory or the
/// stop without changing the frame counter.
pub(super) static BOUNDARY_SEQ: AtomicU64 = AtomicU64::new(0);
/// Last observed native pacing tuple and its revision.
static PACING_REVISION: Mutex<(Option<(c_int, c_int)>, u64)> = Mutex::new((None, 0));

const OBSERVATION_METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "get_state",
    "read_memory",
    "read_memory_batch",
    "list_breakpoints",
    "poll_events",
    "disassemble",
    "call_stack",
    "screenshot",
    "save_state",
    "execution_speed",
];

pub(super) fn note_request(method: &str) {
    if !OBSERVATION_METHODS.contains(&method) {
        BOUNDARY_SEQ.fetch_add(1, Ordering::AcqRel);
    }
}

/// Native speed factor (percent) and limiter state. A fast-forward hotkey sets the factor itself
/// and so reads back as its effective percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct N64Pacing {
    pub(super) factor: c_int,
    pub(super) limiter: bool,
    pub(super) revision: u64,
}

impl N64Pacing {
    fn observe(factor: c_int, limiter: c_int) -> Self {
        let mut revision = PACING_REVISION
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if revision.0 != Some((factor, limiter)) {
            revision.0 = Some((factor, limiter));
            revision.1 += 1;
        }
        Self {
            factor,
            limiter: limiter != 0,
            revision: revision.1,
        }
    }

    /// Longest wait for one frame: `base` at full speed, stretched by a slower limited target.
    pub(super) fn frame_wait(&self, base: Duration) -> Duration {
        match u32::try_from(self.factor) {
            Ok(factor) if self.limiter && (1..100).contains(&factor) => base * 100 / factor,
            _ => base,
        }
    }

    pub(super) fn public(&self) -> Value {
        let percent = u64::try_from(self.factor).ok();
        let (mode, value) = if !self.limiter {
            ("unlimited", Value::Null)
        } else if percent
            .is_some_and(|percent| (PACING_MIN_PERCENT..=PACING_MAX_PERCENT).contains(&percent))
        {
            ("limited", json!(self.factor))
        } else {
            ("custom", Value::Null)
        };
        json!({
            "mode": mode,
            "percent": value,
            "source": "native",
            "policy_revision": self.revision.to_string(),
            "host_constraints": [],
            "diagnostics": {"speed_factor": self.factor, "speed_limiter": self.limiter},
        })
    }
}

impl Mupen64PlusHost {
    pub(super) fn memory_batch_capability() -> MemoryBatchCapability {
        MemoryBatchCapability {
            max_ranges: BATCH_MAX_RANGES,
            max_range_bytes: BATCH_MAX_BYTES,
            max_total_bytes: BATCH_MAX_BYTES,
            consistency: CONSISTENCY_FROZEN_BOUNDARY.into(),
            halt_kinds: vec!["r4300_debugger_pause".into(), "rendered_frame_gate".into()],
            windows: vec![MemoryWindow {
                memory_type: "rdram".into(),
                address: 0,
                length: RDRAM_SIZE,
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

    fn core_state(&self, param: c_int) -> N64Result<c_int> {
        let mut value: c_int = 0;
        check_core("CoreDoCommand(state query)", unsafe {
            (self.api.core_do_command)(
                M64CMD_CORE_STATE_QUERY,
                param,
                (&mut value as *mut c_int).cast(),
            )
        })?;
        Ok(value)
    }

    fn set_core_state(&self, param: c_int, value: c_int) -> N64Result<()> {
        let mut value = value;
        check_core("CoreDoCommand(state set)", unsafe {
            (self.api.core_do_command)(
                M64CMD_CORE_STATE_SET,
                param,
                (&mut value as *mut c_int).cast(),
            )
        })
    }

    pub(super) fn native_pacing(&self) -> N64Result<N64Pacing> {
        self.require_connected()?;
        Ok(N64Pacing::observe(
            self.core_state(M64CORE_SPEED_FACTOR)?,
            self.core_state(M64CORE_SPEED_LIMITER)?,
        ))
    }

    fn apply_pacing(&self, limiter: bool, factor: Option<c_int>) -> N64Result<()> {
        if let Some(factor) = factor {
            self.set_core_state(M64CORE_SPEED_FACTOR, factor)?;
        }
        self.set_core_state(M64CORE_SPEED_LIMITER, c_int::from(limiter))
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> N64Result<Value> {
        let capability = Self::execution_speed_capability();
        let mode = match params.get("mode") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| N64Error::BadParams("mode must be a string".into()))?,
            ),
        };
        let percent = match params.get("percent") {
            None => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .ok_or_else(|| N64Error::BadParams("percent must be a number".into()))?,
            ),
        };
        let request = pacing::parse_request(mode, percent).map_err(N64Error::BadParams)?;
        capability.admit(request).map_err(N64Error::BadParams)?;
        let before = self.native_pacing()?;
        if matches!(request, SpeedRequest::Query) {
            return Ok(before.public());
        }
        let outcome: N64Result<Value> = (|| {
            match request {
                SpeedRequest::Query => unreachable!("handled before mutation"),
                SpeedRequest::Unlimited => self.apply_pacing(false, None)?,
                SpeedRequest::Limited { centi_percent } => {
                    self.apply_pacing(true, Some((centi_percent / 100) as c_int))?
                }
            }
            let after = self.native_pacing()?;
            let reply = json!({
                "status": "completed",
                "state": if self.frozen { "frozen" } else { "running" },
                "previous": before.public(),
                "execution_speed": after.public(),
                "frame": self.public_frame(),
            });
            capability
                .verify_change(request, &reply)
                .map_err(N64Error::BadState)?;
            Ok(reply)
        })();
        outcome.map_err(|error| {
            CONTROL_UNVERIFIED.store(true, Ordering::Release);
            N64Error::BadState(format!("execution_speed unverified: {error}; last verified policy: {}; guest progress may have occurred", before.public()))
        })
    }

    pub(super) fn read_memory_batch(&self, params: &Value) -> N64Result<Value> {
        let ranges = params
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or_else(|| N64Error::BadParams("ranges must be an array".into()))?
            .iter()
            .map(|range| {
                Ok(BatchRange {
                    memory_type: range
                        .get("memory_type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| N64Error::BadParams("memory_type is required".into()))?
                        .to_string(),
                    address: required_num(range, "address")?,
                    length: required_num(range, "length")?,
                })
            })
            .collect::<N64Result<Vec<_>>>()?;
        Self::memory_batch_capability()
            .admit(&ranges)
            .map_err(N64Error::BadParams)?;
        self.require_frozen("read_memory_batch")?;
        let frame = self.public_frame();
        let reads: Vec<Value> = ranges
            .iter()
            .enumerate()
            .map(|(index, range)| {
                let bytes: Vec<u8> = (0..range.length)
                    .map(|offset| unsafe {
                        (self.api.debug_mem_read8)((RDRAM_BASE + range.address + offset) as u32)
                    })
                    .collect();
                json!({
                    "index": index, "memory_type": range.memory_type, "address": range.address,
                    "length": range.length, "hex": hex::encode(bytes),
                })
            })
            .collect();
        let epoch = format!("f{frame}#s{}", BOUNDARY_SEQ.load(Ordering::Acquire));
        Ok(json!({
            "state": "frozen",
            "consistency": CONSISTENCY_FROZEN_BOUNDARY,
            "boundary": {
                "runtime_generation": self.launch_id.clone()
                    .unwrap_or_else(|| format!("unmanaged-pid:{}", std::process::id())),
                "stop_epoch": epoch,
                "memory_mapping_epoch": epoch,
                "clocks": [{
                    "domain": if self.display { "n64.rendered_frame" } else { "n64.vi" },
                    "value": frame,
                }],
            },
            "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
            "reads": reads,
        }))
    }
}

#[cfg(test)]
#[path = "n64_adapter_observation_tests.rs"]
mod tests;
