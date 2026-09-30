//! Common admission and reply verification for `read_memory_batch`.
//!
//! The producer owns the write barrier and the side-effect-free reads. This module only checks the
//! advertised static limits before any adapter work and verifies that a reply is the complete,
//! ordered answer to exactly the admitted request. It never splits a batch into single reads.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::link::LinkError;

pub const METHOD: &str = "read_memory_batch";
pub const CORE_MAX_RANGES: u64 = 64;
pub const CORE_MAX_TOTAL_BYTES: u64 = 64 * 1024;
pub const CONSISTENCY_FROZEN_BOUNDARY: &str = "frozen_boundary";

/// Static batch limits advertised in hello. Part of the capability revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBatchCapability {
    pub max_ranges: u64,
    pub max_range_bytes: u64,
    pub max_total_bytes: u64,
    pub consistency: String,
    /// Stop kinds at which the producer can exclude every writer of the advertised windows.
    pub halt_kinds: Vec<String>,
    /// Side-effect-free address windows. A batch range must lie inside one window of its type.
    pub windows: Vec<MemoryWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWindow {
    pub memory_type: String,
    pub address: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchRange {
    pub memory_type: String,
    pub address: u64,
    pub length: u64,
}

impl MemoryBatchCapability {
    pub fn from_hello(value: &Value, memory_types: &[String]) -> Result<Self, String> {
        let capability: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid memory_batch_capability: {error}"))?;
        capability.validate(memory_types)?;
        Ok(capability)
    }

    fn validate(&self, memory_types: &[String]) -> Result<(), String> {
        if !(1..=CORE_MAX_RANGES).contains(&self.max_ranges) {
            return Err(format!("max_ranges must be 1..={CORE_MAX_RANGES}"));
        }
        // The minimum useful batch is two discontiguous ranges.
        if self.max_ranges < 2 {
            return Err("max_ranges must admit at least two ranges".into());
        }
        if !(1..=CORE_MAX_TOTAL_BYTES).contains(&self.max_total_bytes) {
            return Err(format!(
                "max_total_bytes must be 1..={CORE_MAX_TOTAL_BYTES}"
            ));
        }
        if self.max_range_bytes == 0 || self.max_range_bytes > self.max_total_bytes {
            return Err("max_range_bytes must be 1..=max_total_bytes".into());
        }
        if self.consistency != CONSISTENCY_FROZEN_BOUNDARY {
            return Err(format!("consistency must be {CONSISTENCY_FROZEN_BOUNDARY}"));
        }
        if self.halt_kinds.is_empty() || self.halt_kinds.iter().any(String::is_empty) {
            return Err("halt_kinds must name at least one stop kind".into());
        }
        if self.windows.is_empty() {
            return Err("windows must name at least one side-effect-free window".into());
        }
        for window in &self.windows {
            if !memory_types.iter().any(|name| name == &window.memory_type) {
                return Err(format!(
                    "window memory_type {} is not advertised in memory_types",
                    window.memory_type
                ));
            }
            if window.length == 0 || window.address.checked_add(window.length).is_none() {
                return Err(format!(
                    "window {}@{} has an empty or overflowing length",
                    window.memory_type, window.address
                ));
            }
        }
        Ok(())
    }

    fn window_contains(&self, range: &BatchRange) -> bool {
        let Some(end) = range.address.checked_add(range.length) else {
            return false;
        };
        self.windows.iter().any(|window| {
            window.memory_type == range.memory_type
                && range.address >= window.address
                && end <= window.address + window.length
        })
    }

    /// Validate every range before the adapter receives any request. The error names the first
    /// rejected index so a caller can fix it without a partial read.
    pub fn admit(&self, ranges: &[BatchRange]) -> Result<(), String> {
        if ranges.is_empty() || ranges.len() as u64 > self.max_ranges {
            return Err(format!(
                "ranges must contain 1..={} entries",
                self.max_ranges
            ));
        }
        let mut total = 0u64;
        for (index, range) in ranges.iter().enumerate() {
            if range.length == 0 || range.length > self.max_range_bytes {
                return Err(format!(
                    "range {index} length must be 1..={}",
                    self.max_range_bytes
                ));
            }
            if !self.window_contains(range) {
                return Err(format!(
                    "range {index} ({} {:#x}+{}) is outside the advertised batch windows",
                    range.memory_type, range.address, range.length
                ));
            }
            total += range.length;
            if total > self.max_total_bytes {
                return Err(format!(
                    "ranges exceed max_total_bytes {}",
                    self.max_total_bytes
                ));
            }
        }
        Ok(())
    }
}

/// Verify a producer reply against the admitted request. Any mismatch invalidates the whole
/// result; no read from a mismatched reply is surfaced.
pub fn verify_reply(
    ranges: &[BatchRange],
    reply: &Value,
    expected_generation: Option<&str>,
) -> Result<(), LinkError> {
    let invalid =
        |reason: &str| LinkError::Protocol(format!("invalid memory batch reply: {reason}"));
    if reply.get("state").and_then(Value::as_str) != Some("frozen") {
        return Err(invalid("state is not frozen"));
    }
    if reply.get("consistency").and_then(Value::as_str) != Some(CONSISTENCY_FROZEN_BOUNDARY) {
        return Err(invalid("consistency is not frozen_boundary"));
    }
    verify_boundary(reply.get("boundary"), expected_generation)
        .map_err(|reason| invalid(&reason))?;
    let reads = reply
        .get("reads")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("reads is missing"))?;
    if reads.len() != ranges.len() {
        return Err(invalid("read count differs from the request"));
    }
    let mut total = 0u64;
    for (index, (read, range)) in reads.iter().zip(ranges).enumerate() {
        let matches = read.get("index").and_then(Value::as_u64) == Some(index as u64)
            && read.get("memory_type").and_then(Value::as_str) == Some(range.memory_type.as_str())
            && read.get("address").and_then(Value::as_u64) == Some(range.address)
            && read.get("length").and_then(Value::as_u64) == Some(range.length);
        if !matches {
            return Err(invalid(&format!(
                "read {index} descriptor differs from the request"
            )));
        }
        let hex_len = read
            .get("hex")
            .and_then(Value::as_str)
            .filter(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .map(str::len);
        if hex_len != Some(range.length as usize * 2) {
            return Err(invalid(&format!("read {index} payload length differs")));
        }
        total += range.length;
    }
    if reply.get("total_bytes").and_then(Value::as_u64) != Some(total) {
        return Err(invalid("total_bytes differs from the request"));
    }
    Ok(())
}

fn verify_boundary(
    boundary: Option<&Value>,
    expected_generation: Option<&str>,
) -> Result<(), String> {
    let boundary = boundary.ok_or("boundary is missing")?;
    let token = |name: &str| {
        boundary
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(format!("boundary.{name} is missing"))
    };
    let generation = token("runtime_generation")?;
    if expected_generation.is_some_and(|expected| expected != generation) {
        return Err("boundary.runtime_generation is not the connected generation".into());
    }
    token("stop_epoch")?;
    token("memory_mapping_epoch")?;
    let clocks = boundary
        .get("clocks")
        .and_then(Value::as_array)
        .ok_or("boundary.clocks is missing")?;
    for clock in clocks {
        let named = clock
            .get("domain")
            .and_then(Value::as_str)
            .is_some_and(|domain| !domain.is_empty());
        if !named || clock.get("value").and_then(Value::as_u64).is_none() {
            return Err("boundary clock requires a domain and an unsigned value".into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "memory_batch_tests.rs"]
mod tests;
