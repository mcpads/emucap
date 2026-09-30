//! Shared MAME Lua-plugin encoding for batched peeks and native pacing.
//!
//! The emucap GDB plugin runs on MAME's emulation thread. It applies and reads back MAME's own
//! governor (`throttled`, `speed_factor`) in one call and reads memory with device side effects
//! disabled. This module only encodes requests and decodes replies for the PC-98 and Neo Geo
//! bridges, which share that plugin.
use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::live::memory_batch::{BatchRange, MemoryBatchCapability, MemoryWindow};
use crate::live::pacing::{self, ExecutionSpeedCapability, SpeedRequest};

pub(crate) const FEATURE_PEEK: &str = "peek_block";
pub(crate) const FEATURE_THROTTLE_HOOK: &str = "throttle_wait_hook";
pub(crate) const FEATURE_FASTFORWARD: &str = "fastforward_readback";
const HALT_KIND: &str = "debugger_stop_and_machine_pause";
/// Wall budget assumed for one guest frame at 100 percent when estimating a paced advance. Every
/// supported MAME screen refreshes at 50 Hz or faster.
const MAX_FRAME_PERIOD_MS: u64 = 20;

pub(crate) fn parse_features(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn supports_batch(features: &BTreeSet<String>) -> bool {
    features.contains(FEATURE_PEEK)
}

pub(crate) fn supports_pacing(features: &BTreeSet<String>) -> bool {
    features.contains(FEATURE_THROTTLE_HOOK) && features.contains(FEATURE_FASTFORWARD)
}

pub(crate) fn batch_capability(windows: Vec<MemoryWindow>) -> MemoryBatchCapability {
    MemoryBatchCapability {
        max_ranges: crate::live::memory_batch::CORE_MAX_RANGES,
        max_range_bytes: crate::live::memory_batch::CORE_MAX_TOTAL_BYTES,
        max_total_bytes: crate::live::memory_batch::CORE_MAX_TOTAL_BYTES,
        consistency: crate::live::memory_batch::CONSISTENCY_FROZEN_BOUNDARY.into(),
        halt_kinds: vec![HALT_KIND.into()],
        windows,
    }
}

/// MAME's `speed_factor` is an integer per mille, so the domain is 0.1 percent steps.
pub(crate) fn speed_capability() -> ExecutionSpeedCapability {
    ExecutionSpeedCapability {
        modes: vec!["limited".into(), "unlimited".into()],
        percent: pacing::PercentDomain {
            min: Some(0.1),
            max: Some(10000.0),
            quantum: Some(0.1),
            values: None,
        },
        states: vec!["running".into(), "frozen".into()],
        scope: pacing::SCOPE_HOST_PACING.into(),
        source: "native".into(),
        control_service_ms: 10,
        host_constraints: vec![],
    }
}

/// `addr:len,...` in hex, with absolute addresses already resolved by the bridge.
pub(crate) fn peek_spec(absolute: &[(u64, u64)]) -> String {
    absolute
        .iter()
        .map(|(address, length)| format!("{address:x}:{length:x}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Decoded `OK|<seq>@<time>|<frame>|<hex>,<hex>...` reply.
pub(crate) struct PeekReply {
    pub(crate) epoch: String,
    pub(crate) frame: u64,
    pub(crate) payloads: Vec<String>,
}

pub(crate) fn parse_peek_reply(raw: &str, ranges: &[BatchRange]) -> Option<PeekReply> {
    let mut fields = raw.splitn(4, '|');
    if fields.next()? != "OK" {
        return None;
    }
    let epoch = fields
        .next()
        .filter(|epoch| epoch.contains('@'))?
        .to_string();
    let frame = fields.next()?.parse().ok()?;
    let payloads: Vec<String> = fields.next()?.split(',').map(str::to_string).collect();
    let exact = payloads.len() == ranges.len()
        && payloads.iter().zip(ranges).all(|(hex, range)| {
            hex.len() as u64 == range.length * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit())
        });
    exact.then_some(PeekReply {
        epoch,
        frame,
        payloads,
    })
}

pub(crate) fn batch_reply(
    ranges: &[BatchRange],
    peek: PeekReply,
    runtime_generation: String,
) -> Value {
    let reads: Vec<_> = ranges
        .iter()
        .zip(&peek.payloads)
        .enumerate()
        .map(|(index, (range, hex))| {
            json!({"index": index, "memory_type": range.memory_type, "address": range.address,
                "length": range.length, "hex": hex})
        })
        .collect();
    json!({
        "state": "frozen",
        "consistency": crate::live::memory_batch::CONSISTENCY_FROZEN_BOUNDARY,
        "boundary": {
            "runtime_generation": runtime_generation,
            "stop_epoch": peek.epoch,
            "memory_mapping_epoch": peek.epoch,
            "clocks": [{"domain": "mame.screen_frame", "value": peek.frame}],
        },
        "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
        "reads": reads,
    })
}

/// One observed native governor tuple: `rev|throttled|speed|rate|fastforward|refreshspeed|period`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MamePacing {
    pub(crate) revision: u64,
    pub(crate) throttled: bool,
    pub(crate) speed_per_mille: u64,
    pub(crate) rate: String,
    pub(crate) fastforward: bool,
    pub(crate) refresh_speed: bool,
    pub(crate) frame_period: f64,
}

impl MamePacing {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let fields: Vec<_> = raw.split('|').collect();
        let [revision, throttled, speed, rate, fastforward, refresh_speed, period] = fields[..]
        else {
            return None;
        };
        let flag = |value: &str| match value {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        };
        rate.parse::<f64>().ok().filter(|rate| rate.is_finite())?;
        Some(Self {
            revision: revision.parse().ok()?,
            throttled: flag(throttled)?,
            speed_per_mille: speed.parse().ok()?,
            rate: rate.into(),
            fastforward: flag(fastforward)?,
            refresh_speed: flag(refresh_speed)?,
            frame_period: period
                .parse()
                .ok()
                .filter(|p: &f64| p.is_finite() && *p >= 0.0)?,
        })
    }

    fn unit_rate(&self) -> bool {
        self.rate.parse::<f64>().ok() == Some(1.0)
    }

    pub(crate) fn same_settings(&self, other: &Self) -> bool {
        self.throttled == other.throttled
            && self.speed_per_mille == other.speed_per_mille
            && self.rate.parse::<f64>().ok() == other.rate.parse::<f64>().ok()
    }

    /// Effective common policy. Fast-forward, refresh-rate speed adjustment and a non-unit throttle
    /// rate each replace the plain target, so they are reported as custom.
    pub(crate) fn public(&self, capability: &ExecutionSpeedCapability) -> Value {
        let overridden = self.fastforward || self.refresh_speed || !self.unit_rate();
        let centi = self.speed_per_mille * 10;
        let (mode, percent) = if overridden {
            ("custom", Value::Null)
        } else if !self.throttled {
            ("unlimited", Value::Null)
        } else if capability.percent.contains(centi) {
            ("limited", pacing::percent_value(centi))
        } else {
            ("custom", Value::Null)
        };
        json!({
            "mode": mode, "percent": percent, "source": capability.source,
            "policy_revision": self.revision.to_string(),
            "host_constraints": capability.host_constraints,
            "diagnostics": {"throttled": self.throttled, "speed_factor": self.speed_per_mille,
                "throttle_rate": self.rate, "fastforward": self.fastforward,
                "refresh_speed": self.refresh_speed},
        })
    }

    /// Wall budget per guest frame under this policy, for deadline estimates.
    pub(crate) fn frame_budget_ms(&self, base_ms: u64) -> u64 {
        if !self.throttled || self.fastforward || self.speed_per_mille == 0 {
            return base_ms;
        }
        base_ms + (MAX_FRAME_PERIOD_MS * 1000).div_ceil(self.speed_per_mille)
    }

    pub(crate) fn restore_spec(&self, expected_revision: u64) -> String {
        format!(
            "{expected_revision}|{}|{}|{}",
            u8::from(self.throttled),
            self.speed_per_mille,
            self.rate
        )
    }
}

pub(crate) fn set_spec(request: SpeedRequest) -> Option<String> {
    match request {
        SpeedRequest::Query => None,
        SpeedRequest::Unlimited => Some("unlimited".into()),
        SpeedRequest::Limited { centi_percent } => Some(format!("limited|{}", centi_percent / 10)),
    }
}

/// Decoded `previous;applied;boundary_before;boundary_after` transaction reply.
pub(crate) struct SetReply {
    pub(crate) previous: MamePacing,
    pub(crate) applied: MamePacing,
    pub(crate) boundary_kept: bool,
}

pub(crate) fn parse_set_reply(raw: &str) -> Option<SetReply> {
    let fields: Vec<_> = raw.split(';').collect();
    let [previous, applied, before, after] = fields[..] else {
        return None;
    };
    Some(SetReply {
        previous: MamePacing::parse(previous)?,
        applied: MamePacing::parse(applied)?,
        boundary_kept: before == after,
    })
}

/// Result of a frame-wait operation that carried a host deadline.
pub(crate) fn deadline_frames(raw: &str) -> Option<u64> {
    raw.strip_prefix("DEADLINE:")?.parse().ok()
}

#[cfg(test)]
#[path = "mame_observation_tests.rs"]
mod tests;
