//! Bounded native frame control. Both GDB endpoints share one scheduler owner.
use super::*;

const EXCHANGE: Duration = Duration::from_millis(400);
const POLL: Duration = Duration::from_millis(15);

fn remaining(deadline: Instant) -> NdsResult<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|v| !v.is_zero())
        .ok_or_else(|| NdsBridgeError::Emulator("native stop deadline expired".into()))
}
fn owned_stop(packet: &str) -> bool {
    let bytes = packet.as_bytes();
    bytes.len() >= 3
        && matches!(bytes[0], b'S' | b'T')
        && bytes[1..3].iter().all(u8::is_ascii_hexdigit)
}
fn parse_halt(reply: &str) -> NdsResult<(bool, u64)> {
    let fields: Vec<_> = reply.split('|').collect();
    if fields.len() != 4
        || fields[0] != "HALT"
        || fields[1] != "1"
        || !matches!(fields[2], "parked" | "running")
    {
        return Err(NdsBridgeError::Emulator(
            "native shared halt proof unavailable".into(),
        ));
    }
    let clock = u64::from_str_radix(fields[3], 16)
        .map_err(|_| NdsBridgeError::Emulator("invalid native halt clock".into()))?;
    Ok((fields[2] == "parked", clock))
}
impl<G: GdbTransport> NdsBridge<G> {
    pub(super) fn owned_exchange(&mut self, packet: &str) -> NdsResult<String> {
        let deadline = Instant::now() + EXCHANGE;
        let mut reply = self
            .arm9
            .gdb
            .send_with_timeout(packet, remaining(deadline)?)?;
        for _ in 0..128 {
            if !owned_stop(&reply) {
                return Ok(reply);
            }
            self.arm9.note_stop(reply);
            reply = self
                .arm9
                .gdb
                .recv_reply_with_timeout(remaining(deadline)?)?;
        }
        Err(NdsBridgeError::Emulator(
            "native stop stream did not settle".into(),
        ))
    }
    pub(super) fn native_halt(&mut self) -> NdsResult<(bool, u64)> {
        parse_halt(&self.owned_exchange("qEmucap,haltstate")?)
    }
    pub(super) fn verify_owned_halt(&mut self) -> NdsResult<()> {
        let result = (|| {
            let deadline = Instant::now() + Duration::from_millis(1500);
            if self.native_halt()?.0 {
                self.set_scheduler_frozen(true);
                return Ok(());
            }
            self.arm9.gdb.request_interrupt_with_timeout(EXCHANGE)?;
            // The native sender awaits the stop packet's ACK before accepting
            // another request. Consume either CPU's stop before querying halt.
            loop {
                remaining(deadline)?;
                if let Some((cpu, packet)) = self.poll_owned_stop()? {
                    self.cpu_mut(cpu)?.note_stop(packet);
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            loop {
                remaining(deadline)?;
                if self.native_halt()?.0 {
                    self.set_scheduler_frozen(true);
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })();
        if result.is_err() {
            self.control_unverified = true;
        }
        result
    }
    fn poll_owned_stop(&mut self) -> NdsResult<Option<(CpuId, String)>> {
        for cpu in [CpuId::Arm9, CpuId::Arm7] {
            let packet = self.cpu_mut(cpu)?.gdb.recv_nonblocking_with_timeout(POLL)?;
            if let Some(packet) = packet {
                if !owned_stop(&packet) {
                    return Err(NdsBridgeError::Emulator(format!(
                        "unexpected frame response: {packet}"
                    )));
                }
                return Ok(Some((cpu, packet)));
            }
        }
        Ok(None)
    }
    pub(super) fn owned_frame_step(&mut self, count: u64) -> NdsResult<Value> {
        let token = self
            .request_cancellation
            .clone()
            .expect("owned request token");
        if token.is_cancelled() {
            return Err(NdsBridgeError::Cancelled);
        }
        let result = (|| {
            self.verify_owned_halt()?;
            // Retain preexisting events before admitting the new native advance.
            let drain_deadline = Instant::now() + EXCHANGE;
            while let Some((cpu, packet)) = self.poll_owned_stop()? {
                self.cpu_mut(cpu)?.note_stop(packet);
                remaining(drain_deadline)?;
            }
            let (parked, start) = self.native_halt()?;
            if !parked {
                return Err(NdsBridgeError::Emulator(
                    "scheduler left admitted halt".into(),
                ));
            }
            if token.is_cancelled() {
                return Err(NdsBridgeError::Cancelled);
            }
            self.arm9
                .gdb
                .send_no_reply_with_timeout(&format!("QEmucap,framestep:{count:x}"), EXCHANGE)?;
            self.set_scheduler_frozen(false);
            let deadline = Instant::now() + crate::live::temporal::MAX_SYNC_OPERATION_TIME;
            let mut stop_deadline = None;
            let mut reason = "cancelled";
            loop {
                if let Some((cpu, packet)) = self.poll_owned_stop()? {
                    self.verify_owned_halt()?;
                    let status =
                        parse_frame_step_status(&self.owned_exchange("qEmucap,framestepstatus")?)?;
                    let (parked, clock) = self.native_halt()?;
                    if !parked
                        || status.start != start
                        || status.end != clock
                        || status.end.checked_sub(status.start) != Some(status.completed)
                    {
                        return Err(NdsBridgeError::Emulator(
                            "frame terminal does not match admitted boundary".into(),
                        ));
                    }
                    let interrupted_by_us = is_interrupt_stop(&packet);
                    let mut terminal = self.finish_frame_step(count, cpu, packet, status)?;
                    if terminal["status"] == "interrupted" {
                        terminal["reason"] = json!(if interrupted_by_us {
                            reason
                        } else {
                            "breakpoint"
                        });
                    }
                    return Ok(terminal);
                }
                if let Some(limit) = stop_deadline {
                    remaining(limit)?;
                } else if token.is_cancelled() || Instant::now() >= deadline {
                    if !token.is_cancelled() {
                        reason = "host_deadline";
                    }
                    // The sibling may already be stopping: send one interrupt without waiting
                    // for ARM9 specifically, then keep watching both terminal endpoints.
                    self.arm9.gdb.request_interrupt_with_timeout(EXCHANGE)?;
                    stop_deadline = Some(Instant::now() + Duration::from_millis(1500));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        if result
            .as_ref()
            .is_err_and(|e| !matches!(e, NdsBridgeError::Cancelled))
        {
            self.control_unverified = true;
        }
        result
    }
}

#[cfg(test)]
#[path = "owned_frame_tests.rs"]
mod tests;
