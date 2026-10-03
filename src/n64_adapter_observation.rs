//! Frozen RDRAM batches and the versioned Mupen64Plus native pacing owner.

use super::*;

pub(super) static CONTROL_UNVERIFIED: AtomicBool = AtomicBool::new(false);
use crate::live::memory_batch::{
    BatchRange, MemoryBatchCapability, MemoryWindow, CONSISTENCY_FROZEN_BOUNDARY,
};
use crate::live::pacing::{self, ExecutionSpeedCapability, PercentDomain, SpeedRequest};

/// The core's own speed-factor domain.
const PACING_MIN_PERCENT: u64 = 1;
const PACING_MAX_PERCENT: u64 = 1_000;
const BATCH_MAX_RANGES: u64 = 64;
const BATCH_MAX_BYTES: u64 = 65_536;

/// Bumped by every request outside the observation set, since those can change memory or the
/// stop without changing the frame counter.
pub(super) static BOUNDARY_SEQ: AtomicU64 = AtomicU64::new(0);
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
    fast_forward: bool,
    netplay: bool,
}

/// Version 1 native ABI. Rust never synthesizes this owner's revision.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct NativePolicy {
    factor: i32,
    limiter: i32,
    fast_forward: i32,
    netplay: i32,
    netplay_lag: i32,
    reserved: u32,
    revision: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct NativePacingResult {
    version: u32,
    outcome: u32,
    previous: NativePolicy,
    applied: NativePolicy,
}

impl NativePolicy {
    fn pacing(self) -> N64Result<N64Pacing> {
        if !(1..=1000).contains(&self.factor)
            || ![
                self.limiter,
                self.fast_forward,
                self.netplay,
                self.netplay_lag,
            ]
            .into_iter()
            .all(|flag| matches!(flag, 0 | 1))
            || self.reserved != 0
        {
            return Err(N64Error::BadState("invalid native pacing snapshot".into()));
        }
        Ok(N64Pacing {
            factor: self.factor,
            limiter: self.limiter != 0,
            revision: self.revision,
            fast_forward: self.fast_forward != 0,
            netplay: self.netplay != 0,
        })
    }

    fn same_policy(self, other: Self) -> bool {
        Self {
            revision: 0,
            ..self
        } == Self {
            revision: 0,
            ..other
        }
    }
}

impl NativePacingResult {
    fn validate(self, operation: u32) -> N64Result<Self> {
        if self.version != 1 || self.outcome > 1 {
            return Err(N64Error::BadState(
                "unsupported native pacing response".into(),
            ));
        }
        self.previous.pacing()?;
        self.applied.pacing()?;
        if operation == 0 || self.outcome == 1 {
            if self.previous != self.applied || (operation == 0 && self.outcome != 0) {
                return Err(N64Error::BadState(
                    "native pacing rejection/query changed policy".into(),
                ));
            }
        } else {
            let changed = !self.previous.same_policy(self.applied);
            if self.applied.revision != self.previous.revision.wrapping_add(u64::from(changed)) {
                return Err(N64Error::BadState("invalid native pacing revision".into()));
            }
        }
        Ok(self)
    }
}

impl N64Pacing {
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
            "host_constraints": if self.netplay { vec!["netplay"] } else { vec![] },
            "diagnostics": {"speed_factor": self.factor, "speed_limiter": self.limiter, "fast_forward": self.fast_forward},
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

    fn pacing_transaction(&self, operation: u32, percent: i32) -> N64Result<NativePacingResult> {
        self.require_connected()?;
        let mut result = NativePacingResult::default();
        check_core("CoreEmucapPacing", unsafe {
            (self.api.core_emucap_pacing)(
                1,
                operation,
                percent,
                &mut result,
                std::mem::size_of::<NativePacingResult>() as u32,
            )
        })?;
        Ok(result)
    }

    pub(super) fn native_pacing(&self) -> N64Result<N64Pacing> {
        self.pacing_transaction(0, 0)?.validate(0)?.applied.pacing()
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
        let frame_before = self.public_frame();
        let before = self.native_pacing()?;
        if matches!(request, SpeedRequest::Query) {
            return Ok(before.public());
        }
        let mut unchanged_rejection = false;
        let mut native_result = None;
        let outcome: N64Result<Value> = (|| {
            let percent = match request {
                SpeedRequest::Query => unreachable!("handled before mutation"),
                SpeedRequest::Unlimited => 0,
                SpeedRequest::Limited { centi_percent } => (centi_percent / 100) as i32,
            };
            let raw = self.pacing_transaction(1, percent)?;
            native_result = Some(format!("{raw:?}"));
            let native = raw.validate(1)?;
            if native.outcome == 1 {
                unchanged_rejection = true;
                return Err(N64Error::BadState(format!(
                    "native pacing change rejected without mutation: {}",
                    native.previous.pacing()?.public()
                )));
            }
            let previous = native.previous.pacing()?;
            let after = native.applied.pacing()?;
            if after.fast_forward || after.netplay {
                return Err(N64Error::BadState(
                    "native pacing override remains active".into(),
                ));
            }
            // The emulation thread can hit a breakpoint while native pacing is applied.
            self.drain_debug_update()?;
            self.frozen = (self.frame_paused && frame_gate_is_blocked())
                || unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) }
                    == M64P_DBG_RUNSTATE_PAUSED;
            let reply = json!({
                "status": "completed",
                "state": if self.frozen { "frozen" } else { "running" },
                "previous": previous.public(),
                "execution_speed": after.public(),
                "frame": self.public_frame(),
            });
            capability
                .verify_change(request, &reply)
                .map_err(N64Error::BadState)?;
            Ok(reply)
        })();
        outcome.map_err(|error| {
            if unchanged_rejection { return error; }
            CONTROL_UNVERIFIED.store(true, Ordering::Release);
            let domain = if self.display { "n64.rendered_frame" } else { "n64.vi" };
            N64Error::BadState(format!("execution_speed unverified: {error}; last verified policy: {}; pre-command frame: {frame_before}; current frame: {}; frame domain: {domain}; native transaction: {native_result:?}; guest progress may have occurred", before.public(), self.public_frame()))
        })
    }

    pub(super) fn read_rdram_bytes(&self, offset: u64, length: u64) -> N64Result<Vec<u8>> {
        if !matches!(offset.checked_add(length), Some(end) if end <= RDRAM_SIZE) {
            return Err(N64Error::BadParams("RDRAM read exceeds storage".into()));
        }
        let mut bytes = vec![0; length as usize];
        check_core("DebugMemReadRdram", unsafe {
            (self.api.debug_mem_read_rdram)(offset as u32, bytes.as_mut_ptr(), length as u32)
        })?;
        Ok(bytes)
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
                let bytes = self.read_rdram_bytes(range.address, range.length)?;
                Ok(json!({
                    "index": index, "memory_type": range.memory_type, "address": range.address,
                    "length": range.length, "hex": hex::encode(bytes),
                }))
            })
            .collect::<N64Result<_>>()?;
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
