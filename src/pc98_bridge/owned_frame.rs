//! Correlated native frame operations; each RSP exchange completes before the next begins.
use super::*;
use crate::live::link::RequestCancellation;

impl<G: GdbTransport> Bridge<G> {
    /// No bare-OK skipping: an unrelated terminal cannot satisfy an owned exchange.
    pub(super) fn frame_exchange(&mut self, name: &str, argument: &str) -> BridgeResult<String> {
        self.frame_exchange_with_budget(name, argument, crate::mame_owned_frames::EXCHANGE_BUDGET)
    }

    fn frame_exchange_with_budget(
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
            self.note_stop(raw, false);
        }
        Ok(result?)
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
        if self.backend_terminal() || !self.frozen {
            return Err(BridgeError::BadState(
                "owned frames require healthy frozen control".into(),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(BridgeError::BadState(
                "frame operation cancelled before admission".into(),
            ));
        }
        if !self.mame_features().contains("owned_frames_v1") {
            return Err(BridgeError::BadState(
                "native owned_frames_v1 is required".into(),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(BridgeError::BadState(
                "frame operation cancelled before admission".into(),
            ));
        }
        let previous = self.gdb.get_timeout()?;
        let id = crate::live::temporal::fresh_identity();
        let result = crate::mame_owned_frames::run(self, &id, count, &cancellation);
        let restore = self.gdb.set_timeout(previous);
        match (result, restore) {
            (Ok(value), Ok(())) => Ok(value),
            (result, restore) => {
                let error = format!("owned frame control unverified: result={result:?}, timeout_restore={restore:?}");
                self.control_fatal = Some(error.clone());
                Err(BridgeError::Emulator(error))
            }
        }
    }
}

impl<G: GdbTransport> crate::mame_owned_frames::FrameHost for Bridge<G> {
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
