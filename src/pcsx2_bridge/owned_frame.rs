//! Correlated PINE frame operations. Only a revalidated Finished receipt releases ownership.
use super::*;
use std::time::Instant;

pub(super) const STOP_BUDGET: Duration = Duration::from_millis(5000);
const FRAME_EXCHANGE: Duration = Duration::from_millis(35);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const PROTOCOL: u8 = 0x98;
const BEGIN: u8 = 0x99;
const POLL: u8 = 0x9a;
const CANCEL: u8 = 0x9b;
const FINISH: u8 = 0x9c;
const PAUSE: u8 = 0x9d;

pub(super) fn remaining(deadline: Instant) -> BridgeResult<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|v| !v.is_zero())
        .ok_or_else(|| Pcsx2BridgeError::Emulator("native operation deadline expired".into()))
}
fn invalid(message: &str) -> Pcsx2BridgeError {
    Pcsx2BridgeError::Protocol(message.into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Receipt {
    id: u64,
    generation: u64,
    start: u64,
    end: u64,
    requested: u32,
    count: u32,
    phase: u32,
    reason: u32,
}
impl Receipt {
    fn parse(bytes: &[u8], requested: u32) -> BridgeResult<Self> {
        if bytes.len() != 48 {
            return Err(invalid("invalid native frame receipt size"));
        }
        let mut cursor = SliceCursor::new(bytes);
        let r = Self {
            id: cursor.u64()?,
            generation: cursor.u64()?,
            start: cursor.u64()?,
            end: cursor.u64()?,
            requested: cursor.u32()?,
            count: cursor.u32()?,
            phase: cursor.u32()?,
            reason: cursor.u32()?,
        };
        if r.phase == 6 {
            return Err(Pcsx2BridgeError::Emulator(format!(
                "native frame owner fault: {}",
                r.reason
            )));
        }
        if r.id == 0
            || r.generation == 0
            || r.requested != requested
            || r.phase > 5
            || r.reason > 3
            || r.count > r.requested
            || (requested == 0 && matches!(r.phase, 1 | 2))
            || r.end.checked_sub(r.start) != Some(u64::from(r.count))
            || (r.phase == 0 && (r.start != 0 || r.end != 0 || r.reason != 0))
            || (r.phase >= 3 && ((r.reason == 0) != (r.count == r.requested)))
        {
            return Err(invalid("inconsistent native frame receipt"));
        }
        Ok(r)
    }
    fn follows(&self, previous: &Self) -> BridgeResult<()> {
        if self.id != previous.id
            || self.generation != previous.generation
            || self.requested != previous.requested
            || self.phase < previous.phase
            || self.count < previous.count
            || (previous.phase != 0 && self.start != previous.start)
            || (previous.phase >= 3
                && (self.count != previous.count || self.reason != previous.reason))
        {
            return Err(invalid(
                "native frame receipt changed operation or terminal",
            ));
        }
        Ok(())
    }
}

impl<T: PineTransport> Pcsx2Bridge<T> {
    pub fn enable_owned_control(&mut self) -> BridgeResult<()> {
        let reply = self.bounded_command(
            PROTOCOL,
            &[],
            Instant::now() + Duration::from_secs(1),
            FRAME_EXCHANGE,
        )?;
        let mut expected = Vec::new();
        for value in [
            1u32,
            1,
            MAX_SYNC_ADVANCE_COUNT as u32,
            MAX_SYNC_OPERATION_MS as u32,
        ] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        if reply != expected {
            return Err(invalid(
                "PCSX2 owned frame extension revision/domain/limits mismatch",
            ));
        }
        self.owned_control = true;
        Ok(())
    }

    pub(super) fn bounded_command(
        &mut self,
        opcode: u8,
        body: &[u8],
        deadline: Instant,
        cap: Duration,
    ) -> BridgeResult<Vec<u8>> {
        if self.backend_terminal() {
            return Err(invalid("native control is retired"));
        }
        let deadline = self
            .io_deadline
            .map_or(deadline, |limit| limit.min(deadline));
        let budget = match remaining(deadline) {
            Ok(remaining) => remaining.min(cap),
            Err(error) => {
                self.control_unverified = true;
                return Err(error);
            }
        };
        let exchange_end = Instant::now() + budget;
        let mut request = vec![opcode];
        request.extend_from_slice(body);
        let reply = self
            .pine
            .transact_with_timeout(&request, budget)
            .inspect_err(|error| {
                if matches!(
                    error,
                    Pcsx2BridgeError::Io(_) | Pcsx2BridgeError::Protocol(_)
                ) {
                    self.control_unverified = true;
                }
            })?;
        if let Err(error) = remaining(exchange_end) {
            self.control_unverified = true;
            return Err(error);
        }
        Ok(reply)
    }

    pub(super) fn remember_cancel_deadline(&mut self) {
        if let Some(at) = self
            .request_cancellation
            .as_ref()
            .and_then(|token| token.cancelled_at())
        {
            let limit = at + STOP_BUDGET;
            self.native_stop_deadline = Some(
                self.native_stop_deadline
                    .map_or(limit, |old| old.min(limit)),
            );
        }
    }

    pub(super) fn cleanup_deadline(&mut self) -> Instant {
        self.remember_cancel_deadline();
        *self
            .native_stop_deadline
            .get_or_insert_with(|| Instant::now() + STOP_BUDGET)
    }

    fn receipt_command(
        &mut self,
        opcode: u8,
        previous: &Receipt,
        deadline: Instant,
    ) -> BridgeResult<Receipt> {
        let next = Receipt::parse(
            &self.bounded_command(opcode, &previous.id.to_le_bytes(), deadline, FRAME_EXCHANGE)?,
            previous.requested,
        )?;
        next.follows(previous)?;
        Ok(next)
    }

    fn finish_native(&mut self, mut receipt: Receipt, deadline: Instant) -> BridgeResult<Receipt> {
        if receipt.phase != 3 {
            return Err(invalid("native frame is not terminal"));
        }
        receipt = self.receipt_command(FINISH, &receipt, deadline)?;
        loop {
            remaining(deadline)?;
            if receipt.phase == 5 {
                return Ok(receipt);
            }
            if receipt.phase != 4 {
                return Err(invalid("native finish was not acknowledged"));
            }
            std::thread::sleep(POLL_INTERVAL.min(remaining(deadline)?));
            receipt = self.receipt_command(POLL, &receipt, deadline)?;
        }
    }

    fn admit_native(
        &mut self,
        opcode: u8,
        body: &[u8],
        requested: u32,
        deadline: Instant,
    ) -> BridgeResult<Receipt> {
        let receipt = Receipt::parse(
            &self.bounded_command(opcode, body, deadline, FRAME_EXCHANGE)?,
            requested,
        )?;
        if receipt.id <= self.last_native_id
            || receipt.generation < self.last_native_generation
            || receipt.phase > 3
        {
            return Err(invalid("native operation admission reused an identity"));
        }
        self.last_native_id = receipt.id;
        self.last_native_generation = receipt.generation;
        Ok(receipt)
    }

    pub(super) fn verify_owned_pause(&mut self, deadline: Instant) -> BridgeResult<()> {
        let result = (|| {
            let budget_ms = remaining(deadline)?
                .as_millis()
                .min(MAX_SYNC_OPERATION_MS as u128) as u32;
            if budget_ms == 0 {
                return Err(invalid("native pause budget expired"));
            }
            let mut receipt = self.admit_native(PAUSE, &budget_ms.to_le_bytes(), 0, deadline)?;
            loop {
                remaining(deadline)?;
                if receipt.phase == 3 {
                    self.finish_native(receipt, deadline)?;
                    return Ok(());
                }
                std::thread::sleep(POLL_INTERVAL.min(remaining(deadline)?));
                receipt = self.receipt_command(POLL, &receipt, deadline)?;
            }
        })();
        if result.is_err() {
            self.control_unverified = true;
        }
        result
    }

    pub(super) fn owned_frame_step(&mut self, count: u64) -> BridgeResult<Value> {
        let token = self.request_cancellation.clone().unwrap_or_default();
        if token.is_cancelled() {
            self.remember_cancel_deadline();
            return Err(Pcsx2BridgeError::Cancelled);
        }
        let result = (|| {
            // Admission is checked on the CPU owner; basic status is not a halt proof.
            let execution_end = Instant::now() + crate::live::temporal::MAX_SYNC_OPERATION_TIME;
            let mut body = (count as u32).to_le_bytes().to_vec();
            body.extend_from_slice(&(MAX_SYNC_OPERATION_MS as u32).to_le_bytes());
            let mut receipt = self.admit_native(BEGIN, &body, count as u32, execution_end)?;
            let mut sent_cancel = false;
            loop {
                self.remember_cancel_deadline();
                if Instant::now() >= execution_end {
                    self.native_stop_deadline = Some(
                        self.native_stop_deadline
                            .map_or(execution_end + STOP_BUDGET, |d| {
                                d.min(execution_end + STOP_BUDGET)
                            }),
                    );
                }
                let limit = self
                    .native_stop_deadline
                    .unwrap_or(execution_end + STOP_BUDGET);
                remaining(limit)?;
                if receipt.phase == 3 {
                    let finish_end = limit.min(Instant::now() + STOP_BUDGET);
                    let terminal = self.finish_native(receipt, finish_end)?;
                    return Ok(
                        json!({"advanced":terminal.count,"count":terminal.count,"requested":count,
                        "unit":"frames","state":"frozen",
                        "status":if terminal.reason==0 {"completed"} else {"interrupted"},
                        "reason":match terminal.reason {0=>"requested_count",1=>"cancelled",2=>"host_deadline",_=>"external_pause_or_debugger_stop"}}),
                    );
                }
                if !sent_cancel && (token.is_cancelled() || Instant::now() >= execution_end) {
                    receipt = self.receipt_command(CANCEL, &receipt, limit)?;
                    sent_cancel = true;
                    continue;
                }
                std::thread::sleep(POLL_INTERVAL.min(remaining(limit)?));
                receipt = self.receipt_command(POLL, &receipt, limit)?;
            }
        })();
        if result.is_err() {
            self.control_unverified = true;
        }
        result
    }
}

#[cfg(test)]
#[path = "owned_frame_tests.rs"]
mod tests;
