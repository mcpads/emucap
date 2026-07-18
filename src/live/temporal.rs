use std::time::{Duration, Instant};

/// Maximum work admitted to one synchronous frame/instruction advance. Longer travel is composed
/// from terminally acknowledged calls so a dropped host cannot leave one unbounded operation
/// advancing the emulator.
pub const MAX_SYNC_ADVANCE_COUNT: u64 = 5_000;

/// Backend-side deadline for bridges that execute an advance as repeated GDB/WebSocket requests.
/// It stays below the outer link's 300-second deferred deadline, leaving time for terminal cleanup
/// and the final response.
pub const MAX_SYNC_OPERATION_TIME: Duration = Duration::from_secs(250);

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
