//! Frontend-owned host pacing and frozen memory batches.
//!
//! This frontend owns the only `retro_run` loop, so it is the pacing owner: running frames and
//! synchronous frame advances both start at deadlines derived from guest frames since an anchor.
//! The core is single-threaded, so while `retro_run` is not executing no guest writer is active.
use std::time::Instant;

use super::debug::{memory_region, required_num, required_str};
use super::*;
use crate::live::memory_batch::{self, BatchRange, MemoryBatchCapability, MemoryWindow};
use crate::live::pacing::{self, ExecutionSpeedCapability, SpeedRequest};

/// Absolute spans of the native peek view: directly backed memory below `CPU_MEMREADMAX`, the
/// B/R/G graphics planes and, in analog mode, the I plane, all read raw from the access page.
const PEEK_SPANS: [(u64, u64); 3] = [(0, 0xA4000), (0xA8000, 0xC0000), (0xE0000, 0xE8000)];
const HALT_KIND: &str = "frontend_frame_boundary";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PacingPolicy {
    Limited { centi_percent: u64 },
    Unlimited,
}

/// Deadline source for frame starts. The anchor pairs a host instant with the guest frame that
/// starts there; frozen time, policy changes and state replacement drop it so that neither a
/// catch-up burst nor a stale long sleep follows.
#[derive(Debug)]
pub(super) struct FramePacer {
    policy: PacingPolicy,
    revision: u64,
    anchor: Option<(Instant, u64)>,
}

/// Host lag, in target frame periods, after which the pacer re-anchors instead of catching up.
const MAX_LAG_PERIODS: u32 = 4;

impl FramePacer {
    pub(super) fn new() -> Self {
        Self {
            policy: PacingPolicy::Limited {
                centi_percent: 100 * 100,
            },
            revision: 1,
            anchor: None,
        }
    }

    pub(super) fn reanchor(&mut self) {
        self.anchor = None;
    }

    fn set(&mut self, policy: PacingPolicy) {
        self.policy = policy;
        self.revision += 1;
        self.anchor = None;
    }

    /// Host instant at which guest frame `frame` may start; `None` means no intentional wait.
    pub(super) fn frame_start(
        &mut self,
        frame: u64,
        frame_duration: Duration,
        now: Instant,
    ) -> Option<Instant> {
        let PacingPolicy::Limited { centi_percent } = self.policy else {
            return None;
        };
        let period = frame_duration.mul_f64(10_000.0 / centi_percent as f64);
        let (host, base) = *self.anchor.get_or_insert((now, frame));
        let target = host + period.mul_f64(frame.saturating_sub(base) as f64);
        if now > target + period * MAX_LAG_PERIODS {
            self.anchor = Some((now, frame));
            return Some(now);
        }
        Some(target)
    }
}

/// Why a synchronous frame advance stopped before its requested count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdvanceStop {
    Breakpoint,
    HostDeadline,
}

impl AdvanceStop {
    pub(super) fn reason(stop: Option<Self>) -> Value {
        match stop {
            None => Value::Null,
            Some(Self::Breakpoint) => json!("breakpoint"),
            Some(Self::HostDeadline) => json!("host_deadline"),
        }
    }
}

impl Np2kaiHost {
    pub(super) fn memory_batch_capability() -> MemoryBatchCapability {
        // Each memory type's offsets that fall inside the peek view.
        let windows = MEMORY_REGIONS
            .iter()
            .flat_map(|region| {
                let base = u64::from(region.base);
                let end = base + u64::from(region.size);
                PEEK_SPANS.iter().filter_map(move |(start, stop)| {
                    let (low, high) = (base.max(*start), end.min(*stop));
                    (low < high).then(|| MemoryWindow {
                        memory_type: region.name.into(),
                        address: low - base,
                        length: high - low,
                    })
                })
            })
            .collect();
        MemoryBatchCapability {
            max_ranges: memory_batch::CORE_MAX_RANGES,
            max_range_bytes: 0x4000,
            max_total_bytes: memory_batch::CORE_MAX_TOTAL_BYTES,
            consistency: memory_batch::CONSISTENCY_FROZEN_BOUNDARY.into(),
            halt_kinds: vec![HALT_KIND.into()],
            windows,
        }
    }

    pub(super) fn execution_speed_capability() -> ExecutionSpeedCapability {
        ExecutionSpeedCapability {
            modes: vec!["limited".into(), "unlimited".into()],
            percent: pacing::PercentDomain {
                min: Some(0.01),
                max: Some(10000.0),
                quantum: Some(0.01),
                values: None,
            },
            states: vec!["running".into(), "frozen".into()],
            scope: pacing::SCOPE_HOST_PACING.into(),
            source: "frontend".into(),
            control_service_ms: 50,
            host_constraints: vec![],
        }
    }

    pub(super) fn pacing_policy_value(&self) -> Value {
        let (mode, percent) = match self.pacer.policy {
            PacingPolicy::Limited { centi_percent } => {
                ("limited", pacing::percent_value(centi_percent))
            }
            PacingPolicy::Unlimited => ("unlimited", Value::Null),
        };
        json!({
            "mode": mode, "percent": percent, "source": "frontend",
            "policy_revision": self.pacer.revision.to_string(), "host_constraints": [],
        })
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> Np2kaiResult<Value> {
        let mode = match params.get("mode") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| Np2kaiError::BadParams("mode must be a string".into()))?,
            ),
        };
        let percent = match params.get("percent") {
            None => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .ok_or_else(|| Np2kaiError::BadParams("percent must be a number".into()))?,
            ),
        };
        let request = pacing::parse_request(mode, percent).map_err(Np2kaiError::BadParams)?;
        Self::execution_speed_capability()
            .admit(request)
            .map_err(Np2kaiError::BadParams)?;
        let policy = match request {
            SpeedRequest::Query => return Ok(self.pacing_policy_value()),
            SpeedRequest::Unlimited => PacingPolicy::Unlimited,
            SpeedRequest::Limited { centi_percent } => PacingPolicy::Limited { centi_percent },
        };
        let previous = self.pacing_policy_value();
        // Applied between frames on the only execution thread; no frame runs here.
        self.pacer.set(policy);
        Ok(json!({
            "status": "completed",
            "state": if self.frozen { "frozen" } else { "running" },
            "previous": previous,
            "execution_speed": self.pacing_policy_value(),
            "frame": self.frame,
        }))
    }

    /// Run up to `count` frames, each starting at its pacing deadline, inside the synchronous
    /// operation budget. A frame whose start would pass the budget is not run.
    pub(super) fn run_paced_frames(
        &mut self,
        count: u64,
        mut per_frame: impl FnMut(&mut Self) -> Np2kaiResult<bool>,
    ) -> Np2kaiResult<(u64, Option<AdvanceStop>)> {
        let budget = Instant::now() + crate::live::temporal::MAX_SYNC_OPERATION_TIME;
        let frame_duration = self.frame_duration();
        self.pacer.reanchor();
        let mut completed = 0;
        let mut stop = None;
        while completed < count {
            if let Some(start) = self
                .pacer
                .frame_start(self.frame, frame_duration, Instant::now())
            {
                if start > budget {
                    stop = Some(AdvanceStop::HostDeadline);
                    break;
                }
                std::thread::sleep(start.saturating_duration_since(Instant::now()));
            }
            if !per_frame(self)? {
                stop = Some(AdvanceStop::Breakpoint);
                break;
            }
            completed += 1;
        }
        self.pacer.reanchor();
        Ok((completed, stop))
    }

    /// Next running-frame start for the frontend loop; `None` runs without an intentional wait.
    pub fn next_frame_start(&mut self, now: Instant) -> Option<Instant> {
        let frame_duration = self.frame_duration();
        self.pacer.frame_start(self.frame, frame_duration, now)
    }

    pub(super) fn read_memory_batch(&mut self, params: &Value) -> Np2kaiResult<Value> {
        let ranges = params
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or_else(|| Np2kaiError::BadParams("ranges must be an array".into()))?
            .iter()
            .map(|range| {
                Ok(BatchRange {
                    memory_type: required_str(range, "memory_type")?.to_string(),
                    address: required_num(range, "address")?,
                    length: required_num(range, "length")?,
                })
            })
            .collect::<Np2kaiResult<Vec<_>>>()?;
        Self::memory_batch_capability()
            .admit(&ranges)
            .map_err(Np2kaiError::BadParams)?;
        self.require_frozen("read_memory_batch")?;
        if !self.initialized {
            return Err(Np2kaiError::BadState(
                "read_memory_batch requires an initialized machine; step once first".into(),
            ));
        }
        let mut reads = Vec::with_capacity(ranges.len());
        for (index, range) in ranges.iter().enumerate() {
            let region = memory_region(&range.memory_type).ok_or_else(|| {
                Np2kaiError::BadParams(format!("unknown memory_type {}", range.memory_type))
            })?;
            let bytes = self
                .peek_absolute(region.base + range.address as u32, range.length as usize)
                .ok_or_else(|| {
                    Np2kaiError::BadState(format!(
                        "range {index} is outside the current peek view; the I plane needs analog mode"
                    ))
                })?;
            reads.push(json!({"index": index, "memory_type": range.memory_type,
                "address": range.address, "length": range.length, "hex": hex::encode(bytes)}));
        }
        let epoch = format!("f{}#s{}", self.frame, self.boundary_seq);
        Ok(json!({
            "state": "frozen",
            "consistency": memory_batch::CONSISTENCY_FROZEN_BOUNDARY,
            "boundary": {
                "runtime_generation": self.launch_id.clone()
                    .unwrap_or_else(|| format!("unmanaged-pid:{}", std::process::id())),
                "stop_epoch": epoch,
                "memory_mapping_epoch": epoch,
                "clocks": [{"domain": "np2kai.retro_run_frame", "value": self.frame}],
            },
            "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
            "reads": reads,
        }))
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
