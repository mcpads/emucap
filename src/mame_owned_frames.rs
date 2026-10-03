//! Shared MAME owned-frame wire contract and correlated stop state machine.
use crate::gdb_rsp::{GdbError, GdbResult, GdbTransport};
use crate::live::link::RequestCancellation;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

// A complete command includes its RSP acknowledgment and correlated reply.
// Native startup and Windows scheduling may exceed one polling interval. Each
// exchange borrows from the operation/cleanup deadline; poll cadence is separate.
pub(crate) const EXCHANGE_BUDGET: Duration = Duration::from_secs(2);

pub(crate) trait FrameHost {
    type Error: From<GdbError>;
    fn exchange(
        &mut self,
        name: &str,
        argument: &str,
        budget: Duration,
    ) -> Result<String, Self::Error>;
    fn set_frozen(&mut self, frozen: bool);
    fn error(message: String) -> Self::Error {
        GdbError::Emulator(message).into()
    }
}

pub(crate) fn exchange<G: GdbTransport>(
    gdb: &mut G,
    name: &str,
    argument: &str,
    budget: Duration,
    mut on_stop: impl FnMut(String),
) -> GdbResult<String> {
    let result = (|| {
        let deadline = Instant::now() + budget;
        let mut reply =
            gdb.send_with_timeout(&format!("qEmucap,{name},{}", hex::encode(argument)), budget)?;
        let mut stops = 0;
        while is_stop(&reply) {
            on_stop(reply);
            stops += 1;
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| {
                    GdbError::Emulator("owned frame exchange deadline expired".into())
                })?;
            if stops > 128 {
                return Err(GdbError::Emulator(
                    "owned frame stop stream exceeded its exchange bound".into(),
                ));
            }
            reply = gdb.recv_reply_with_timeout(remaining)?;
        }
        Ok(reply)
    })();
    result.map_err(|error| {
        let context = format!("MAME {name} exchange ({} ms): {error}", budget.as_millis());
        match error {
            GdbError::Io(error) => GdbError::Io(std::io::Error::new(error.kind(), context)),
            GdbError::Emulator(_) => GdbError::Emulator(context),
            GdbError::Poisoned => GdbError::Poisoned,
        }
    })
}

pub(crate) fn is_stop(reply: &str) -> bool {
    let bytes = reply.as_bytes();
    bytes.len() >= 3
        && matches!(bytes[0], b'S' | b'T')
        && bytes[1].is_ascii_hexdigit()
        && bytes[2].is_ascii_hexdigit()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FrameReply {
    phase: String,
    completed: u64,
    requested: u64,
    reason: String,
}

impl FrameReply {
    pub(crate) fn parse(raw: &str, id: &str, requested: u64) -> GdbResult<Self> {
        let fields: Vec<_> = raw.split('|').collect();
        let invalid = || GdbError::Emulator("unverified owned frame response".into());
        if fields.len() != 6 || fields[0] != "FRAME" || fields[1] != id {
            return Err(invalid());
        }
        let completed = fields[3].parse::<u64>().map_err(|_| invalid())?;
        let count = fields[4].parse::<u64>().map_err(|_| invalid())?;
        if count != requested
            || completed > count
            || !matches!(
                fields[2],
                "running" | "stopping" | "completed" | "interrupted"
            )
            || !matches!(
                fields[5],
                "none" | "cancelled" | "host_deadline" | "native_stop"
            )
            || (fields[2] == "completed" && (completed != count || fields[5] != "none"))
            || (fields[2] == "running" && fields[5] != "none")
            || (fields[2] == "interrupted" && fields[5] == "none")
        {
            return Err(invalid());
        }
        Ok(Self {
            phase: fields[2].into(),
            completed,
            requested: count,
            reason: fields[5].into(),
        })
    }
    pub(crate) fn terminal(&self) -> bool {
        matches!(self.phase.as_str(), "completed" | "interrupted")
    }
}

fn exchange_until<H: FrameHost>(
    host: &mut H,
    name: &str,
    argument: &str,
    deadline: Instant,
) -> Result<String, H::Error> {
    let budget = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| H::error("native frame exchange deadline expired".into()))?
        .min(EXCHANGE_BUDGET);
    host.exchange(name, argument, budget)
}

pub(crate) fn run<H: FrameHost>(
    host: &mut H,
    id: &str,
    count: u64,
    cancellation: &RequestCancellation,
) -> Result<Value, H::Error> {
    let deadline = Instant::now() + Duration::from_secs(240);
    let raw = exchange_until(
        host,
        "framebegin",
        &format!("{id}:{count}:240000"),
        deadline,
    )?;
    let mut reply = FrameReply::parse(&raw, id, count)?;
    if reply.phase != "running" || reply.completed != 0 {
        return Err(H::error(
            "native frame admission did not identify a new operation".into(),
        ));
    }
    host.set_frozen(false);
    let mut stop_deadline = None;
    let mut host_deadline = false;
    loop {
        let now = Instant::now();
        if stop_deadline.is_none() && (cancellation.is_cancelled() || now >= deadline) {
            host_deadline = now >= deadline;
            let start = cancellation.cancelled_at().unwrap_or(now);
            stop_deadline = Some(start + Duration::from_millis(2500));
            let raw = exchange_until(host, "framecancel", id, stop_deadline.unwrap())?;
            let cancelled = FrameReply::parse(&raw, id, count)?;
            if cancelled.completed < reply.completed {
                return Err(H::error("native cancel progress moved backwards".into()));
            }
            reply = cancelled;
        }
        if stop_deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(H::error(
                "native frame cancellation stop deadline expired".into(),
            ));
        }
        if reply.terminal() {
            let finished =
                exchange_until(host, "framefinish", id, stop_deadline.unwrap_or(deadline))?;
            if FrameReply::parse(&finished, id, count)? != reply {
                return Err(H::error(
                    "native frame terminal changed before ownership release".into(),
                ));
            }
            let frame = exchange_until(host, "frame", "", stop_deadline.unwrap_or(deadline))?
                .parse::<u64>()
                .map_err(|_| H::error("invalid native terminal frame clock".into()))?;
            if stop_deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(H::error(
                    "native frame cleanup exceeded stop deadline".into(),
                ));
            }
            host.set_frozen(true);
            let reason = if host_deadline && reply.reason == "cancelled" {
                "host_deadline"
            } else {
                &reply.reason
            };
            let mut result = json!({"status":reply.phase,"unit":"frames","frames":count,
                    "count":count,"completed":reply.completed,"state":"frozen","frame":frame,"native_operation_id":id});
            if reason != "none" {
                result["reason"] = json!(reason);
            }
            return Ok(result);
        }
        if stop_deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(H::error(
                "native frame cancellation stop deadline expired".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
        let raw = exchange_until(host, "framepoll", id, stop_deadline.unwrap_or(deadline))?;
        let next = FrameReply::parse(&raw, id, count)?;
        if next.completed < reply.completed {
            return Err(H::error("native frame progress moved backwards".into()));
        }
        reply = next;
        // An asynchronous non-pausing breakpoint does not replace native operation state.
        host.set_frozen(reply.terminal());
    }
}

#[cfg(test)]
#[path = "mame_owned_frames_tests.rs"]
mod tests;
