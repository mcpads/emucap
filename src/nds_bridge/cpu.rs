use super::*;

impl<G: GdbTransport> CpuConn<G> {
    pub(super) fn new(id: CpuId, mut gdb: G) -> Self {
        // DeSmuME halts on start, so `?` returns a stop reply and the core begins frozen.
        let frozen = gdb.send("?").is_ok();
        Self {
            id,
            gdb,
            frozen,
            events: Vec::new(),
        }
    }

    pub(super) fn note_stop(&mut self, stop: String) {
        // S02(SIGINT)는 async 이벤트가 아니라 우리가 건 pause/interrupt다. 두 가지를 지운다:
        //   1) 이벤트 큐 — with_frozen이 데이터 명령마다 pause하면 이 SIGINT가 쌓여 poll_events에서 실제 BP
        //      히트(S05=SIGTRAP)를 가린다.
        //   2) frozen — interrupt()는 0x03의 async stop을 소비하지만 뒤이은 `?` 조회가 만든 *중복* SIGINT가
        //      소켓에 남는다. 그 잔류를 (이미 resume한 뒤) 나중 drain_stops가 읽어 frozen=true로 되돌리면
        //      running 코어를 phantom freeze시킨다(공유 write 후 비-라우팅 ARM7이 그렇게 굳었다). 그래서
        //      frozen은 reportable stop에서만 세우고, 우리 pause/interrupt의 frozen 부기는 pause()/resume()가
        //      명시적으로 소유한다.
        if is_interrupt_stop(&stop) {
            return;
        }
        self.frozen = true;
        let mut event = stop_event(&stop);
        set_event_field(&mut event, "cpu", json!(self.id.as_str()));
        self.events.push(event);
    }

    /// Send an RSP command and return its reply. For commands whose reply is not itself a stop
    /// packet, a stale async stop sitting ahead of the real reply is drained to the event queue
    /// and the true reply is read next, so a late breakpoint stop cannot desync the stream.
    pub(super) fn send_cmd(&mut self, payload: &str) -> NdsResult<String> {
        self.with_frozen(|s| {
            let mut resp = s.gdb.send(payload)?;
            if !command_expects_stop(payload) {
                while is_stop_packet(&resp) {
                    s.note_stop(resp);
                    resp = s.gdb.recv_reply()?;
                }
            }
            Ok(resp)
        })
    }

    /// Drain any buffered async stop packets (breakpoint hits) without blocking.
    pub(super) fn drain_stops(&mut self) -> NdsResult<()> {
        while let Some(pkt) = self.gdb.recv_nonblocking()? {
            if is_stop_packet(&pkt) {
                self.note_stop(pkt);
            } else {
                break;
            }
        }
        Ok(())
    }

    pub(super) fn read_regs_hex(&mut self) -> NdsResult<String> {
        let resp = self.send_cmd("g")?;
        if resp.starts_with('E') {
            return Err(NdsBridgeError::Emulator(format!(
                "GDB register read failed: {resp}"
            )));
        }
        Ok(resp)
    }

    pub(super) fn read_abs_hex(&mut self, address: u64, length: usize) -> NdsResult<String> {
        // 전체 다중청크 read를 한 번의 with_frozen으로 — 청크마다 pause/resume하면 게임이 청크 사이에 진행해
        // torn read(서로 다른 시점의 청크)가 된다. 내부 send_cmd는 이미 frozen이라 재-pause 안 함.
        self.with_frozen(|s| {
            let mut out = String::with_capacity(length.saturating_mul(2));
            let mut offset = 0usize;
            while offset < length {
                let chunk = std::cmp::min(MAX_READ_CHUNK, length - offset);
                let resp = s.send_cmd(&format!("m{:x},{:x}", address + offset as u64, chunk))?;
                if resp.starts_with('E') {
                    return Err(NdsBridgeError::Emulator(format!(
                        "GDB memory read failed: {resp}"
                    )));
                }
                out.push_str(&resp);
                offset += chunk;
            }
            Ok(out)
        })
    }

    /// Write `hexstr` (2 hex chars per byte) at `address` via GDB `M` packets, split into
    /// `MAX_WRITE_CHUNK`-byte packets so no packet exceeds the stub's fixed input buffer — a
    /// too-large `M` packet is silently dropped by DeSmuME (lost write + multi-second stall). Like
    /// `read_abs_hex`, the whole write runs under one freeze so a running core cannot advance between
    /// chunks and tear it; the inner `send_cmd` is already frozen and does not re-pause.
    pub(super) fn write_abs_hex(&mut self, address: u64, hexstr: &str) -> NdsResult<()> {
        self.with_frozen(|s| {
            let size = hexstr.len() / 2;
            let mut offset = 0usize;
            while offset < size {
                let chunk = std::cmp::min(MAX_WRITE_CHUNK, size - offset);
                let hex_slice = &hexstr[offset * 2..(offset + chunk) * 2];
                let resp = s.send_cmd(&format!(
                    "M{:x},{chunk:x}:{hex_slice}",
                    address + offset as u64
                ))?;
                // DeSmuME answers `M` with an empty packet, not "OK"; accept either. A non-empty
                // non-OK reply (e.g. "E02" on a bad address) is a real error.
                if !resp.is_empty() && resp != "OK" {
                    return Err(NdsBridgeError::Emulator(format!(
                        "GDB memory write failed: {resp}"
                    )));
                }
                offset += chunk;
            }
            Ok(())
        })
    }

    pub(super) fn step_instructions(&mut self, count: u64) -> NdsResult<()> {
        if count > crate::live::temporal::MAX_SYNC_ADVANCE_COUNT {
            return Err(NdsBridgeError::BadParams(format!(
                "instruction count {count} exceeds the synchronous cap {}; split the advance and verify each terminal response",
                crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
            )));
        }
        // Stepping halts the core, so the bridge must halt it first: otherwise send_cmd's with_frozen
        // treats each `s` as a bridge-injected pause and auto-resumes ("c") after it, re-running the
        // core while step then labels it frozen — a mismatch that desyncs the next command. Pausing
        // up front makes with_frozen a no-op per step and keeps the frozen bookkeeping consistent.
        self.pause()?;
        let deadline = Instant::now() + crate::live::temporal::MAX_SYNC_OPERATION_TIME;
        for completed in 0..count {
            if Instant::now() >= deadline {
                return Err(NdsBridgeError::Emulator(format!(
                    "instruction step deadline exceeded after {completed} of {count}; the core remains frozen"
                )));
            }
            // `s` replies with a stop, so it bypasses send_cmd's demux; clear any buffered
            // stale stop first so it is not mistaken for this step's completion.
            self.drain_stops()?;
            let resp = self.send_cmd("s")?;
            if resp.starts_with('E') {
                return Err(NdsBridgeError::Emulator(format!(
                    "GDB instruction step failed: {resp}"
                )));
            }
            if !is_stop_packet(&resp) {
                return Err(NdsBridgeError::Emulator(format!(
                    "GDB instruction step returned unexpected response: {resp}"
                )));
            }
        }
        self.frozen = true;
        Ok(())
    }

    /// Halt the core. Returns whether pausing drained a *reportable* async stop — a breakpoint or
    /// signal the bridge did NOT cause (queued as an event). When it did, the core is legitimately
    /// halted at that stop and callers must not auto-resume past it (resuming would drift the PC
    /// and lose the stopped state); when it did not, the bridge injected the pause itself and
    /// callers may undo it by resuming.
    pub(super) fn pause(&mut self) -> NdsResult<bool> {
        if !self.frozen {
            // 인터럽트 전에 대기 중인 스톱(BP 히트 S05 등)을 드레인해 큐에 넣는다 — 안 그러면 interrupt()의
            // 읽기가 그 S05를 삼켜 poll_events가 BP 히트를 잃는다. 드레인으로 이미 멈춘 게 드러나면(frozen)
            // 인터럽트를 생략한다(멈춘 스텁에 0x03은 무응답→hang 위험). 살아있으면 인터럽트하고 우리 SIGINT의
            // frozen 부기를 여기서 명시적으로 소유한다 — note_stop은 S02(우리 SIGINT)로 frozen을 세우지 않는다
            // (잔류 SIGINT가 나중 drain에서 running 코어를 phantom freeze시키는 걸 막으려고).
            let events_before = self.events.len();
            self.drain_stops()?;
            let drained_reportable = self.events.len() > events_before;
            if !self.frozen {
                let stop = self.gdb.interrupt()?;
                self.note_stop(stop);
                self.frozen = true;
            }
            return Ok(drained_reportable);
        }
        Ok(false)
    }

    pub(super) fn resume(&mut self) -> NdsResult<()> {
        if self.frozen {
            self.gdb.send_no_reply("c")?;
            self.frozen = false;
        }
        Ok(())
    }

    /// Send a command whose reply is a (long) base64 blob and read it, demuxing any stray async stop
    /// that slipped past `drain_stops`. The reply bypasses `send_cmd`'s demux because base64 can begin
    /// with 'S'/'T' (so `is_stop_packet` would eat it); but a genuine stray stop read as the reply —
    /// e.g. "S05", 3 chars — would base64-decode to a *padding error*. So `drain_stops` first, then
    /// skip only base64-impossible stop shapes (`looks_like_stray_stop`) before returning the reply.
    pub(super) fn send_b64_reply(&mut self, payload: &str) -> NdsResult<String> {
        self.with_frozen(|s| {
            s.drain_stops()?;
            let mut resp = s.gdb.send(payload)?;
            let mut guard = 0;
            while looks_like_stray_stop(&resp) && guard < 16 {
                s.note_stop(resp);
                resp = s.gdb.recv_reply()?;
                guard += 1;
            }
            Ok(resp)
        })
    }

    /// emucap custom RSP screenshot (`qEmucap,ss`) → base64 PNG of both DS screens.
    pub(super) fn screenshot_b64(&mut self) -> NdsResult<String> {
        let resp = self.send_b64_reply("qEmucap,ss")?;
        if resp.is_empty() {
            return Err(NdsBridgeError::Emulator(
                "screenshot: DeSmuME returned an empty reply (frame buffer unavailable)".into(),
            ));
        }
        Ok(resp)
    }

    /// DeSmuME의 GDB 스텁은 **프롬프트(frozen)일 때만** 명령을 clean하게 처리한다 — running(`c`) 중 패킷을
    /// 보내면 `-`(nack)로 거절하고, write_packet이 `-`에 프레임을 재전송해 nack/재전송 dance가 트레일링 응답을
    /// 파이프에 남긴다. 그러면 이후 명령이 그 잔류를 읽어 desync된다: screenshot이 스테일(직전 PNG 재서빙)·
    /// read_memory가 그 PNG를 읽음(누수)·touch가 트레일링 OK — 전부 같은 클래스. 그래서 stub에 응답을 기대하는
    /// 모든 명령(데이터 read·override)은 이걸 거쳐 running이면 잠깐 pause→frozen에서 전송→running 복원한다.
    pub(super) fn with_frozen<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> NdsResult<T>,
    ) -> NdsResult<T> {
        let was_running = !self.frozen;
        // Pausing a running core may drain a real breakpoint stop that was already pending. In that
        // case the core is legitimately halted at the breakpoint and must not be resumed past it
        // (that would drift the PC and misattribute the hit) — only undo the pause when the bridge
        // itself injected it, i.e. no reportable stop was drained.
        let resume_after = if was_running { !self.pause()? } else { false };
        let r = f(self);
        if resume_after {
            let _ = self.resume();
        }
        r
    }

    /// emucap custom RSP input (`QEmucap,input:<hexmask>[,<hexframes>]`). `frames=None` holds
    /// until the next input command; `Some(n)` auto-releases after n processed frames.
    pub(super) fn send_input(&mut self, mask: u16, frames: Option<u64>) -> NdsResult<()> {
        let payload = match frames {
            Some(frames) => format!("QEmucap,input:{mask:x},{frames:x}"),
            None => format!("QEmucap,input:{mask:x}"),
        };
        let resp = self.send_cmd(&payload)?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!(
                "input injection failed: {resp}"
            )));
        }
        Ok(())
    }

    /// emucap custom RSP touch (`QEmucap,touch:<hexX>,<hexY>[,<hexframes>]`, `QEmucap,touch:release`).
    /// `frames=None` holds until changed; `Some(n)` auto-lifts after n processed frames (a tap).
    pub(super) fn send_touch(&mut self, x: u16, y: u16, frames: Option<u64>) -> NdsResult<()> {
        let payload = match frames {
            Some(frames) => format!("QEmucap,touch:{x:x},{y:x},{frames:x}"),
            None => format!("QEmucap,touch:{x:x},{y:x}"),
        };
        let resp = self.send_cmd(&payload)?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!(
                "touch injection failed: {resp}"
            )));
        }
        Ok(())
    }

    pub(super) fn override_remaining(&mut self, status_command: &str) -> NdsResult<i64> {
        let resp = self.send_cmd(status_command)?;
        let remaining = resp.parse::<i64>().map_err(|_| {
            NdsBridgeError::Emulator(format!(
                "timed override status returned an invalid value: {resp:?}"
            ))
        })?;
        if remaining < -1 {
            return Err(NdsBridgeError::Emulator(format!(
                "timed override status returned an invalid remaining count: {remaining}"
            )));
        }
        Ok(remaining)
    }

    /// Poll the fork-owned emulator-frame countdown until release. Each query is sent through
    /// `with_frozen`, so the RSP stub sees a clean prompt; the brief host polling gaps only affect
    /// wall time, never the number of emulator frames for which the override is applied.
    pub(super) fn wait_timed_override(
        &mut self,
        status_command: &str,
        requested_frames: u64,
    ) -> NdsResult<TimedOverrideTerminal> {
        let started = Instant::now();
        loop {
            std::thread::sleep(TIMED_INPUT_POLL_INTERVAL);
            let remaining = self.override_remaining(status_command)?;
            if remaining < 0 {
                return Err(NdsBridgeError::Emulator(
                    "timed override unexpectedly became a persistent hold".into(),
                ));
            }
            let frames_elapsed = requested_frames.saturating_sub(remaining as u64);
            // with_frozen leaves a genuinely stopped core frozen instead of resuming past its BP.
            // Check this before remaining==0: release and a breakpoint can land on the same frame,
            // and reporting completed/running would otherwise hide the real frozen terminal state.
            // The caller owns cleanup and will release the transient override before responding.
            if self.frozen {
                return Ok(TimedOverrideTerminal::Interrupted { frames_elapsed });
            }
            if remaining == 0 {
                return Ok(TimedOverrideTerminal::Completed);
            }
            if started.elapsed() >= TIMED_INPUT_DEADLINE {
                return Err(NdsBridgeError::Emulator(format!(
                    "timed override did not complete within {} ms (requested {requested_frames} frames, {remaining} remaining)",
                    TIMED_INPUT_DEADLINE.as_millis()
                )));
            }
        }
    }

    pub(super) fn send_touch_release(&mut self) -> NdsResult<()> {
        let resp = self.send_cmd("QEmucap,touch:release")?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!(
                "touch release failed: {resp}"
            )));
        }
        Ok(())
    }

    /// emucap custom RSP savestate (`QEmucap,{save,load}state:<hexpath>`). The path is hex
    /// encoded so spaces/`/`/`.` ride the packet cleanly. DeSmuME's savestate is global (both
    /// cores), so this is issued on the ARM9 connection. Reply "OK" or "E01".
    pub(super) fn savestate(&mut self, path: &str, load: bool) -> NdsResult<()> {
        self.drain_stops()?;
        let verb = if load { "loadstate" } else { "savestate" };
        let payload = format!("QEmucap,{verb}:{}", hex::encode(path));
        let resp = self.send_cmd(&payload)?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!(
                "{verb} failed (DeSmuME reply {resp}); the emulator must be paused and the path writable/readable"
            )));
        }
        Ok(())
    }

    /// emucap custom RSP disassemble (`qEmucap,disasm:<hexaddr>,<hexcount>[,<mode>]`) → base64 of
    /// newline-separated `<addr>|<opcode>|<text>` rows. Sent raw (not via `send_cmd`) because a
    /// base64 reply can begin with `S`/`T` and be misread as a stop packet; any pending stop is
    /// drained first. `mode` is "arm"/"thumb" or "" for auto (the CPU's CPSR T-bit).
    pub(super) fn disasm_b64(&mut self, addr: u64, count: u64, mode: &str) -> NdsResult<String> {
        let payload = match mode {
            "arm" => format!("qEmucap,disasm:{addr:x},{count:x},a"),
            "thumb" => format!("qEmucap,disasm:{addr:x},{count:x},t"),
            _ => format!("qEmucap,disasm:{addr:x},{count:x}"),
        };
        let resp = self.send_b64_reply(&payload)?;
        if resp.is_empty() {
            return Err(NdsBridgeError::Emulator(
                "disassemble: DeSmuME returned an empty reply (bus unavailable)".into(),
            ));
        }
        Ok(resp)
    }

    /// Read a 32-bit little-endian pointer at `address` for the best-effort stack walk. A read
    /// or decode failure yields `None` so the walk ends cleanly instead of erroring the request.
    pub(super) fn read_ptr_le(&mut self, address: u64) -> Option<u64> {
        let hex = self.read_abs_hex(address, 4).ok()?;
        le_hex_to_u32(&hex).map(|v| v as u64)
    }
}
