pub(crate) mod wire;
pub mod owner;

use std::time::{Duration, Instant};

/// Maximum work admitted to one synchronous frame/instruction advance. Longer travel is composed
/// from terminally acknowledged calls so a dropped host cannot leave one unbounded operation
/// advancing the emulator.
pub const MAX_SYNC_ADVANCE_COUNT: u64 = 5_000;

/// Backend-side deadline for bridges that execute an advance as repeated GDB/WebSocket requests.
/// It stays below the outer link's 300-second deferred deadline, leaving time for terminal cleanup
/// and the final response.
pub const MAX_SYNC_OPERATION_MS: u64 = 250_000;
pub const MAX_SYNC_OPERATION_TIME: Duration = Duration::from_millis(MAX_SYNC_OPERATION_MS);

/// Monotonic wall-clock boundary shared by adapter-owned synchronous work.
///
/// `remaining_timeout` is intended to be applied to every blocking backend exchange. Checking only
/// between exchanges is insufficient: the final exchange could otherwise outlive the operation
/// budget and still be reported as completed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OperationDeadline {
    at: Instant,
}

impl OperationDeadline {
    pub(crate) fn after(budget: Duration) -> Self {
        Self {
            at: Instant::now() + budget,
        }
    }

    /// Remaining backend wait budget. A nonzero duration is rounded up to one millisecond because
    /// socket APIs reject a zero timeout and some platforms round sub-millisecond values down.
    pub(crate) fn remaining_timeout(self) -> Option<Duration> {
        self.at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.max(Duration::from_millis(1)))
    }

    pub(crate) fn expired(self) -> bool {
        Instant::now() >= self.at
    }
}

/// Finish a temporal operation only after its terminal cleanup has run.
///
/// A successful effect with failed cleanup is not a successful operation. If both the effect and
/// cleanup fail, `combine` preserves both causes in one fail-loud error.
pub fn finish_with_cleanup<T, E>(
    outcome: Result<T, E>,
    cleanup: Result<(), E>,
    combine: impl FnOnce(Option<E>, E) -> E,
) -> Result<T, E> {
    match (outcome, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup_error)) => Err(combine(None, cleanup_error)),
        (Err(primary), Ok(())) => Err(primary),
        (Err(primary), Err(cleanup_error)) => Err(combine(Some(primary), cleanup_error)),
    }
}

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod tests;

/// Producer-advertised methods whose native stop can be verified after a keyed cancellation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancellationCapability {
    pub methods: Vec<String>,
    pub control_service_ms: u64,
    pub stop_host_ms: u64,
}

impl CancellationCapability {
    pub fn from_hello(value: &serde_json::Value, methods: &[String]) -> Result<Self, String> {
        let capability: Self = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if capability.methods.is_empty()
            || capability.control_service_ms == 0
            || capability.control_service_ms > 50
            || capability.stop_host_ms < capability.control_service_ms
            || capability.stop_host_ms > 300_000
        {
            return Err("invalid temporal cancellation bounds".into());
        }
        let mut unique = std::collections::HashSet::new();
        for method in &capability.methods {
            if !matches!(method.as_str(), "step" | "step_instructions")
                || !methods.contains(method)
                || !unique.insert(method)
            {
                return Err("invalid temporal cancellation method".into());
            }
        }
        Ok(capability)
    }
}

pub(crate) fn fresh_identity() -> String {
    format!("{}{}", ulid::Ulid::generate(), ulid::Ulid::generate())
}
