//! Nintendo DS (DeSmuME) GDB-RSP ↔ emucap wire-protocol bridge.
//!
//! DeSmuME's headless CLI exposes one standard GDB-RSP stub per ARM core (ARM9, ARM7),
//! each on its own TCP port. This bridge speaks emucap's line-JSON protocol on one side and
//! standard RSP (`c`/`s`/`m`/`M`/`g`/`Z0`/`z0`/`?`) to those stubs on the other, routing
//! memory/registers/stepping/breakpoints to the ARM9 or ARM7 connection per request.
//!
//! Transport (`GdbRspClient`, `GdbTransport`, `BridgeEnv`) is reused from `pc98_bridge`.
//! Tier-1 (memory/registers/step/breakpoints) rides standard RSP. Screenshot, input, save/load
//! state, and disassemble, which standard RSP cannot serve, ride repo-owned custom RSP commands
//! the DeSmuME fork adds: `qEmucap,ss` returns a base64 PNG of both screens,
//! `QEmucap,input:<hexmask>[,<frames>]` forces the pad, `QEmucap,{save,load}state:<hexpath>`
//! drive native DeSmuME savestates, and `qEmucap,disasm:<addr>,<count>[,<mode>]` returns
//! base64 disassembly rows. `call_stack` needs no fork hook — it walks the ARM APCS r11 frame
//! chain over standard `g`/`m`. Remaining emulator-side verbs (trace, run_frames, …) still need a
//! fork hook and are reported as `unsupported` rather than advertised.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};
use crate::pc98_bridge::{BridgeEnv, BridgeError as GdbError, GdbTransport};

const MAX_READ_CHUNK: usize = 0x4000;

const METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "read_memory",
    "write_memory",
    "get_state",
    "step_instructions",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "pause",
    "resume",
    "poll_events",
    "screenshot",
    "set_input",
    "press_buttons",
    "touch",
    "save_state",
    "load_state",
    "disassemble",
    "call_stack",
    "reset",
];

/// Methods present in the shared emucap surface but not reachable through the DeSmuME RSP
/// stub. They resolve to a clear `unsupported` error and are omitted from advertised methods.
const UNSUPPORTED_METHODS: &[&str] = &[
    "run_frames",
    "probe",
    "find_pattern",
    "dump_memory",
    "watch_register",
    "set_trace",
    "get_trace",
    "break_on_reset",
];

const NDS_INPUT_BUTTONS: &[&str] = &[
    "a", "b", "x", "y", "l", "r", "start", "select", "up", "down", "left", "right",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuId {
    Arm9,
    Arm7,
}

impl CpuId {
    fn as_str(self) -> &'static str {
        match self {
            CpuId::Arm9 => "arm9",
            CpuId::Arm7 => "arm7",
        }
    }

    fn from_name(name: &str) -> Option<CpuId> {
        match name {
            "arm9" => Some(CpuId::Arm9),
            "arm7" => Some(CpuId::Arm7),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct NdsRegion {
    name: &'static str,
    base: u64,
    size: u64,
    cpu: CpuId,
}

/// v1 minimal NDS memory map. `main` is the shared 4 MB Main RAM (read via the ARM9 bus by
/// default); `arm9`/`arm7` expose each core's full bus with absolute addressing (base 0).
const MEMORY_REGIONS: &[NdsRegion] = &[
    NdsRegion {
        name: "main",
        base: 0x0200_0000,
        size: 0x0040_0000,
        cpu: CpuId::Arm9,
    },
    NdsRegion {
        name: "arm9",
        base: 0,
        size: 0x1_0000_0000,
        cpu: CpuId::Arm9,
    },
    NdsRegion {
        name: "arm7",
        base: 0,
        size: 0x1_0000_0000,
        cpu: CpuId::Arm7,
    },
];

#[derive(Debug, Clone)]
struct NdsBreakpoint {
    cpu: CpuId,
    kind: String,
    addr: u64,
    ztype: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum NdsBridgeError {
    #[error("{0}")]
    BadParams(String),
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("unsupported on nds (planned): {0}")]
    Unsupported(String),
    #[error("{0}")]
    Emulator(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<GdbError> for NdsBridgeError {
    fn from(err: GdbError) -> Self {
        NdsBridgeError::Emulator(err.to_string())
    }
}

type NdsResult<T> = Result<T, NdsBridgeError>;

/// One CPU's GDB-RSP connection plus the async-stop bookkeeping for that core.
struct CpuConn<G> {
    id: CpuId,
    gdb: G,
    frozen: bool,
    events: Vec<Value>,
}

impl<G: GdbTransport> CpuConn<G> {
    fn new(id: CpuId, mut gdb: G) -> Self {
        // DeSmuME halts on start, so `?` returns a stop reply and the core begins frozen.
        let frozen = gdb.send("?").is_ok();
        Self {
            id,
            gdb,
            frozen,
            events: Vec::new(),
        }
    }

    fn note_stop(&mut self, stop: String) {
        self.frozen = true;
        // S02(SIGINT)는 async 이벤트가 아니라 우리가 건 pause/interrupt다 — with_frozen이 데이터 명령마다
        // pause하면 이 인터럽트 스톱이 큐에 쌓여 poll_events에서 실제 BP 히트(S05=SIGTRAP)를 가린다.
        // frozen 상태만 갱신하고 큐엔 넣지 않는다.
        if is_interrupt_stop(&stop) {
            return;
        }
        let mut event = stop_event(&stop);
        set_event_field(&mut event, "cpu", json!(self.id.as_str()));
        self.events.push(event);
    }

    /// Send an RSP command and return its reply. For commands whose reply is not itself a stop
    /// packet, a stale async stop sitting ahead of the real reply is drained to the event queue
    /// and the true reply is read next, so a late breakpoint stop cannot desync the stream.
    fn send_cmd(&mut self, payload: &str) -> NdsResult<String> {
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
    fn drain_stops(&mut self) -> NdsResult<()> {
        while let Some(pkt) = self.gdb.recv_nonblocking()? {
            if is_stop_packet(&pkt) {
                self.note_stop(pkt);
            } else {
                break;
            }
        }
        Ok(())
    }

    fn read_regs_hex(&mut self) -> NdsResult<String> {
        let resp = self.send_cmd("g")?;
        if resp.starts_with('E') {
            return Err(NdsBridgeError::Emulator(format!(
                "GDB register read failed: {resp}"
            )));
        }
        Ok(resp)
    }

    fn read_abs_hex(&mut self, address: u64, length: usize) -> NdsResult<String> {
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

    fn step_instructions(&mut self, count: u64) -> NdsResult<()> {
        for _ in 0..count {
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

    fn pause(&mut self) -> NdsResult<()> {
        if !self.frozen {
            let _ = self.gdb.interrupt()?;
            self.frozen = true;
        }
        Ok(())
    }

    fn resume(&mut self) -> NdsResult<()> {
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
    fn send_b64_reply(&mut self, payload: &str) -> NdsResult<String> {
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
    fn screenshot_b64(&mut self) -> NdsResult<String> {
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
    fn with_frozen<T>(&mut self, f: impl FnOnce(&mut Self) -> NdsResult<T>) -> NdsResult<T> {
        let was_running = !self.frozen;
        if was_running {
            self.pause()?;
        }
        let r = f(self);
        if was_running {
            let _ = self.resume();
        }
        r
    }

    /// 타이밍(frames) 입력은 프레임이 흘러야 auto-release되는데 그 카운터는 running 중에만 감소한다(fork). frozen
    /// 코어에 timed 입력을 심으면 프레임이 안 흘러 armed로 남아 다음 resume에 stale 발화한다 — 그래서 frozen이면
    /// 거부한다(resume 후 다시 호출하거나 frames를 빼 hold로 쓰라). hold(frames=None)는 frozen에서도 무해하다.
    fn reject_timed_input_while_frozen(&self, frames: Option<u64>) -> NdsResult<()> {
        if frames.is_some() && self.frozen {
            return Err(NdsBridgeError::BadParams(
                "timed input (frames) needs a running emulator — on a frozen core the frame counter \
                 never advances, so the press/tap never elapses and stays armed until a later resume \
                 (firing in an unrelated state). resume first, or omit frames to hold the input."
                    .into(),
            ));
        }
        Ok(())
    }

    /// emucap custom RSP input (`QEmucap,input:<hexmask>[,<hexframes>]`). `frames=None` holds
    /// until the next input command; `Some(n)` auto-releases after n processed frames.
    fn send_input(&mut self, mask: u16, frames: Option<u64>) -> NdsResult<()> {
        self.reject_timed_input_while_frozen(frames)?;
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
    fn send_touch(&mut self, x: u16, y: u16, frames: Option<u64>) -> NdsResult<()> {
        self.reject_timed_input_while_frozen(frames)?;
        let payload = match frames {
            Some(frames) => format!("QEmucap,touch:{x:x},{y:x},{frames:x}"),
            None => format!("QEmucap,touch:{x:x},{y:x}"),
        };
        let resp = self.send_cmd(&payload)?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!("touch injection failed: {resp}")));
        }
        Ok(())
    }

    fn send_touch_release(&mut self) -> NdsResult<()> {
        let resp = self.send_cmd("QEmucap,touch:release")?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!("touch release failed: {resp}")));
        }
        Ok(())
    }

    /// emucap custom RSP savestate (`QEmucap,{save,load}state:<hexpath>`). The path is hex
    /// encoded so spaces/`/`/`.` ride the packet cleanly. DeSmuME's savestate is global (both
    /// cores), so this is issued on the ARM9 connection. Reply "OK" or "E01".
    fn savestate(&mut self, path: &str, load: bool) -> NdsResult<()> {
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
    fn disasm_b64(&mut self, addr: u64, count: u64, mode: &str) -> NdsResult<String> {
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
    fn read_ptr_le(&mut self, address: u64) -> Option<u64> {
        let hex = self.read_abs_hex(address, 4).ok()?;
        le_hex_to_u32(&hex).map(|v| v as u64)
    }
}

pub struct NdsBridge<G> {
    arm9: CpuConn<G>,
    arm7: Option<CpuConn<G>>,
    env: BridgeEnv,
    bps: BTreeMap<u64, NdsBreakpoint>,
    next_bp: u64,
    events: Vec<Value>,
}

impl<G: GdbTransport> NdsBridge<G> {
    pub fn new(arm9: G, arm7: Option<G>, env: BridgeEnv) -> Self {
        Self {
            arm9: CpuConn::new(CpuId::Arm9, arm9),
            arm7: arm7.map(|g| CpuConn::new(CpuId::Arm7, g)),
            env,
            bps: BTreeMap::new(),
            next_bp: 1,
            events: Vec::new(),
        }
    }

    pub fn handle_request(&mut self, req: Request) -> Response {
        let id = req.id;
        let result = match req.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "get_rom_info" => self.get_rom_info(),
            "read_memory" => self.read_memory(&req.params),
            "write_memory" => self.write_memory(&req.params),
            "get_state" => self.get_state(&req.params),
            "step" => self.step(&req.params),
            "step_instructions" => self.step_instructions(&req.params),
            "set_breakpoint" => self.set_breakpoint(&req.params),
            "clear_breakpoint" => self.clear_breakpoint(&req.params),
            "list_breakpoints" => self.list_breakpoints(),
            "clear_all_breakpoints" => self.clear_all_breakpoints(),
            "pause" => self.pause(&req.params),
            "resume" => self.resume(&req.params),
            "poll_events" => self.poll_events(&req.params),
            "screenshot" => self.screenshot(),
            "set_input" => self.set_input(&req.params),
            "press_buttons" => self.press_buttons(&req.params),
            "touch" => self.touch(&req.params),
            "save_state" => self.save_state(&req.params),
            "load_state" => self.load_state(&req.params),
            "disassemble" => self.disassemble(&req.params),
            "call_stack" => self.call_stack(&req.params),
            "reset" => self.reset(&req.params),
            other if UNSUPPORTED_METHODS.contains(&other) => {
                Err(NdsBridgeError::Unsupported(other.into()))
            }
            other => Err(NdsBridgeError::UnknownMethod(other.into())),
        };
        match result {
            Ok(value) => Response {
                id,
                ok: true,
                result: Some(value),
                error: None,
            },
            Err(err) => Response {
                id,
                ok: false,
                result: None,
                error: Some(ProtocolError {
                    kind: error_kind(&err).into(),
                    message: err.to_string(),
                }),
            },
        }
    }

    fn cpu_mut(&mut self, id: CpuId) -> NdsResult<&mut CpuConn<G>> {
        match id {
            CpuId::Arm9 => Ok(&mut self.arm9),
            CpuId::Arm7 => self.arm7.as_mut().ok_or_else(|| {
                NdsBridgeError::Emulator(
                    "ARM7 GDB connection is not attached (launch with an ARM7 endpoint to use arm7 memory/cpu)".into(),
                )
            }),
        }
    }

    fn hello(&self) -> NdsResult<Value> {
        let mut result = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "nds",
            "adapter": "desmume-nds-rust-gdb",
            "backend": "desmume-gdbstub",
            "debugger": true,
            "methods": METHODS,
            "memory_types": self.memory_type_names(),
            "region_sizes": self.region_sizes_json(),
            "capability_notes": self.capability_notes(),
            "input_buttons": nds_input_buttons_json(),
            "cpus": self.connected_cpu_names(),
        });
        let obj = result.as_object_mut().expect("hello is an object");
        if let Some(name) = &self.env.name {
            obj.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.env.session_token {
            obj.insert("session_token".into(), json!(token));
        }
        if let Some(content) = &self.env.content {
            obj.insert("content".into(), json!(content.display().to_string()));
        }
        obj.insert(
            "build".into(),
            json!(self.env.build.as_deref().unwrap_or("unknown")),
        );
        Ok(result)
    }

    fn status(&mut self) -> NdsResult<Value> {
        self.arm9.drain_stops()?;
        if let Some(a7) = self.arm7.as_mut() {
            a7.drain_stops()?;
        }
        Ok(json!({
            "connected": true,
            "system": "nds",
            "adapter": "desmume-nds-rust-gdb",
            "backend": "desmume-gdbstub",
            "debugger": true,
            "state": if self.all_frozen() { "frozen" } else { "running" },
            "memory_types": self.memory_type_names(),
            "cpus": self.cpu_status(),
            "capability_notes": self.capability_notes(),
            "input_buttons": nds_input_buttons_json(),
        }))
    }

    fn get_rom_info(&self) -> NdsResult<Value> {
        let content = self.env.content.as_ref().ok_or_else(|| {
            NdsBridgeError::BadParams("EMUCAP_CONTENT is not set for get_rom_info".into())
        })?;
        if !content.is_file() {
            return Err(NdsBridgeError::BadParams(format!(
                "content image not found: {}",
                content.display()
            )));
        }
        Ok(json!({
            "system": "nds",
            "adapter": "desmume-nds-rust-gdb",
            "name": content.file_name().and_then(|s| s.to_str()).unwrap_or(""),
            "path": absolute_display(content),
            "sha1": sha1_file(content)?,
            "size": content.metadata()?.len(),
            "media_type": content.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase(),
        }))
    }

    fn read_memory(&mut self, params: &Value) -> NdsResult<Value> {
        let (cpu, addr) = route(params)?;
        let length = required_num(params, "length")? as usize;
        let hex = self.cpu_mut(cpu)?.read_abs_hex(addr, length)?;
        Ok(json!({ "hex": hex, "cpu": cpu.as_str() }))
    }

    fn write_memory(&mut self, params: &Value) -> NdsResult<Value> {
        let (cpu, addr) = route(params)?;
        let hexstr = required_str(params, "hex")?;
        if hexstr.len() % 2 != 0 {
            return Err(NdsBridgeError::BadParams("hex must have even length".into()));
        }
        hex::decode(hexstr).map_err(|_| NdsBridgeError::BadParams("hex decode failed".into()))?;
        let size = hexstr.len() / 2;
        let resp = self
            .cpu_mut(cpu)?
            .send_cmd(&format!("M{addr:x},{size:x}:{hexstr}"))?;
        // DeSmuME's stub performs the write but answers `M` with an empty packet, not
        // "OK" (standard GDB / the MAME stub reply). Accept empty or "OK" as success; a
        // non-empty non-OK reply (e.g. "E02" on a bad address) is a real error.
        if !resp.is_empty() && resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!(
                "GDB memory write failed: {resp}"
            )));
        }
        Ok(json!({ "written": size, "cpu": cpu.as_str() }))
    }

    fn get_state(&mut self, params: &Value) -> NdsResult<Value> {
        let cpu = cpu_from_params(params)?;
        let hex = self.cpu_mut(cpu)?.read_regs_hex()?;
        Ok(json!({ "cpu": cpu.as_str(), "state": state_from_arm_regs_hex(&hex) }))
    }

    fn step(&mut self, params: &Value) -> NdsResult<Value> {
        // NDS는 프레임 step을 못 한다 — GDB-RSP엔 프레임 개념이 없고, DeSmuME fork에 run-frames 훅이 아직 없다.
        // 두 MCP 도구가 이 메서드로 온다: step(프레임)은 `{frames:n}`(unit 없음), step_instructions(명령)는
        // `{frames:n, unit:"instructions"}`. unit=instructions면 그 값을 명령 수로 해석한다(step_count가 frames도
        // 읽는다). unit이 없는데 frames가 오면 진짜 프레임-step 요청이라 거부한다 — 명령으로 조용히 오해석하면
        // (60프레임→60명령) freeze-step/tap이 어긋난다.
        match params.get("unit").and_then(Value::as_str) {
            Some("instructions") => {}
            Some(other) => {
                return Err(NdsBridgeError::Unsupported(format!(
                    "step unit={other} (nds bridge steps by instructions only)"
                )));
            }
            None => {
                if params.get("frames").is_some() {
                    return Err(NdsBridgeError::Unsupported(
                        "nds bridge: 프레임 step 미지원 — GDB-RSP엔 프레임 개념이 없다. 명령 단위 진행은 \
                         step_instructions를 쓰라. 진짜 프레임 실행(DeSmuME fork run-frames 훅)은 미구현(TODO)"
                            .into(),
                    ));
                }
            }
        }
        let count = step_count(params)?;
        self.step_cpu(params, count)
    }

    fn step_instructions(&mut self, params: &Value) -> NdsResult<Value> {
        let count = step_count(params)?;
        self.step_cpu(params, count)
    }

    fn step_cpu(&mut self, params: &Value, count: u64) -> NdsResult<Value> {
        let cpu = cpu_from_params(params)?;
        let conn = self.cpu_mut(cpu)?;
        conn.step_instructions(count)?;
        let state = state_from_arm_regs_hex(&conn.read_regs_hex()?);
        let pc = state.get("cpu.pc").and_then(Value::as_u64);
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "count": count,
            "cpu": cpu.as_str(),
            "pc": pc,
            "state": state,
        }))
    }

    fn set_breakpoint(&mut self, params: &Value) -> NdsResult<Value> {
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("exec")
            .to_string();
        if kind != "exec" {
            return Err(NdsBridgeError::BadParams(format!(
                "nds bridge supports exec breakpoints (kind=exec); got kind={kind}"
            )));
        }
        // NDS GDB-RSP는 단일 주소 exec BP만이다(Z0/Z1 @ addr, 4바이트). 코어 BP 페이로드의 범위(end)·pc/value
        // 필터·비-pausing·auto_savestate·snapshot은 브리지가 아직 구현 안 했다. 이들을 조용히 무시하면(성공인데
        // start만 걸리거나 GDB가 무조건 halt) 호출자가 오해하므로, 지원 서브셋만 통과시키고 나머지는 큰소리로
        // 거부한다. 진짜 지원은 후속 TODO.
        if let (Some(s), Some(e)) = (optional_num(params, "start")?, optional_num(params, "end")?) {
            if e != s {
                return Err(NdsBridgeError::Unsupported(
                    "nds bridge: 범위 BP 미지원 — 단일 주소 exec만(start==end). 범위/워치포인트는 아직 미구현(TODO)"
                        .into(),
                ));
            }
        }
        for opt in ["pc_min", "pc_max", "value"] {
            if optional_num(params, opt)?.is_some() {
                return Err(NdsBridgeError::Unsupported(format!(
                    "nds bridge: {opt} 미지원 — 단일 주소 exec BP만(GDB Z0/Z1). pc/value 필터는 아직 미구현(TODO)"
                )));
            }
        }
        if params.get("pause_on_hit").and_then(Value::as_bool) == Some(false) {
            return Err(NdsBridgeError::Unsupported(
                "nds bridge: pause_on_hit=false 미지원 — GDB BP는 항상 코어를 halt한다(비-pausing 텔레메트리 미구현 TODO)"
                    .into(),
            ));
        }
        if params.get("auto_savestate").and_then(Value::as_bool) == Some(true) {
            return Err(NdsBridgeError::Unsupported(
                "nds bridge: auto_savestate 미지원(TODO)".into(),
            ));
        }
        if params
            .get("snapshot")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            return Err(NdsBridgeError::Unsupported(
                "nds bridge: snapshot 미지원 — 히트 후 read_memory로 직접 캡처하라(TODO)".into(),
            ));
        }
        let hardware = params
            .get("hardware")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let ztype = if hardware { "1" } else { "0" };
        let (cpu, addr) = route(params)?;
        let resp = self
            .cpu_mut(cpu)?
            .send_cmd(&format!("Z{ztype},{addr:x},4"))?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!(
                "GDB breakpoint set failed: {resp}"
            )));
        }
        let id = self.next_bp;
        self.next_bp += 1;
        self.bps.insert(
            id,
            NdsBreakpoint {
                cpu,
                kind,
                addr,
                ztype,
            },
        );
        Ok(json!({ "id": id, "cpu": cpu.as_str(), "address": addr, "hardware": hardware }))
    }

    fn clear_breakpoint(&mut self, params: &Value) -> NdsResult<Value> {
        let id = required_num(params, "id")?;
        let bp = self
            .bps
            .get(&id)
            .cloned()
            .ok_or_else(|| NdsBridgeError::BadParams(format!("unknown breakpoint id: {id}")))?;
        let resp = self
            .cpu_mut(bp.cpu)?
            .send_cmd(&format!("z{},{:x},4", bp.ztype, bp.addr))?;
        if resp != "OK" && resp != "E00" {
            return Err(NdsBridgeError::Emulator(format!(
                "GDB breakpoint clear failed: {resp}"
            )));
        }
        self.bps.remove(&id);
        Ok(json!({ "cleared": id }))
    }

    fn list_breakpoints(&self) -> NdsResult<Value> {
        let mut rows = Vec::new();
        for (id, bp) in &self.bps {
            rows.push(json!({
                "id": id,
                "cpu": bp.cpu.as_str(),
                "kind": bp.kind.clone(),
                "address": bp.addr,
                "hardware": bp.ztype == "1",
            }));
        }
        Ok(json!({ "breakpoints": rows }))
    }

    fn clear_all_breakpoints(&mut self) -> NdsResult<Value> {
        let mut cleared = Vec::new();
        for id in self.bps.keys().copied().collect::<Vec<_>>() {
            if self.clear_breakpoint(&json!({ "id": id })).is_ok() {
                cleared.push(id);
            }
        }
        Ok(json!({ "cleared": cleared }))
    }

    fn pause(&mut self, params: &Value) -> NdsResult<Value> {
        let targets = self.pause_targets(params)?;
        let mut states = serde_json::Map::new();
        for cpu in targets {
            self.cpu_mut(cpu)?.pause()?;
            states.insert(cpu.as_str().into(), json!("frozen"));
        }
        Ok(json!({ "state": "frozen", "cpus": Value::Object(states) }))
    }

    fn resume(&mut self, params: &Value) -> NdsResult<Value> {
        let targets = self.resume_targets(params)?;
        let mut states = serde_json::Map::new();
        for cpu in targets {
            self.cpu_mut(cpu)?.resume()?;
            states.insert(cpu.as_str().into(), json!("running"));
        }
        Ok(json!({ "state": "running", "cpus": Value::Object(states) }))
    }

    /// Capture both DS screens (256x384, top over bottom) as a PNG. The DeSmuME fork encodes
    /// the native RGB555 frame buffer and returns it base64-encoded over the ARM9 connection.
    fn screenshot(&mut self) -> NdsResult<Value> {
        let b64 = self.arm9.screenshot_b64()?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|err| {
                // send_b64_reply()가 stray stop을 이미 걸러낸 뒤에도 decode가 실패하면 다른 원인이다(잘림 등).
                // 응답 길이와 앞부분을 실어 재발 시 진단 가능하게 한다 — stop이면 "S.."/"T..", 잘렸으면 len%4≠0.
                let t = b64.trim();
                let head: String = t.chars().take(32).collect();
                NdsBridgeError::Emulator(format!(
                    "screenshot: base64 decode failed: {err} (reply_len={}, head={head:?})",
                    t.len()
                ))
            })?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(NdsBridgeError::Emulator(
                "screenshot: DeSmuME reply was not a PNG".into(),
            ));
        }
        Ok(json!({
            "png_base64": b64,
            "format": "png",
            "width": 256,
            "height": 384,
        }))
    }

    /// Force a held button set until the next input command (empty list releases). Input is
    /// injected on the ARM9 connection (the primary CPU) and applied every frame by the fork.
    fn set_input(&mut self, params: &Value) -> NdsResult<Value> {
        let (mask, buttons) = buttons_to_mask(params.get("buttons"))?;
        self.arm9.send_input(mask, None)?;
        Ok(json!({ "buttons": buttons, "cpu": "arm9" }))
    }

    /// Hold a button set for `frames` processed frames, then auto-release. The fork counts the
    /// frames down itself, so the hold survives the frontend's per-frame input reset while the
    /// emulator runs.
    fn press_buttons(&mut self, params: &Value) -> NdsResult<Value> {
        let (mask, buttons) = buttons_to_mask(params.get("buttons"))?;
        if mask == 0 {
            return Err(NdsBridgeError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        self.arm9.send_input(mask, Some(frames))?;
        Ok(json!({ "buttons": buttons, "frames": frames, "cpu": "arm9" }))
    }

    /// Touch the bottom screen at (x, y) (256x192). `release: true` lifts; `frames` presses for that
    /// many frames then auto-lifts (a tap); omitting both holds the press until the next touch command.
    fn touch(&mut self, params: &Value) -> NdsResult<Value> {
        if params.get("release").and_then(Value::as_bool).unwrap_or(false) {
            self.arm9.send_touch_release()?;
            return Ok(json!({ "released": true, "cpu": "arm9" }));
        }
        let x = optional_num(params, "x")?
            .ok_or_else(|| NdsBridgeError::BadParams("touch requires x (0-255)".into()))?;
        let y = optional_num(params, "y")?
            .ok_or_else(|| NdsBridgeError::BadParams("touch requires y (0-191)".into()))?;
        if x > 255 || y > 191 {
            return Err(NdsBridgeError::BadParams(format!(
                "touch out of range: x 0-255, y 0-191 (got x={x}, y={y})"
            )));
        }
        let frames = optional_num(params, "frames")?;
        self.arm9.send_touch(x as u16, y as u16, frames)?;
        Ok(json!({ "x": x, "y": y, "frames": frames, "cpu": "arm9" }))
    }

    /// Write a native DeSmuME savestate to `path`. Savestates are global (both cores + PPU/SPU),
    /// so the command rides the ARM9 connection. The emulator should be frozen when this runs.
    fn save_state(&mut self, params: &Value) -> NdsResult<Value> {
        let path = required_str(params, "path")?.to_string();
        self.arm9.savestate(&path, false)?;
        Ok(json!({ "path": path, "status": "completed" }))
    }

    /// Restore a native DeSmuME savestate from `path`.
    fn load_state(&mut self, params: &Value) -> NdsResult<Value> {
        let path = required_str(params, "path")?.to_string();
        self.arm9.savestate(&path, true)?;
        Ok(json!({ "path": path, "status": "completed" }))
    }

    /// Power-cycle the NDS via the DeSmuME fork hook (`QEmucap,reset` → NDS_Reset). Both cores
    /// return to the HLE direct-boot entry and stay halted; issued on the ARM9 connection
    /// (reset is global). Stub-side breakpoints survive the reset, so `bps` is left intact.
    fn reset(&mut self, _params: &Value) -> NdsResult<Value> {
        // reset의 계약은 state:"frozen" — 코어를 halt 상태로 남긴다. 하지만 send_cmd는 with_frozen을 거쳐
        // running 코어를 잠깐 pause했다가 reset 후 resume해버린다. 그러면 frozen=true는 거짓이 되고(실제 running)
        // 다음 명령이 with_frozen을 건너뛰어 running 스텁에 나가 desync된다. 그래서 send_cmd 전에 두 코어를
        // 명시적으로 pause해 frozen에서 보내고, reset 후에도 halt가 유지되게 한다.
        self.arm9.pause()?;
        if let Some(a7) = self.arm7.as_mut() {
            a7.pause()?;
        }
        self.arm9.drain_stops()?;
        let resp = self.arm9.send_cmd("QEmucap,reset")?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!("reset failed: {resp}")));
        }
        self.arm9.frozen = true;
        if let Some(a7) = self.arm7.as_mut() {
            a7.frozen = true;
        }
        Ok(json!({ "status": "completed", "state": "frozen" }))
    }

    /// Disassemble `count` instructions from `address`/`start` on the routed CPU (default ARM9).
    /// `mode` ("arm"/"thumb"/"auto", default auto from the CPU's CPSR T-bit) picks the decoder.
    /// Returns `[{addr, bytes, text}]` where `bytes` is the little-endian in-memory opcode hex.
    fn disassemble(&mut self, params: &Value) -> NdsResult<Value> {
        let cpu = cpu_from_params(params)?;
        let addr = absolute_address(params)?;
        let count = optional_num(params, "count")?.unwrap_or(8).clamp(1, 4096);
        let mode = match params.get("mode").and_then(Value::as_str) {
            None | Some("auto") => "auto",
            Some("arm") => "arm",
            Some("thumb") => "thumb",
            Some(other) => {
                return Err(NdsBridgeError::BadParams(format!(
                    "unsupported disassemble mode: {other}; valid: arm, thumb, auto"
                )))
            }
        };
        let b64 = self.cpu_mut(cpu)?.disasm_b64(addr, count, mode)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|err| {
                NdsBridgeError::Emulator(format!("disassemble: base64 decode failed: {err}"))
            })?;
        let text = String::from_utf8_lossy(&bytes);
        let instructions = parse_disasm_rows(&text, count as usize);
        if instructions.is_empty() {
            return Err(NdsBridgeError::Emulator(
                "disassemble: DeSmuME produced no instructions".into(),
            ));
        }
        Ok(json!({ "instructions": instructions, "cpu": cpu.as_str(), "mode": mode }))
    }

    /// Best-effort ARM call stack for the routed CPU (default ARM9). Frame 0 is the PC; frame 1
    /// is LR (the current function's return address, valid only before it is overwritten); deeper
    /// frames walk the APCS r11 frame-pointer chain (`[fp-4]`=saved lr, `[fp-12]`=saved fp) and
    /// end early once r11 stops looking like a frame pointer — which is exactly when the game does
    /// not keep one. Each frame PC is sanity-checked against plausible NDS code regions.
    fn call_stack(&mut self, params: &Value) -> NdsResult<Value> {
        let cpu = cpu_from_params(params)?;
        let state = state_from_arm_regs_hex(&self.cpu_mut(cpu)?.read_regs_hex()?);
        let reg = |k: &str| state.get(k).and_then(Value::as_u64).unwrap_or(0);
        let pc = reg("cpu.pc");
        let lr = reg("cpu.lr");
        let sp = reg("cpu.sp");
        let mut fp = reg("cpu.r11");

        let mut frames = vec![json!({
            "pc": pc, "kind": "pc", "in_code_region": nds_in_code_region(pc)
        })];
        if lr != 0 {
            frames.push(json!({
                "pc": lr, "kind": "lr", "in_code_region": nds_in_code_region(lr)
            }));
        }
        let conn = self.cpu_mut(cpu)?;
        let mut depth = 0;
        while depth < 64 {
            // The frame base must be RAM-resident and at/above the stack top (stack grows down).
            if fp == 0 || !nds_in_ram(fp) || (sp != 0 && fp < sp) {
                break;
            }
            let (Some(saved_lr), Some(saved_fp)) = (
                conn.read_ptr_le(fp.wrapping_sub(4)),
                conn.read_ptr_le(fp.wrapping_sub(12)),
            ) else {
                break;
            };
            // A saved return address outside code space means r11 was not a frame pointer here.
            if !nds_in_code_region(saved_lr) {
                break;
            }
            frames.push(json!({ "pc": saved_lr, "kind": "fp-walk", "in_code_region": true }));
            // Callers sit at higher stack addresses; a non-increasing/out-of-RAM link ends the chain.
            if saved_fp <= fp || !nds_in_ram(saved_fp) {
                break;
            }
            fp = saved_fp;
            depth += 1;
        }
        Ok(json!({
            "frames": frames,
            "cpu": cpu.as_str(),
            "method": "lr+fp-walk (best-effort)",
            "note": "frame 0 = pc; frame 1 = lr (valid only until the current function overwrites it); deeper frames walk the APCS r11 frame-pointer chain and end early when the game does not keep r11 as a frame pointer. PCs are sanity-checked against NDS code regions.",
        }))
    }

    /// Cores to resume. DeSmuME runs the two CPUs in lockstep behind independent stubs, so
    /// continuing BOTH lets the un-broken core drag the broken one past its breakpoint — a
    /// nondeterministic overshoot. Continuing ARM9 alone makes ARM9 breakpoints
    /// deterministic while ARM7 stays frozen and inspectable.
    ///
    /// The DEFAULT (no `cpu`) depends on the session: **HITL windowed sessions default to
    /// `both`** because NDS input is largely read by ARM7 (the touchscreen TSC is on ARM7's SPI
    /// bus; ARM7 also mirrors the keypad) — leaving ARM7 frozen kills the human's touch/input
    /// while the demo (ARM9 video) keeps running. Headless (agent) sessions default to
    /// **ARM9-primary** for deterministic breakpoints. Either way the agent can force a specific
    /// core with `cpu:"arm9"` / `"arm7"` / `"both"`.
    fn resume_targets(&self, params: &Value) -> NdsResult<Vec<CpuId>> {
        let both = || {
            let mut targets = vec![CpuId::Arm9];
            if self.arm7.is_some() {
                targets.push(CpuId::Arm7);
            }
            targets
        };
        match params.get("cpu").and_then(Value::as_str) {
            None => Ok(if hitl_display() { both() } else { vec![CpuId::Arm9] }),
            Some("arm9") => Ok(vec![CpuId::Arm9]),
            Some("arm7") => Ok(vec![CpuId::Arm7]),
            Some("both") | Some("all") => Ok(both()),
            Some(other) => Err(NdsBridgeError::BadParams(format!(
                "unsupported cpu: {other}; valid: arm9, arm7, both"
            ))),
        }
    }

    fn poll_events(&mut self, params: &Value) -> NdsResult<Value> {
        let mut fresh = Vec::new();
        self.arm9.drain_stops()?;
        fresh.append(&mut std::mem::take(&mut self.arm9.events));
        if let Some(a7) = self.arm7.as_mut() {
            a7.drain_stops()?;
            fresh.append(&mut std::mem::take(&mut a7.events));
        }
        for event in &mut fresh {
            self.enrich_event(event);
        }
        let mut all = std::mem::take(&mut self.events);
        all.append(&mut fresh);

        let filter_id = optional_num(params, "breakpoint_id")?;
        let mut out = Vec::new();
        for mut event in all {
            let matches_filter = match filter_id {
                Some(fid) => event.get("id").and_then(Value::as_u64) == Some(fid),
                None => true,
            };
            if matches_filter {
                if let Some(obj) = event.as_object_mut() {
                    obj.remove("_enriched");
                }
                out.push(event);
            } else {
                self.events.push(event);
            }
        }
        Ok(json!({ "events": out, "dropped": 0 }))
    }

    /// Attach the halted core's registers/PC to a stop event and, when the PC matches a known
    /// exec breakpoint on that core, reclassify it as a breakpoint hit.
    fn enrich_event(&mut self, event: &mut Value) {
        if event.get("_enriched").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let cpu_name = event
            .get("cpu")
            .and_then(Value::as_str)
            .unwrap_or("arm9")
            .to_string();
        let cpu_id = CpuId::from_name(&cpu_name).unwrap_or(CpuId::Arm9);
        if event.get("regs").is_none() {
            match self.cpu_mut(cpu_id).and_then(|conn| conn.read_regs_hex()) {
                Ok(hex) => {
                    let state = state_from_arm_regs_hex(&hex);
                    if event.get("pc").is_none() {
                        if let Some(pc) = state.get("cpu.pc").cloned() {
                            set_event_field(event, "pc", pc);
                        }
                    }
                    set_event_field(event, "regs", state);
                }
                Err(err) => set_event_field(event, "regs_error", json!(err.to_string())),
            }
        }
        let pc = event
            .get("pc")
            .and_then(Value::as_u64)
            .or_else(|| event.get("regs").and_then(|r| r.get("cpu.pc")).and_then(Value::as_u64));
        if let Some(pc) = pc {
            let matched = self
                .bps
                .iter()
                .find(|(_, bp)| bp.cpu == cpu_id && bp.kind == "exec" && bp.addr == pc)
                .map(|(id, _)| *id);
            if let Some(id) = matched {
                set_event_field(event, "type", json!("breakpoint_hit"));
                set_event_field(event, "kind", json!("exec"));
                set_event_field(event, "address", json!(pc));
                set_event_field(event, "id", json!(id));
                set_event_field(event, "breakpoint_id", json!(id));
            }
        }
        set_event_field(event, "_enriched", json!(true));
    }

    fn pause_targets(&self, params: &Value) -> NdsResult<Vec<CpuId>> {
        match params.get("cpu").and_then(Value::as_str) {
            Some("arm9") => Ok(vec![CpuId::Arm9]),
            Some("arm7") => Ok(vec![CpuId::Arm7]),
            Some(other) => Err(NdsBridgeError::BadParams(format!(
                "unsupported cpu: {other}; valid: arm9, arm7"
            ))),
            None => {
                let mut targets = vec![CpuId::Arm9];
                if self.arm7.is_some() {
                    targets.push(CpuId::Arm7);
                }
                Ok(targets)
            }
        }
    }

    fn all_frozen(&self) -> bool {
        self.arm9.frozen && self.arm7.as_ref().map(|c| c.frozen).unwrap_or(true)
    }

    fn cpu_status(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "arm9".into(),
            json!({ "connected": true, "state": if self.arm9.frozen { "frozen" } else { "running" } }),
        );
        match &self.arm7 {
            Some(c) => obj.insert(
                "arm7".into(),
                json!({ "connected": true, "state": if c.frozen { "frozen" } else { "running" } }),
            ),
            None => obj.insert("arm7".into(), json!({ "connected": false })),
        };
        Value::Object(obj)
    }

    fn connected_cpu_names(&self) -> Vec<&'static str> {
        let mut names = vec!["arm9"];
        if self.arm7.is_some() {
            names.push("arm7");
        }
        names
    }

    fn memory_type_names(&self) -> Vec<&'static str> {
        MEMORY_REGIONS
            .iter()
            .filter(|r| r.cpu != CpuId::Arm7 || self.arm7.is_some())
            .map(|r| r.name)
            .collect()
    }

    fn region_sizes_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        for region in MEMORY_REGIONS {
            if region.cpu == CpuId::Arm7 && self.arm7.is_none() {
                continue;
            }
            obj.insert(region.name.into(), json!(region.size));
        }
        Value::Object(obj)
    }

    fn capability_notes(&self) -> Value {
        json!({
            "backend": "desmume-gdbstub",
            "rust_bridge": true,
            "implemented_methods": METHODS,
            "screenshot": true,
            "input": true,
            "frame_step": false,
            "step_units": ["instructions"],
            "breakpoints": true,
            "watch_register": false,
            "trace": false,
            "state_restore": true,
            "disassemble": true,
            "call_stack": true,
            "dual_cpu": true,
            "cpus": self.connected_cpu_names(),
        })
    }
}

fn error_kind(err: &NdsBridgeError) -> &'static str {
    match err {
        NdsBridgeError::BadParams(_) => "bad_params",
        NdsBridgeError::UnknownMethod(_) => "unknown_method",
        NdsBridgeError::Unsupported(_) => "unsupported",
        NdsBridgeError::Emulator(_) => "emulator_error",
        NdsBridgeError::Io(_) | NdsBridgeError::Json(_) => "bridge_error",
    }
}

fn memory_region(name: &str) -> Option<&'static NdsRegion> {
    MEMORY_REGIONS.iter().find(|r| r.name == name)
}

/// Resolve a request's `(cpu, absolute_address)` from `memory_type` + `address`/`start`.
/// The routing CPU is the memory_type's default unless an explicit `cpu` param overrides it.
fn route(params: &Value) -> NdsResult<(CpuId, u64)> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("arm9");
    let region = memory_region(memory_type).ok_or_else(|| {
        NdsBridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
    })?;
    let cpu = match params.get("cpu").and_then(Value::as_str) {
        None => region.cpu,
        Some("arm9") => CpuId::Arm9,
        Some("arm7") => CpuId::Arm7,
        Some(other) => {
            return Err(NdsBridgeError::BadParams(format!(
                "unsupported cpu: {other}; valid: arm9, arm7"
            )))
        }
    };
    Ok((cpu, region.base.wrapping_add(region_offset(params)?)))
}

fn region_offset(params: &Value) -> NdsResult<u64> {
    if let Some(value) = optional_num(params, "address")? {
        return Ok(value);
    }
    if let Some(value) = optional_num(params, "start")? {
        return Ok(value);
    }
    Err(NdsBridgeError::BadParams(
        "missing required param: address".into(),
    ))
}

/// Absolute code address for disassemble/call-stack use (a raw PC-style address, no region base
/// added — unlike `read_memory` these consume absolute addresses such as `cpu.pc`).
fn absolute_address(params: &Value) -> NdsResult<u64> {
    if let Some(value) = optional_num(params, "address")? {
        return Ok(value);
    }
    if let Some(value) = optional_num(params, "start")? {
        return Ok(value);
    }
    Err(NdsBridgeError::BadParams(
        "missing required param: address".into(),
    ))
}

/// Parse the fork's disassembly block (`<addrhex>|<opcodehex>|<text>` per line) into
/// `[{addr, bytes, text}]`. `bytes` is re-emitted in little-endian in-memory order (the fork
/// prints the opcode as a big-endian value), matching the pc98 adapter's byte convention.
fn parse_disasm_rows(text: &str, count: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text.lines() {
        if out.len() >= count {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '|');
        let addr_raw = parts.next().unwrap_or("").trim();
        let op_raw = parts.next().unwrap_or("").trim();
        let insn = parts.next().unwrap_or("").trim();
        let Ok(addr) = u64::from_str_radix(addr_raw, 16) else {
            continue;
        };
        let mut item = serde_json::Map::new();
        item.insert("addr".into(), json!(addr));
        item.insert("text".into(), json!(insn));
        item.insert("bytes".into(), json!(opcode_hex_to_le_bytes(op_raw)));
        out.push(Value::Object(item));
    }
    out
}

/// Convert a big-endian opcode value hex string (as the fork prints, e.g. "e3a00001") to the
/// little-endian in-memory byte order ("0100a0e3"). Odd/invalid input is passed through as-is.
fn opcode_hex_to_le_bytes(op_hex: &str) -> String {
    match hex::decode(op_hex) {
        Ok(mut bytes) => {
            bytes.reverse();
            hex::encode(bytes)
        }
        Err(_) => op_hex.to_ascii_lowercase(),
    }
}

/// NDS RAM windows a stack/frame pointer can legitimately live in (main RAM + WRAM). Used to
/// gate the best-effort stack walk's pointer reads away from MMIO.
fn nds_in_ram(addr: u64) -> bool {
    (0x0200_0000..0x0240_0000).contains(&addr) || (0x0300_0000..0x0400_0000).contains(&addr)
}

/// Plausible NDS executable regions for a return-address sanity check. The Thumb low bit is
/// masked off. Intentionally lenient (main RAM, ITCM, WRAM, ARM9 BIOS) — a hard reject here
/// would prune legitimate frames, so callers treat this as advisory (`in_code_region`).
fn nds_in_code_region(addr: u64) -> bool {
    let a = addr & !1;
    (0x0200_0000..0x0240_0000).contains(&a)      // main RAM
        || (0x0100_0000..0x0200_0000).contains(&a) // ITCM (ARM9)
        || (0x0300_0000..0x0400_0000).contains(&a) // shared + ARM7 WRAM
        || (0xFFFF_0000..0xFFFF_8000).contains(&a) // ARM9 BIOS
}

fn cpu_from_params(params: &Value) -> NdsResult<CpuId> {
    match params.get("cpu").and_then(Value::as_str) {
        None | Some("arm9") => Ok(CpuId::Arm9),
        Some("arm7") => Ok(CpuId::Arm7),
        Some(other) => Err(NdsBridgeError::BadParams(format!(
            "unsupported cpu: {other}; valid: arm9, arm7"
        ))),
    }
}

fn step_count(params: &Value) -> NdsResult<u64> {
    let count = match optional_num(params, "count")? {
        Some(count) => count,
        None => match optional_num(params, "n")? {
            Some(n) => n,
            None => optional_num(params, "frames")?.unwrap_or(1),
        },
    };
    Ok(count.max(1))
}

fn required_num(params: &Value, key: &str) -> NdsResult<u64> {
    let value = params
        .get(key)
        .ok_or_else(|| NdsBridgeError::BadParams(format!("missing required param: {key}")))?;
    parse_num(value)
        .ok_or_else(|| NdsBridgeError::BadParams(format!("invalid numeric param: {key}")))
}

fn optional_num(params: &Value, key: &str) -> NdsResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| NdsBridgeError::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

fn parse_num(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => parse_num_str(s),
        _ => None,
    }
}

fn parse_num_str(s: &str) -> Option<u64> {
    let raw = s.trim();
    if let Some(hex) = raw.strip_prefix('$') {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<u64>().ok()
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> NdsResult<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| NdsBridgeError::BadParams(format!("missing required param: {key}")))
}

fn is_stop_packet(resp: &str) -> bool {
    resp.starts_with('S') || resp.starts_with('T')
}

/// `S02` / `T02…` = SIGINT — the pause/interrupt WE injected (via `with_frozen` or `pause`), not an
/// async game event. Distinguished from a breakpoint stop (`S05` = SIGTRAP) so `note_stop` can drop
/// our own pauses instead of flooding the poll_events queue with them.
fn is_interrupt_stop(resp: &str) -> bool {
    is_stop_packet(resp) && resp.get(1..3) == Some("02")
}

/// A stray async stop that a base64 reply (screenshot/disasm) reader could mistake for its reply.
/// base64 is `[A-Za-z0-9+/=]` only, and `is_stop_packet` over-matches because a base64 blob can begin
/// with 'S'/'T'. So match only stop shapes that a real base64 reply can never be: an S-stop is exactly
/// "S"+2 hex (a real base64 reply is far longer than 3 chars), and a T-stop carries ';'/':' which
/// base64 lacks. This catches e.g. a stray "S05" that would otherwise base64-decode to a padding error.
fn looks_like_stray_stop(resp: &str) -> bool {
    let b = resp.as_bytes();
    (b.len() == 3 && b[0] == b'S' && b[1].is_ascii_hexdigit() && b[2].is_ascii_hexdigit())
        || (b.first() == Some(&b'T') && (resp.contains(';') || resp.contains(':')))
}

/// Commands whose normal RSP reply is itself a stop packet — their stop is a real reply, not a
/// stale async stop, so it must not be demuxed.
fn command_expects_stop(payload: &str) -> bool {
    payload == "c"
        || payload == "s"
        || payload == "?"
        || payload.starts_with('C')
        || payload.starts_with('S')
        || payload.starts_with("vCont")
}

fn stop_event(stop: &str) -> Value {
    json!({ "type": "stop", "signal": stop.get(1..3).unwrap_or(""), "raw": stop })
}

fn set_event_field(event: &mut Value, key: &str, value: Value) {
    if let Some(obj) = event.as_object_mut() {
        obj.insert(key.into(), value);
    }
}

fn nds_input_buttons_json() -> Value {
    json!({
        "system": "nds",
        "buttons": NDS_INPUT_BUTTONS,
        "implemented": true,
        "notes": "Injected on the ARM9 connection via the DeSmuME fork's QEmucap,input command. set_input holds until changed; press_buttons holds for N frames while the emulator runs.",
    })
}

/// emucap common NDS button → bit in the 12-bit mask the DeSmuME fork consumes
/// (`QEmucap,input:<hexmask>`). Layout matches the fork's decode in NDSSystem.cpp.
fn nds_button_bit(name: &str) -> Option<u16> {
    let bit = match name {
        "a" => 0,
        "b" => 1,
        "select" => 2,
        "start" => 3,
        "right" => 4,
        "left" => 5,
        "up" => 6,
        "down" => 7,
        "r" => 8,
        "l" => 9,
        "x" => 10,
        "y" => 11,
        _ => return None,
    };
    Some(1 << bit)
}

/// Fold a small set of aliases onto the canonical shared button names.
fn nds_button_alias(name: &str) -> &str {
    match name {
        "sel" => "select",
        "lb" | "l1" => "l",
        "rb" | "r1" => "r",
        other => other,
    }
}

/// Parse a `buttons` param (list of names) into the fork's 12-bit mask plus the normalized
/// names. An unknown button is rejected rather than silently dropped.
fn buttons_to_mask(raw: Option<&Value>) -> NdsResult<(u16, Vec<String>)> {
    let Some(raw) = raw else {
        return Ok((0, Vec::new()));
    };
    let Some(items) = raw.as_array() else {
        return Err(NdsBridgeError::BadParams("buttons must be a list".into()));
    };
    let mut mask = 0u16;
    let mut names = Vec::new();
    for value in items {
        let key = value
            .as_str()
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_else(|| value.to_string().trim_matches('"').to_ascii_lowercase());
        let normalized = nds_button_alias(&key);
        match nds_button_bit(normalized) {
            Some(bit) => {
                mask |= bit;
                names.push(normalized.to_string());
            }
            None => {
                return Err(NdsBridgeError::BadParams(format!(
                    "unsupported nds button: {key}; valid: {}",
                    NDS_INPUT_BUTTONS.join(", ")
                )))
            }
        }
    }
    Ok((mask, names))
}

/// Decode DeSmuME's standard ARM `g` packet. Layout (168 bytes): words 0..15 = r0..r15
/// (r13=sp, r14=lr, r15=pc), then FPA f0-f7 (96B) + FPS (4B) ignored, then CPSR as the last
/// 4 bytes. Each 32-bit word is little-endian byte order. A compact 68-byte layout
/// (r0..r15 + CPSR, no FPA) is also accepted.
fn state_from_arm_regs_hex(resp: &str) -> Value {
    let mut state = serde_json::Map::new();
    for i in 0..16 {
        let start = i * 8;
        let end = start + 8;
        if end > resp.len() {
            break;
        }
        if let Some(value) = le_hex_to_u32(&resp[start..end]) {
            state.insert(format!("cpu.r{i}"), json!(value));
        }
    }
    let cpsr = if resp.len() >= 336 {
        le_hex_to_u32(&resp[328..336])
    } else if resp.len() >= 136 {
        le_hex_to_u32(&resp[128..136])
    } else {
        None
    };
    if let Some(cpsr) = cpsr {
        state.insert("cpu.cpsr".into(), json!(cpsr));
    }
    if let Some(pc) = state.get("cpu.r15").cloned() {
        state.insert("cpu.pc".into(), pc);
    }
    if let Some(sp) = state.get("cpu.r13").cloned() {
        state.insert("cpu.sp".into(), sp);
    }
    if let Some(lr) = state.get("cpu.r14").cloned() {
        state.insert("cpu.lr".into(), lr);
    }
    if state.is_empty() {
        state.insert("cpu.raw_register_bytes".into(), json!(resp.len() / 2));
    }
    Value::Object(state)
}

fn le_hex_to_u32(hex: &str) -> Option<u32> {
    let bytes = hex::decode(hex).ok()?;
    if bytes.len() != 4 {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn sha1_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = Sha1::new();
    let mut file = File::open(path)?;
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// True for a HITL windowed session — the launcher sets `EMUCAP_NDS_DISPLAY=1` on the bridge when
/// `display=true`, which flips the default resume to `both` so ARM7 (which reads NDS input) advances.
fn hitl_display() -> bool {
    std::env::var("EMUCAP_NDS_DISPLAY")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

fn absolute_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pc98_bridge::BridgeError;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakeGdb {
        replies: VecDeque<(String, String)>,
        calls: Vec<String>,
        nonblocking: VecDeque<String>,
    }

    impl FakeGdb {
        fn with(replies: &[(&str, &str)]) -> Self {
            Self {
                replies: replies
                    .iter()
                    .map(|(a, b)| ((*a).into(), (*b).into()))
                    .collect(),
                ..Default::default()
            }
        }
    }

    impl GdbTransport for FakeGdb {
        fn send(&mut self, payload: &str) -> Result<String, BridgeError> {
            self.calls.push(payload.into());
            let Some((expected, reply)) = self.replies.pop_front() else {
                return Err(BridgeError::Emulator(format!(
                    "unexpected fake GDB call: {payload}"
                )));
            };
            assert_eq!(payload, expected);
            Ok(reply)
        }

        fn send_no_reply(&mut self, payload: &str) -> Result<(), BridgeError> {
            self.calls.push(payload.into());
            Ok(())
        }

        fn interrupt(&mut self) -> Result<String, BridgeError> {
            Ok("S05".into())
        }

        fn recv_nonblocking(&mut self) -> Result<Option<String>, BridgeError> {
            Ok(self.nonblocking.pop_front())
        }
    }

    /// Build a 168-byte DeSmuME ARM `g` packet with the given r-registers and CPSR.
    fn arm_regs_hex(regs: &[(usize, u32)], cpsr: u32) -> String {
        let mut bytes = vec![0u8; 168];
        for i in 0..16 {
            let value = regs
                .iter()
                .find(|(idx, _)| *idx == i)
                .map(|(_, v)| *v)
                .unwrap_or(0);
            bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[164..168].copy_from_slice(&cpsr.to_le_bytes());
        hex::encode(bytes)
    }

    fn bridge_arm9_only(replies: &[(&str, &str)]) -> NdsBridge<FakeGdb> {
        NdsBridge::new(FakeGdb::with(replies), None, BridgeEnv::default())
    }

    #[test]
    fn looks_like_stray_stop_distinguishes_stops_from_base64() {
        // 실 stop 패킷: 스샷/디스어셈 base64 응답으로 오독되면 padding 에러를 내던 것들.
        assert!(looks_like_stray_stop("S05")); // "S"+2hex(정확히 3자) — base64로 디코드 시 길이 3 → padding 에러
        assert!(looks_like_stray_stop("S00"));
        assert!(looks_like_stray_stop("T05thread:1;0d:0000;")); // T-stop은 ';'/':'를 포함(base64엔 없는 문자)
        assert!(looks_like_stray_stop("T0b20:0102;"));
        // 실 base64 응답(길고 [A-Za-z0-9+/=]만): S/T로 시작해도 stop으로 오분류하면 안 된다.
        assert!(!looks_like_stray_stop("SGVsbG8=")); // "S..."로 시작하는 base64
        assert!(!looks_like_stray_stop("TWFuIGlzIGRpc3Rpbmd1aXNoZWQ=")); // "T..."지만 ';'/':' 없음
        assert!(!looks_like_stray_stop("iVBORw0KGgoAAAANSUhEUg==")); // 일반 PNG base64
        assert!(!looks_like_stray_stop("S0")); // 짧지만 "S"+2hex 형식 아님
    }

    #[test]
    fn hello_advertises_only_tier1_truths() {
        let mut bridge = NdsBridge::new(
            FakeGdb::with(&[("?", "S05")]),
            Some(FakeGdb::with(&[("?", "S05")])),
            BridgeEnv {
                name: Some("nds".into()),
                ..Default::default()
            },
        );
        let response = bridge.handle_request(Request::new(1, "hello", json!({})));
        let result = response.result.unwrap();
        assert_eq!(result["adapter"], "desmume-nds-rust-gdb");
        assert_eq!(result["system"], "nds");
        assert_eq!(result["memory_types"], json!(["main", "arm9", "arm7"]));

        let methods = result["methods"].as_array().unwrap();
        for wanted in [
            "read_memory",
            "get_state",
            "set_breakpoint",
            "poll_events",
            "step_instructions",
            "screenshot",
            "set_input",
            "press_buttons",
            "save_state",
            "load_state",
            "disassemble",
            "call_stack",
            "reset",
        ] {
            assert!(methods.iter().any(|m| m == wanted), "missing {wanted}");
        }
        for forbidden in ["run_frames", "probe", "set_trace", "watch_register"] {
            assert!(
                !methods.iter().any(|m| m == forbidden),
                "should not advertise {forbidden}"
            );
        }

        let caps = &result["capability_notes"];
        assert_eq!(caps["screenshot"], true);
        assert_eq!(caps["input"], true);
        assert_eq!(caps["frame_step"], false);
        assert_eq!(caps["breakpoints"], true);
        assert_eq!(caps["state_restore"], true);
        assert_eq!(caps["disassemble"], true);
        assert_eq!(caps["call_stack"], true);
        assert_eq!(caps["step_units"], json!(["instructions"]));
    }

    #[test]
    fn hello_omits_arm7_memory_type_when_arm7_absent() {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response = bridge.handle_request(Request::new(1, "hello", json!({})));
        let result = response.result.unwrap();
        assert_eq!(result["memory_types"], json!(["main", "arm9"]));
        assert_eq!(result["cpus"], json!(["arm9"]));
    }

    #[test]
    fn read_memory_maps_main_region_to_absolute_arm9_address() {
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("m2000000,4", "deadbeef")]);
        let response = bridge.handle_request(Request::new(
            2,
            "read_memory",
            json!({"memory_type": "main", "address": 0, "length": 4}),
        ));
        let result = response.result.unwrap();
        assert_eq!(result["hex"], "deadbeef");
        assert_eq!(result["cpu"], "arm9");
    }

    #[test]
    fn write_memory_sends_m_packet_on_routed_cpu() {
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("M2000000,2:aabb", "OK")]);
        let response = bridge.handle_request(Request::new(
            3,
            "write_memory",
            json!({"memory_type": "main", "address": "0x0", "hex": "aabb"}),
        ));
        assert_eq!(response.result.unwrap()["written"], 2);
    }

    #[test]
    fn get_state_decodes_arm_register_packet_little_endian() {
        let regs = arm_regs_hex(
            &[
                (0, 0x0000_0011),
                (13, 0x0380_0000),
                (14, 0x0200_1000),
                (15, 0x0200_0800),
            ],
            0x6000_00DF,
        );
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("g", &regs)]);
        let response = bridge.handle_request(Request::new(4, "get_state", json!({})));
        let result = response.result.unwrap();
        assert_eq!(result["cpu"], "arm9");
        let state = &result["state"];
        assert_eq!(state["cpu.r0"], 0x11);
        assert_eq!(state["cpu.r15"], 0x0200_0800);
        assert_eq!(state["cpu.pc"], 0x0200_0800);
        assert_eq!(state["cpu.sp"], 0x0380_0000);
        assert_eq!(state["cpu.lr"], 0x0200_1000);
        assert_eq!(state["cpu.cpsr"], 0x6000_00DF);
    }

    #[test]
    fn set_breakpoint_sends_z0_and_tracks_id() {
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("Z0,2000100,4", "OK")]);
        let set = bridge.handle_request(Request::new(
            5,
            "set_breakpoint",
            json!({"memory_type": "main", "address": 0x100}),
        ));
        let set = set.result.unwrap();
        assert_eq!(set["id"], 1);
        assert_eq!(set["address"], 0x0200_0100);
        assert_eq!(set["cpu"], "arm9");

        let list = bridge.handle_request(Request::new(6, "list_breakpoints", json!({})));
        assert_eq!(
            list.result.unwrap()["breakpoints"],
            json!([{
                "id": 1,
                "cpu": "arm9",
                "kind": "exec",
                "address": 0x0200_0100,
                "hardware": false,
            }])
        );
    }

    #[test]
    fn arm7_memory_type_routes_to_arm7_connection() {
        // ARM9 only handles the handshake; the read must land on the ARM7 stub.
        let arm9 = FakeGdb::with(&[("?", "S05")]);
        let arm7 = FakeGdb::with(&[("?", "S05"), ("m3800000,4", "cafef00d")]);
        let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
        let response = bridge.handle_request(Request::new(
            7,
            "read_memory",
            json!({"memory_type": "arm7", "address": 0x0380_0000, "length": 4}),
        ));
        let result = response.result.unwrap();
        assert_eq!(result["hex"], "cafef00d");
        assert_eq!(result["cpu"], "arm7");
        // ARM9 stub saw only the handshake.
        assert_eq!(bridge.arm9.gdb.calls, vec!["?".to_string()]);
    }

    #[test]
    fn arm7_memory_type_errors_when_arm7_not_attached() {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response = bridge.handle_request(Request::new(
            8,
            "read_memory",
            json!({"memory_type": "arm7", "address": 0, "length": 4}),
        ));
        assert!(!response.ok);
        assert!(response.error.unwrap().message.contains("ARM7"));
    }

    #[test]
    fn step_instructions_single_steps_then_reports_pc() {
        let regs = arm_regs_hex(&[(15, 0x0200_0004)], 0);
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("s", "S05"), ("g", &regs)]);
        let response =
            bridge.handle_request(Request::new(9, "step_instructions", json!({"count": 1})));
        let result = response.result.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["unit"], "instructions");
        assert_eq!(result["count"], 1);
        assert_eq!(result["pc"], 0x0200_0004);
        assert_eq!(
            bridge.arm9.gdb.calls.iter().filter(|c| c.as_str() == "s").count(),
            1
        );
    }

    #[test]
    fn step_method_treats_frames_with_instructions_unit_as_instruction_count() {
        // The MCP's step_instructions tool sends {frames:N, unit:"instructions"} to the "step"
        // method. That must run as an instruction step, NOT be rejected as a frame step.
        let regs = arm_regs_hex(&[(15, 0x0200_0008)], 0);
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("s", "S05"), ("g", &regs)]);
        let response = bridge.handle_request(Request::new(
            9,
            "step",
            json!({"frames": 1, "unit": "instructions"}),
        ));
        let result = response.result.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["count"], 1);
        assert_eq!(
            bridge.arm9.gdb.calls.iter().filter(|c| c.as_str() == "s").count(),
            1
        );
    }

    #[test]
    fn step_method_rejects_bare_frames_as_unsupported_frame_step() {
        // A bare {frames:N} (the frame-step tool, no unit) has no NDS meaning → reject, do not
        // silently run N instructions.
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response = bridge.handle_request(Request::new(9, "step", json!({"frames": 60})));
        assert!(!response.ok);
        assert!(response.error.unwrap().message.contains("프레임 step 미지원"));
    }

    #[test]
    fn is_interrupt_stop_matches_sigint_only() {
        assert!(is_interrupt_stop("S02")); // SIGINT = our pause
        assert!(is_interrupt_stop("T02thread:1;")); // T-form SIGINT
        assert!(!is_interrupt_stop("S05")); // SIGTRAP = breakpoint, reportable
        assert!(!is_interrupt_stop("T05thread:1;"));
        assert!(!is_interrupt_stop("OK"));
    }

    #[test]
    fn note_stop_drops_sigint_keeps_sigtrap() {
        // with_frozen pauses on every data command; those SIGINT (S02) stops must not flood the
        // poll_events queue and bury a real breakpoint hit (S05).
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        bridge.arm9.note_stop("S02".into());
        assert!(
            bridge.arm9.events.is_empty(),
            "SIGINT (S02) must not enter the event queue"
        );
        bridge.arm9.note_stop("S05".into());
        assert_eq!(
            bridge.arm9.events.len(),
            1,
            "SIGTRAP (S05) breakpoint stop must be reported"
        );
    }

    #[test]
    fn reset_while_running_halts_core_without_resuming() {
        // reset's contract is state:"frozen" — it must leave the core actually halted. If ARM9 is
        // running, reset must NOT resume it (send_cmd's with_frozen would) while still claiming
        // frozen; that mismatch sends the next command to a running stub and desyncs. Assert no `c`
        // (resume) is emitted and the reported state matches reality.
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,reset", "OK")]);
        bridge.arm9.frozen = false; // simulate a running core (e.g. HITL resume-both)
        let response = bridge.handle_request(Request::new(9, "reset", json!({})));
        let result = response.result.unwrap();
        assert_eq!(result["state"], "frozen");
        assert!(bridge.arm9.frozen, "ARM9 must actually be halted after reset");
        assert!(
            !bridge.arm9.gdb.calls.iter().any(|c| c == "c"),
            "reset must not resume the core; calls = {:?}",
            bridge.arm9.gdb.calls
        );
    }

    #[test]
    fn reset_from_frozen_completes_without_resuming() {
        // The normal path (already frozen) must still work and never emit a resume.
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,reset", "OK")]);
        let response = bridge.handle_request(Request::new(9, "reset", json!({})));
        assert_eq!(response.result.unwrap()["state"], "frozen");
        assert!(bridge.arm9.frozen);
        assert!(!bridge.arm9.gdb.calls.iter().any(|c| c == "c"));
    }

    #[test]
    fn unsupported_method_returns_unsupported_error_kind() {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        for method in ["run_frames", "probe", "set_trace", "watch_register"] {
            let response = bridge.handle_request(Request::new(10, method, json!({})));
            assert!(!response.ok, "{method} should fail");
            let error = response.error.unwrap();
            assert_eq!(error.kind, "unsupported", "{method} kind");
            assert!(error.message.contains("unsupported on nds"), "{method} msg");
        }
    }

    #[test]
    fn screenshot_sends_query_and_returns_png_base64() {
        let png = b"\x89PNG\r\n\x1a\nDESMUME-TEST-BYTES";
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("qEmucap,ss", b64.as_str())]);
        let response = bridge.handle_request(Request::new(1, "screenshot", json!({})));
        assert!(response.ok, "screenshot failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["png_base64"], b64);
        assert_eq!(result["format"], "png");
        assert_eq!(result["width"], 256);
        assert_eq!(result["height"], 384);
        assert!(bridge.arm9.gdb.calls.iter().any(|c| c == "qEmucap,ss"));
    }

    #[test]
    fn screenshot_rejects_non_png_reply() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"not a png");
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("qEmucap,ss", b64.as_str())]);
        let response = bridge.handle_request(Request::new(1, "screenshot", json!({})));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "emulator_error");
    }

    #[test]
    fn set_input_sends_mask_for_a_and_b() {
        // a=bit0, b=bit1 -> 0b11 = 0x3
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,input:3", "OK")]);
        let response =
            bridge.handle_request(Request::new(1, "set_input", json!({"buttons": ["a", "b"]})));
        assert!(response.ok, "set_input failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["buttons"], json!(["a", "b"]));
        assert_eq!(result["cpu"], "arm9");
    }

    #[test]
    fn set_input_maps_shoulder_and_dpad_bits() {
        // left=bit5 (0x20), r shoulder=bit8 (0x100), start=bit3 (0x8) -> 0x128
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,input:128", "OK")]);
        let response = bridge.handle_request(Request::new(
            1,
            "set_input",
            json!({"buttons": ["left", "r", "start"]}),
        ));
        assert!(response.ok, "set_input failed: {:?}", response.error);
    }

    #[test]
    fn set_input_empty_releases_with_zero_mask() {
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,input:0", "OK")]);
        let response = bridge.handle_request(Request::new(1, "set_input", json!({"buttons": []})));
        assert!(response.ok, "release failed: {:?}", response.error);
    }

    #[test]
    fn press_buttons_encodes_mask_and_frames() {
        // a=bit0 -> mask 1, frames 3 -> "QEmucap,input:1,3"
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,input:1,3", "OK")]);
        bridge.arm9.frozen = false; // timed input needs a running core (with_frozen pauses to inject)
        let response = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["a"], "frames": 3}),
        ));
        assert!(response.ok, "press failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["frames"], 3);
        assert_eq!(result["buttons"], json!(["a"]));
    }

    #[test]
    fn press_buttons_requires_a_button() {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response = bridge.handle_request(Request::new(1, "press_buttons", json!({"buttons": []})));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn unknown_button_is_rejected() {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response =
            bridge.handle_request(Request::new(1, "set_input", json!({"buttons": ["turbo"]})));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn touch_sends_hex_coords() {
        // x=128 (0x80), y=96 (0x60), no frames -> hold "QEmucap,touch:80,60"
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,touch:80,60", "OK")]);
        let response = bridge.handle_request(Request::new(1, "touch", json!({"x": 128, "y": 96})));
        assert!(response.ok, "touch failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["x"], 128);
        assert_eq!(result["y"], 96);
    }

    #[test]
    fn touch_with_frames_is_a_tap() {
        // x=10 (0xa), y=20 (0x14), frames=5 -> "QEmucap,touch:a,14,5"
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,touch:a,14,5", "OK")]);
        bridge.arm9.frozen = false; // a timed tap needs a running core
        let response =
            bridge.handle_request(Request::new(1, "touch", json!({"x": 10, "y": 20, "frames": 5})));
        assert!(response.ok, "touch failed: {:?}", response.error);
        assert_eq!(response.result.unwrap()["frames"], 5);
    }

    #[test]
    fn timed_input_while_frozen_is_rejected() {
        // press_buttons / touch(frames) on a frozen core would install a timed override whose frame
        // counter never advances (the game is not running), leaving it armed to fire on a later
        // resume in an unrelated state. Reject with bad_params; a held input (no frames) is fine.
        let mut bridge = bridge_arm9_only(&[("?", "S05")]); // arm9 frozen after handshake
        let press = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["a"], "frames": 3}),
        ));
        assert!(!press.ok);
        assert_eq!(press.error.unwrap().kind, "bad_params");
        let tap = bridge.handle_request(Request::new(
            2,
            "touch",
            json!({"x": 10, "y": 20, "frames": 5}),
        ));
        assert!(!tap.ok);
        assert_eq!(tap.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn touch_release_lifts() {
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,touch:release", "OK")]);
        let response = bridge.handle_request(Request::new(1, "touch", json!({"release": true})));
        assert!(response.ok, "touch release failed: {:?}", response.error);
        assert_eq!(response.result.unwrap()["released"], true);
    }

    #[test]
    fn touch_out_of_range_is_rejected() {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response = bridge.handle_request(Request::new(1, "touch", json!({"x": 300, "y": 96})));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn touch_requires_coords() {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response = bridge.handle_request(Request::new(1, "touch", json!({"y": 96})));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn unknown_method_uses_unknown_method_kind() {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response = bridge.handle_request(Request::new(11, "not_a_method", json!({})));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "unknown_method");
    }

    #[test]
    fn write_memory_accepts_desmume_empty_reply() {
        // DeSmuME performs the write but answers `M` with an empty packet, not "OK".
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("M2100000,4:deadbeef", "")]);
        let response = bridge.handle_request(Request::new(
            1,
            "write_memory",
            json!({"memory_type": "main", "address": 0x100000, "hex": "deadbeef"}),
        ));
        assert!(response.ok, "empty M reply is success: {:?}", response.error);
        assert_eq!(response.result.unwrap()["written"], json!(4));
    }

    #[test]
    fn write_memory_rejects_error_reply() {
        // A real error code (bad address) is still an error, not silently accepted.
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("M2100000,4:deadbeef", "E02")]);
        let response = bridge.handle_request(Request::new(
            1,
            "write_memory",
            json!({"memory_type": "main", "address": 0x100000, "hex": "deadbeef"}),
        ));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "emulator_error");
    }

    #[test]
    fn resume_defaults_to_arm9_only() {
        // ARM9-primary: continuing both cores is racy in DeSmuME's lockstep (the un-broken core
        // drags the broken one past its breakpoint), so a bare resume continues only ARM9.
        let mut bridge = NdsBridge::new(
            FakeGdb::with(&[("?", "S05")]),
            Some(FakeGdb::with(&[("?", "S05")])),
            BridgeEnv::default(),
        );
        let response = bridge.handle_request(Request::new(1, "resume", json!({})));
        assert!(response.ok);
        let cpus = response.result.unwrap()["cpus"].clone();
        assert_eq!(cpus.get("arm9").and_then(|v| v.as_str()), Some("running"));
        assert!(cpus.get("arm7").is_none(), "arm7 must not resume by default");
    }

    #[test]
    fn resume_both_opts_into_dual_continue() {
        let mut bridge = NdsBridge::new(
            FakeGdb::with(&[("?", "S05")]),
            Some(FakeGdb::with(&[("?", "S05")])),
            BridgeEnv::default(),
        );
        let response = bridge.handle_request(Request::new(1, "resume", json!({"cpu": "both"})));
        assert!(response.ok);
        let cpus = response.result.unwrap()["cpus"].clone();
        assert_eq!(cpus.get("arm9").and_then(|v| v.as_str()), Some("running"));
        assert_eq!(cpus.get("arm7").and_then(|v| v.as_str()), Some("running"));
    }

    #[test]
    fn save_state_sends_hex_encoded_savestate_command() {
        let path = "/tmp/s.dsv";
        let cmd = format!("QEmucap,savestate:{}", hex::encode(path));
        let mut bridge = bridge_arm9_only(&[("?", "S05"), (cmd.as_str(), "OK")]);
        let response =
            bridge.handle_request(Request::new(1, "save_state", json!({ "path": path })));
        assert!(response.ok, "save_state failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["path"], path);
        assert_eq!(result["status"], "completed");
        assert!(bridge.arm9.gdb.calls.iter().any(|c| c == &cmd));
    }

    #[test]
    fn reset_sends_reset_command_and_reports_frozen() {
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,reset", "OK")]);
        let response = bridge.handle_request(Request::new(1, "reset", json!({})));
        assert!(response.ok, "reset failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["state"], "frozen");
        assert!(bridge.arm9.gdb.calls.iter().any(|c| c == "QEmucap,reset"));
    }

    #[test]
    fn load_state_sends_hex_encoded_loadstate_command() {
        let path = "/tmp/s.dsv";
        let cmd = format!("QEmucap,loadstate:{}", hex::encode(path));
        let mut bridge = bridge_arm9_only(&[("?", "S05"), (cmd.as_str(), "OK")]);
        let response =
            bridge.handle_request(Request::new(1, "load_state", json!({ "path": path })));
        assert!(response.ok, "load_state failed: {:?}", response.error);
        assert_eq!(response.result.unwrap()["status"], "completed");
    }

    #[test]
    fn save_state_surfaces_emulator_error_on_e01() {
        let path = "/bad/s.dsv";
        let cmd = format!("QEmucap,savestate:{}", hex::encode(path));
        let mut bridge = bridge_arm9_only(&[("?", "S05"), (cmd.as_str(), "E01")]);
        let response =
            bridge.handle_request(Request::new(1, "save_state", json!({ "path": path })));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "emulator_error");
    }

    #[test]
    fn disassemble_sends_query_and_parses_little_endian_bytes() {
        // Fork emits "<addr>|<opcode-value-hex>|<text>" per line, base64-encoded.
        let block = "2000000|e3a00001|mov r0, #1\n2000004|e12fff1e|bx lr\n";
        let b64 = base64::engine::general_purpose::STANDARD.encode(block);
        let mut bridge =
            bridge_arm9_only(&[("?", "S05"), ("qEmucap,disasm:2000000,2", b64.as_str())]);
        let response = bridge.handle_request(Request::new(
            1,
            "disassemble",
            json!({ "address": 0x0200_0000u64, "count": 2 }),
        ));
        assert!(response.ok, "disassemble failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["cpu"], "arm9");
        let insns = result["instructions"].as_array().unwrap();
        assert_eq!(insns.len(), 2);
        assert_eq!(insns[0]["addr"], 0x0200_0000u64);
        assert_eq!(insns[0]["text"], "mov r0, #1");
        // e3a00001 in memory is little-endian: 01 00 a0 e3.
        assert_eq!(insns[0]["bytes"], "0100a0e3");
        assert_eq!(insns[1]["addr"], 0x0200_0004u64);
        assert_eq!(insns[1]["text"], "bx lr");
        assert_eq!(insns[1]["bytes"], "1eff2fe1");
    }

    #[test]
    fn disassemble_passes_thumb_mode_to_fork() {
        let block = "2000000|2001|movs r0, #1\n";
        let b64 = base64::engine::general_purpose::STANDARD.encode(block);
        let mut bridge =
            bridge_arm9_only(&[("?", "S05"), ("qEmucap,disasm:2000000,1,t", b64.as_str())]);
        let response = bridge.handle_request(Request::new(
            1,
            "disassemble",
            json!({ "address": 0x0200_0000u64, "count": 1, "mode": "thumb" }),
        ));
        assert!(response.ok, "disassemble thumb failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["mode"], "thumb");
        // 2001 in memory little-endian: 01 20.
        assert_eq!(result["instructions"][0]["bytes"], "0120");
    }

    #[test]
    fn call_stack_walks_lr_then_fp_chain_over_g_and_m() {
        // pc/lr in main RAM (code region); sp/fp in WRAM. One valid frame-pointer frame,
        // then a saved lr outside code space terminates the walk.
        let regs = arm_regs_hex(
            &[
                (11, 0x0300_0100), // fp
                (13, 0x0300_0000), // sp (stack top)
                (14, 0x0200_0200), // lr
                (15, 0x0200_0100), // pc
            ],
            0,
        );
        let mut bridge = bridge_arm9_only(&[
            ("?", "S05"),
            ("g", &regs),
            // iter 1: [fp-4]=saved lr=0x02000300, [fp-12]=saved fp=0x03000200
            ("m30000fc,4", "00030002"),
            ("m30000f4,4", "00020003"),
            // iter 2: [fp-4]=saved lr=0 (out of code region -> stop), [fp-12]=0
            ("m30001fc,4", "00000000"),
            ("m30001f4,4", "00000000"),
        ]);
        let response = bridge.handle_request(Request::new(1, "call_stack", json!({})));
        assert!(response.ok, "call_stack failed: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["cpu"], "arm9");
        assert_eq!(result["method"], "lr+fp-walk (best-effort)");
        let frames = result["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 3, "pc + lr + one fp-walk frame");
        assert_eq!(frames[0]["pc"], 0x0200_0100u64);
        assert_eq!(frames[0]["kind"], "pc");
        assert_eq!(frames[1]["pc"], 0x0200_0200u64);
        assert_eq!(frames[1]["kind"], "lr");
        assert_eq!(frames[2]["pc"], 0x0200_0300u64);
        assert_eq!(frames[2]["kind"], "fp-walk");
    }

    #[test]
    fn call_stack_without_frame_pointer_returns_pc_and_lr_only() {
        // r11 = 0 (no frame pointer) -> walk contributes nothing; only pc + lr frames.
        let regs = arm_regs_hex(&[(14, 0x0200_0200), (15, 0x0200_0100)], 0);
        let mut bridge = bridge_arm9_only(&[("?", "S05"), ("g", &regs)]);
        let response = bridge.handle_request(Request::new(1, "call_stack", json!({})));
        assert!(response.ok, "call_stack failed: {:?}", response.error);
        let frames = response.result.unwrap()["frames"].as_array().unwrap().clone();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["kind"], "pc");
        assert_eq!(frames[1]["kind"], "lr");
    }

    #[test]
    fn opcode_hex_to_le_bytes_reverses_byte_order() {
        assert_eq!(opcode_hex_to_le_bytes("e3a00001"), "0100a0e3");
        assert_eq!(opcode_hex_to_le_bytes("2001"), "0120");
    }
}
