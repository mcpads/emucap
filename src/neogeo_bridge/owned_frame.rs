//! Neo Geo ownership admission and bounded breakpoint completion around shared MAME frames.
use super::*;
use crate::live::link::RequestCancellation;
use std::time::Instant;

impl<G: GdbTransport> NeoGeoBridge<G> {
    pub(super) fn note_owned_stop(&mut self, packet: String) {
        if is_breakpoint_stop(&packet) {
            self.owned_stops.push(packet);
        }
    }
    pub(super) fn frame_exchange(&mut self, name: &str, argument: &str) -> BridgeResult<String> {
        self.frame_exchange_with_budget(name, argument, crate::mame_owned_frames::EXCHANGE_BUDGET)
    }
    pub(super) fn frame_exchange_with_budget(
        &mut self,
        name: &str,
        argument: &str,
        budget: Duration,
    ) -> BridgeResult<String> {
        let mut stops = Vec::new();
        let result =
            crate::mame_owned_frames::exchange(&mut self.gdb, name, argument, budget, |raw| {
                stops.push(raw)
            });
        for raw in stops {
            self.note_owned_stop(raw);
        }
        Ok(result?)
    }
    fn owned_budget(&self) -> BridgeResult<Option<Duration>> {
        self.owned_reply_deadline
            .map(|deadline| {
                deadline
                    .checked_duration_since(Instant::now())
                    .filter(|d| !d.is_zero())
                    .ok_or_else(|| {
                        BridgeError::Emulator("owned breakpoint completion deadline expired".into())
                    })
            })
            .transpose()
    }
    pub(super) fn owned_aware_send(&mut self, payload: &str) -> BridgeResult<String> {
        Ok(if let Some(budget) = self.owned_budget()? {
            self.gdb.send_with_timeout(payload, budget)?
        } else {
            self.gdb.send(payload)?
        })
    }
    pub(super) fn owned_aware_poll(&mut self) -> BridgeResult<Option<String>> {
        Ok(if let Some(budget) = self.owned_budget()? {
            self.gdb.recv_nonblocking_with_timeout(budget)?
        } else {
            self.gdb.recv_nonblocking()?
        })
    }
    pub(super) fn finish_owned_stops(&mut self) -> BridgeResult<Option<Value>> {
        if self.owned_stops.is_empty() {
            return Ok(None);
        }
        let previous = self.owned_reply_deadline;
        self.owned_reply_deadline =
            Some(previous.unwrap_or_else(|| Instant::now() + Duration::from_millis(500)));
        let result = (|| {
            let mut last = None;
            for stop in std::mem::take(&mut self.owned_stops) {
                last = Some(self.record_breakpoint_hit(stop)?);
                self.owned_budget()?;
            }
            if self.backend_terminal() {
                return Err(BridgeError::Emulator(
                    "native breakpoint completion lost control".into(),
                ));
            }
            Ok(last)
        })();
        self.owned_reply_deadline = previous;
        if let Err(error) = &result {
            self.control_fatal = Some(error.to_string());
        }
        result
    }
    pub(super) fn owned_frame_step(
        &mut self,
        count: u64,
        cancellation: RequestCancellation,
    ) -> BridgeResult<Value> {
        if count == 0 || count > crate::live::temporal::MAX_SYNC_ADVANCE_COUNT {
            return Err(BridgeError::BadParams(
                "owned frame count is outside the synchronous domain".into(),
            ));
        }
        if self.backend_terminal() || !self.owned_control || !self.frozen {
            return Err(BridgeError::BadState(
                "owned frames require healthy frozen control".into(),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(BridgeError::Cancelled);
        }
        let result = (|| {
            self.owned_reply_deadline = Some(Instant::now() + Duration::from_millis(500));
            let drained = self.drain_breakpoint_packets();
            self.owned_reply_deadline = None;
            drained?;
            let before = self
                .frame_exchange_with_budget("frame", "", crate::mame_owned_frames::EXCHANGE_BUDGET)?
                .parse::<u64>()
                .map_err(|_| BridgeError::Emulator("invalid native start frame clock".into()))?;
            if cancellation.is_cancelled() {
                return Err(BridgeError::Cancelled);
            }
            let id = crate::live::temporal::fresh_identity();
            let mut value = crate::mame_owned_frames::run(self, &id, count, &cancellation)?;
            let after = value["frame"].as_u64().expect("shared frame clock");
            let delta = after.checked_sub(before);
            if delta != value["completed"].as_u64() {
                return Err(BridgeError::Emulator(
                    "owned frame progress differs from native clock delta".into(),
                ));
            }
            value["frame_before"] = json!(before);
            value["frame_counter_delta"] = json!(delta);
            value["frame_counter_continuous"] = json!(true);
            if value["status"] == "completed" {
                value["frames_observed_min"] = json!(count);
            }
            if let Some(event) = self.finish_owned_stops()? {
                value["status"] = json!("interrupted");
                value["reason"] = json!("breakpoint");
                value["breakpoint_id"] = event["id"].clone();
                value["event"] = event;
            }
            Ok(value)
        })();
        if let Err(error) = &result {
            if !matches!(error, BridgeError::Cancelled) {
                self.control_fatal = Some(error.to_string());
            }
        }
        result
    }
}
impl<G: GdbTransport> crate::mame_owned_frames::FrameHost for NeoGeoBridge<G> {
    type Error = BridgeError;
    fn exchange(&mut self, name: &str, argument: &str, budget: Duration) -> BridgeResult<String> {
        self.frame_exchange_with_budget(name, argument, budget)
    }
    fn set_frozen(&mut self, frozen: bool) {
        self.frozen = frozen;
    }
}

#[cfg(test)]
#[path = "owned_frame_tests.rs"]
mod tests;
