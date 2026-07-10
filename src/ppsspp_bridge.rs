//! PSP (PPSSPP) WebSocket ↔ emucap wire-protocol bridge.
//!
//! PPSSPP's built-in remote debugger speaks JSON over a WebSocket at
//! `ws://127.0.0.1:<port>/debugger` with subprotocol `debugger.ppsspp.org`: a request is
//! `{"event": "<name>", ...params}`, the matching response reuses the same event name, and PPSSPP
//! also emits spontaneous events (log lines, breakpoint/stepping hits) unprompted.
//!
//! `WsTransport` abstracts that call/drain surface so `PpssppBridge` can run against either the real
//! `TungsteniteWs` connection or the `FakeWs` test double. Wired up today: `status` (via PPSSPP's
//! `version` + `game.status` + `cpu.status`), `hello` (identity handshake), `read_memory`/`write_memory`
//! (via `memory.read`/`memory.write`, base64 on the wire), `get_state` (via `cpu.getAllRegs`, GPR
//! category flattened to `cpu.<name>`), `disassemble` (via `memory.disasm`), breakpoints
//! (`cpu.breakpoint.*`/`memory.breakpoint.*`), stepping (`cpu.stepInto`), pause/resume
//! (`cpu.stepping`/`cpu.resume`), `poll_events` (draining PPSSPP's spontaneous `cpu.stepping`
//! events), `screenshot` (the emucap fork's `emucap.screenshot`, a GE-stepping-driving variant of
//! stock `gpu.buffer.screenshot` that also works while the game is running), `set_input`/
//! `press_buttons` (`input.buttons.send`/`input.buttons.press`, both stock PPSSPP WS commands —
//! no fork hook needed), `save_state`/`load_state` (the emucap fork's `savestate.save`/
//! `savestate.load`, stock PPSSPP exposes no WS savestate command), `reset` (stock `game.reset`),
//! and `get_rom_info` (stock `game.status` for id/title + a locally computed sha1 of the
//! `EMUCAP_CONTENT` image, since PPSSPP's WS API never exposes a content path or hash). `step`
//! (frame-based stepping) has no PPSSPP WS/fork primitive and is not advertised.
//!
//! Two PPSSPP protocol quirks shape the stepping/pause/resume/poll_events code below:
//! - `cpu.stepInto`/`cpu.stepOver`/`cpu.stepOut`/`cpu.runUntil`/`cpu.nextHLE` have **no synchronous
//!   reply** — PPSSPP acks them with a *differently named* spontaneous `cpu.stepping` event once the
//!   step completes (`SteppingSubscriber.cpp`). `cpu.resume` and the plain `cpu.stepping` (pause)
//!   request DO ack under their own name, so the existing `call()` demux (match-by-name) already
//!   handles those two; `call_and_wait_for` covers the mismatched-name case for stepping verbs.
//! - The `cpu.stepping` event's optional `reason`/`relatedAddress` fields (which would otherwise say
//!   *why* the CPU stopped, e.g. `"cpu.breakpoint"`) are in practice never populated — `Core_Break()`
//!   sets `g_cpuStepCommand.type = CPUStepType::None` in the same breath it stores the reason, which
//!   makes `Core_GetSteppingReason()`'s `!g_cpuStepCommand.empty()` guard immediately false. So a
//!   breakpoint hit and a plain stepping-request completion produce the *same* bare `{pc, ticks}`
//!   event; `poll_events` classifies a hit by matching the event's `pc` against tracked exec
//!   breakpoints (mirroring the NDS bridge) and, for memory breakpoints (which trip on a data access
//!   at some *other* pc), by a `memory.breakpoint.list` hit-count delta.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tungstenite::http::Uri;
use tungstenite::{ClientRequestBuilder, Message, WebSocket};

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

/// PPSSPP subprotocol the debugger WebSocket upgrade must advertise (`Core/Debugger/WebSocket.cpp`).
const PPSSPP_SUBPROTOCOL: &str = "debugger.ppsspp.org";

/// v1 PSP memory map. `main` is PSP user RAM at `Core/MemMap.h`'s
/// `PSP_GetUserMemoryBase()`. `read_memory`/`write_memory` add this base to the
/// request's `address`/`start` offset; PPSSPP's own `memory.read`/`memory.write` take the resulting
/// absolute address.
const MEMORY_TYPES: &[&str] = &["main"];

/// Absolute PSP address of the start of user RAM (`Core/MemMap.h: PSP_GetUserMemoryBase()`).
const PSP_MAIN_RAM_BASE: u64 = 0x0880_0000;

/// Routable extent of `main` (user RAM) above `PSP_MAIN_RAM_BASE`. PPSSPP maps user RAM
/// `PSP_GetUserMemoryBase()` (0x0880_0000) .. `PSP_GetUserMemoryEnd()` = `0x0800_0000 + g_MemorySize`;
/// the default `g_MemorySize` is `RAM_NORMAL_SIZE` = 0x0200_0000 (PSP-1000, `System.cpp`), so user RAM
/// ends at 0x0A00_0000 — 24 MiB (`Core/MemMap.{h,cpp}`). Extra-RAM models (PSP-2000 slim / remaster
/// games) map more, but 24 MiB is the conservative floor that never aliases past user RAM into another
/// region — the value this default headless build maps. `read_memory`/`write_memory` reject any access
/// whose `[offset, offset+length)` leaves `[0, PSP_MAIN_RAM_SIZE]`, so an out-of-bounds `main` write
/// cannot corrupt non-`main` memory while the bridge reports success.
const PSP_MAIN_RAM_SIZE: u64 = 0x0180_0000;

/// Cap on a single `read_memory` length (matches the NDS/Mesen adapters' read cap). PPSSPP's
/// `memory.read` itself streams the reply in 65535-byte base64 fragments and would happily serve an
/// enormous range in one response; the cap here bounds how much a single emucap request can tie up
/// the bridge and the wire, not a PPSSPP-side limit. A larger region is read in multiple requests
/// (advance the start address).
const MAX_READ_LEN: usize = 0x2_0000;

/// Cap on `press_buttons`' `frames` (hold duration). PPSSPP only acks `input.buttons.press` once
/// the button auto-releases after `duration` frames elapse (`WebSocketInputState::Broadcast`), so
/// `call()` blocks for that long — at PSP's ~60 fps a large `frames` value (e.g. a 10s hold, 600
/// frames) runs past the bridge binary's 8s WS read timeout (`emucap-ppsspp-bridge.rs`), which then
/// misattributes PPSSPP's late reply to whatever unrelated request comes next (this transport
/// demuxes by event name only, no per-request id — see the module doc). 240 frames (~4s at 60 fps)
/// leaves a comfortable margin under that 8s timeout for the round trip and any frame-timing
/// jitter; hold a button longer by issuing repeated `press_buttons` calls or `set_input` instead.
const MAX_PRESS_FRAMES: u64 = 240;

/// Dedicated WS read budget for `save_state`/`load_state`, threaded per-call over the bridge's
/// default read timeout (8s, `emucap-ppsspp-bridge.rs`). The emucap fork's `SaveStateSubscriber.cpp`
/// runs the async save/load on the EmuThread and blocks up to `cv.wait_for(..., seconds(15))` before
/// it replies. The 8s default is shorter than that worst case, so a slow (>8s) save/load would time
/// out on the bridge's socket read while PPSSPP is still working: the `call()` reports a spurious
/// failure, PPSSPP's later reply arrives unread, and — since this transport demuxes by event name
/// only — a stale `{event:"error"}` (PPSSPP's own 15s timeout) gets misattributed to whatever
/// unrelated request is next. Giving just these two calls a budget past the fork's 15s wait lets the
/// savestate call absorb its own reply/error while every other read keeps failing fast. Kept bounded
/// (not unbounded) so a genuinely wedged save still surfaces an error rather than hanging forever.
const SAVESTATE_READ_TIMEOUT: Duration = Duration::from_secs(20);

/// Dedicated WS read budget for `reset`. The emucap fork's headless build performs a *real* reboot
/// (`PSP_Shutdown` + re-init) on its run loop and blocks the `game.reset` ack until that reboot
/// completes (`GameSubscriber.cpp`, capped fork-side at 30s). The default 8s read is shorter than a
/// full commercial-title reboot, so — like `save_state` — this call gets its own budget past the
/// fork's wait. Kept bounded so a wedged reboot still surfaces an error rather than hanging forever.
const RESET_READ_TIMEOUT: Duration = Duration::from_secs(35);

/// After `game.reset` acks, how many times to poll `cpu.status.stepping` — and the gap between polls
/// — to confirm the reboot actually left the CPU halted at the fresh boot entry before reporting
/// completion. The headless fork only acks once the reboot finished and halted the core
/// (`CORE_STEPPING_CPU`), so the first poll already reads stepping (no wait). A display:true GUI
/// session does not block `game.reset` and keeps the core running through an async reboot, so these
/// polls read "still running" and `reset` reports the async reboot instead of a false "completed".
const RESET_HALT_POLLS: u32 = 3;
const RESET_HALT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Methods this bridge actually dispatches — kept truthful to `handle_request` so callers can trust
/// `status.methods`/`hello.methods`. Add a name only with its working handler.
const METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "read_memory",
    "write_memory",
    "dump_memory",
    "get_state",
    "disassemble",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "step_instructions",
    "pause",
    "resume",
    "poll_events",
    "screenshot",
    "set_input",
    "press_buttons",
    "save_state",
    "load_state",
    "reset",
];

/// PSP surface concretely planned for later tasks — none right now. Frame-based `step` is *not*
/// here: PPSSPP has no frame-advance primitive, so it is a permanent platform gap (an
/// `unsupported`), not a pending feature. It must not be advertised as "planned", which would imply
/// it is merely not-yet-callable; the stepping capability is at instruction granularity, exposed by
/// `step_instructions` being in `METHODS` and `capability_notes.step_units == ["instructions"]`.
/// Surfaced under `capability_notes.planned_methods` (alongside `UNSUPPORTED_METHODS`, below) so a
/// caller can see the target shape while `methods` reflects what works right now.
const PLANNED_METHODS: &[&str] = &[];

/// Real emucap tool names this bridge does not (yet) implement — mirrors the NDS bridge's
/// `UNSUPPORTED_METHODS` list verbatim (no PPSSPP WS/fork primitive backs any of these today).
/// Dispatching one of these returns a clear `unsupported` error instead of `unknown_method`,
/// reserving `unknown_method` for genuine typos/garbage method names. Also folded into
/// `capability_notes.planned_methods` so a caller can discover the gap without a trial call.
const UNSUPPORTED_METHODS: &[&str] = &[
    "run_frames",
    "probe",
    "find_pattern",
    "watch_register",
    "set_trace",
    "get_trace",
    "break_on_reset",
];

/// Native PSP display resolution — `emucap.screenshot`'s output framebuffer capture is always
/// this size in practice (it downloads the GE's *display* output, not an upscaled render target).
const PSP_SCREEN_WIDTH: u64 = 480;
const PSP_SCREEN_HEIGHT: u64 = 272;

/// emucap common PSP button name → PPSSPP's own button name (`Core/Debugger/WebSocket/InputSubscriber.cpp`
/// `buttonLookup`). Face buttons use PlayStation naming on the PPSSPP side (cross/circle/square/
/// triangle); shoulder buttons are ltrigger/rtrigger (no L2/R2 in the common surface). D-pad and
/// start/select share the emucap name verbatim. This is the full set PPSSPP maps input for today
/// (home/screen/note/hold/wlan/... are real PPSSPP button names but not part of the emucap common
/// surface, so they are not accepted here).
const PSP_INPUT_BUTTONS: &[&str] = &[
    "a", "b", "x", "y", "l", "r", "start", "select", "up", "down", "left", "right",
];

fn psp_button_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "a" => "cross",
        "b" => "circle",
        "x" => "square",
        "y" => "triangle",
        "l" => "ltrigger",
        "r" => "rtrigger",
        "start" => "start",
        "select" => "select",
        "up" => "up",
        "down" => "down",
        "left" => "left",
        "right" => "right",
        _ => return None,
    })
}

fn psp_input_buttons_json() -> Value {
    json!({
        "system": "psp",
        "buttons": PSP_INPUT_BUTTONS,
        "implemented": true,
        "notes": "Button names map to PSP: a→cross(✕), b→circle(○), x→square(□), y→triangle(△), l→ltrigger, r→rtrigger, plus start, select, and the d-pad (up/down/left/right). Confirm/cancel is game-defined — Japanese titles typically confirm with circle (b) and cancel with cross (a). Stock PPSSPP WebSocket commands (input.buttons.send/press), no fork hook needed. set_input holds until changed (a full replace — an empty list releases every button); press_buttons is a terminal-ack timed pulse for exactly one button and rejects multi-button lists because stock PPSSPP cannot apply them in one frame window.",
    })
}

/// Parse and validate a `buttons` param (list of emucap button names) against `PSP_INPUT_BUTTONS`.
/// An unknown button is rejected rather than silently dropped; an absent/empty list is valid
/// (means "no buttons" — `set_input` uses that to release everything).
fn button_list(raw: Option<&Value>) -> BridgeResult<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let Some(items) = raw.as_array() else {
        return Err(BridgeError::BadParams("buttons must be a list".into()));
    };
    let mut names = Vec::new();
    for value in items {
        let key = value
            .as_str()
            .map(|s| s.trim().to_ascii_lowercase())
            .ok_or_else(|| BridgeError::BadParams("buttons must be a list of strings".into()))?;
        if psp_button_name(&key).is_none() {
            return Err(BridgeError::BadParams(format!(
                "unsupported psp button: {key}; valid: {}",
                PSP_INPUT_BUTTONS.join(", ")
            )));
        }
        names.push(key);
    }
    Ok(names)
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("{0}")]
    BadParams(String),
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("unsupported on psp (planned): {0}")]
    Unsupported(String),
    #[error("{0}")]
    Emulator(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Ws(#[from] tungstenite::Error),
}

type BridgeResult<T> = Result<T, BridgeError>;

/// The PPSSPP debugger WebSocket call/drain surface `PpssppBridge` runs against — implemented for
/// real by `TungsteniteWs` and for tests by `FakeWs`.
pub trait WsTransport {
    /// Send `{"event": event, ...params}` and block for the reply carrying the same event name.
    /// A `{"event":"error", ...}` reply becomes `Err`. Any other event observed while waiting (a
    /// spontaneous log/breakpoint notification) is queued rather than dropped, so `drain_events`
    /// can surface it later.
    fn call(&mut self, event: &str, params: Value) -> Result<Value, BridgeError>;
    /// Send `{"event": event, ...params}` for a command whose acknowledgement is a *differently
    /// named* spontaneous event — PPSSPP's `cpu.stepInto`/`stepOver`/`stepOut`/`runUntil`/`nextHLE`
    /// have no reply of their own name; they ack via a `cpu.stepping` event once the step completes
    /// (`SteppingSubscriber.cpp`). Blocks until an event named `expect_event` arrives; anything else
    /// observed meanwhile is queued exactly like `call`.
    fn call_and_wait_for(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
    ) -> Result<Value, BridgeError>;
    /// Like `call`, but with a per-call read budget overriding the transport's default read timeout
    /// for just this exchange — for a command whose reply is legitimately slow (`save_state`/
    /// `load_state`, whose fork handler waits up to 15s). Restores the default afterward so ordinary
    /// reads still fail fast.
    fn call_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError>;
    /// Like `call`, but tags the request with a `ticket` and only accepts a reply whose `event`
    /// name AND echoed `ticket` both match. PPSSPP echoes any `ticket` a request carried back on its
    /// reply (`WebSocketUtils`/`InputSubscriber.cpp`), so this is how a timed `input.buttons.press`
    /// is correlated: a stale ack from an *earlier* press that only released once the CPU resumed
    /// (its own ack was stranded when a breakpoint halted the core mid-press) carries the earlier
    /// ticket and is queued/ignored here rather than misattributed to this call. Off-ticket replies
    /// seen while waiting are queued as spontaneous events, exactly like `call`.
    fn call_ticketed(
        &mut self,
        event: &str,
        params: Value,
        ticket: &str,
    ) -> Result<Value, BridgeError>;
    /// Return, without blocking, any spontaneous events queued since the last drain (from `call`'s
    /// queueing above, plus anything read off the socket right now).
    fn drain_events(&mut self) -> Vec<Value>;
}

/// Real transport: a `tungstenite` WebSocket connected to PPSSPP's debugger endpoint.
pub struct TungsteniteWs {
    socket: WebSocket<TcpStream>,
    pending_events: VecDeque<Value>,
    /// The default per-read socket timeout set at connect, restored after any `call_with_timeout`
    /// widens it for a single slow exchange.
    default_timeout: Duration,
}

impl TungsteniteWs {
    /// Connect to `ws://127.0.0.1:<port>/debugger` with the `debugger.ppsspp.org` subprotocol.
    /// `timeout` bounds the initial TCP connect/handshake and every subsequent blocking read/write.
    pub fn connect(port: u16, timeout: Duration) -> BridgeResult<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|err| {
            BridgeError::Emulator(format!(
                "connect PPSSPP debugger websocket at 127.0.0.1:{port}: {err}"
            ))
        })?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(timeout)).ok();
        stream.set_write_timeout(Some(timeout)).ok();

        let uri: Uri = format!("ws://127.0.0.1:{port}/debugger")
            .parse()
            .map_err(|err| BridgeError::Emulator(format!("invalid PPSSPP debugger URL: {err}")))?;
        let request = ClientRequestBuilder::new(uri).with_sub_protocol(PPSSPP_SUBPROTOCOL);
        let (socket, _response) = tungstenite::client(request, stream).map_err(|err| {
            BridgeError::Emulator(format!(
                "websocket handshake with PPSSPP debugger failed: {err}"
            ))
        })?;
        Ok(Self {
            socket,
            pending_events: VecDeque::new(),
            default_timeout: timeout,
        })
    }
}

impl TungsteniteWs {
    /// Send `{"event": event, ...params}` without waiting for any reply.
    fn send_request(&mut self, event: &str, params: Value) -> BridgeResult<()> {
        let mut obj = match params {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                return Err(BridgeError::BadParams(format!(
                    "params for {event} must be a JSON object, got {other}"
                )))
            }
        };
        obj.insert("event".into(), json!(event));
        let text = serde_json::to_string(&Value::Object(obj))?;
        self.socket.send(Message::from(text))?;
        Ok(())
    }

    /// Block until an event named `expect` arrives. A `{"event":"error", ...}` reply becomes
    /// `Err`. Any other event observed while waiting (a spontaneous log/breakpoint/stepping
    /// notification) is queued rather than dropped, so a later `drain_events` can surface it.
    fn read_until(&mut self, expect: &str) -> BridgeResult<Value> {
        loop {
            match self.socket.read()? {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(text.as_str())?;
                    let seen = value.get("event").and_then(Value::as_str).unwrap_or("");
                    if seen == "error" {
                        let message = value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("PPSSPP debugger returned an error")
                            .to_string();
                        return Err(BridgeError::Emulator(message));
                    }
                    if seen == expect {
                        return Ok(value);
                    }
                    // A spontaneous event (log line, breakpoint/stepping hit) arrived ahead of our
                    // reply — queue it instead of dropping it, so a later poll_events can surface it.
                    self.pending_events.push_back(value);
                }
                Message::Close(_) => {
                    return Err(BridgeError::Emulator(
                        "PPSSPP debugger closed the websocket".into(),
                    ));
                }
                // Ping/Pong/Frame carry no event payload; tungstenite answers pings automatically.
                _ => {}
            }
        }
    }

    /// Like `read_until`, but the reply must also echo back `ticket` — a reply named `expect` whose
    /// `ticket` differs (a stale ack from an earlier correlated request) is queued as a spontaneous
    /// event, not returned, so it can never satisfy the wrong call.
    fn read_until_ticketed(&mut self, expect: &str, ticket: &str) -> BridgeResult<Value> {
        loop {
            match self.socket.read()? {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(text.as_str())?;
                    let seen = value.get("event").and_then(Value::as_str).unwrap_or("");
                    if seen == "error" {
                        let message = value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("PPSSPP debugger returned an error")
                            .to_string();
                        return Err(BridgeError::Emulator(message));
                    }
                    let ticket_matches =
                        value.get("ticket").and_then(Value::as_str) == Some(ticket);
                    if seen == expect && ticket_matches {
                        return Ok(value);
                    }
                    self.pending_events.push_back(value);
                }
                Message::Close(_) => {
                    return Err(BridgeError::Emulator(
                        "PPSSPP debugger closed the websocket".into(),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl WsTransport for TungsteniteWs {
    fn call(&mut self, event: &str, params: Value) -> BridgeResult<Value> {
        self.send_request(event, params)?;
        self.read_until(event)
    }

    fn call_and_wait_for(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
    ) -> BridgeResult<Value> {
        self.send_request(event, params)?;
        self.read_until(expect_event)
    }

    fn call_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        timeout: Duration,
    ) -> BridgeResult<Value> {
        self.send_request(event, params)?;
        // Widen the read budget for just this one slow exchange, then restore the default so the
        // next ordinary read still fails fast. Restored on both the ok and error paths.
        self.socket.get_ref().set_read_timeout(Some(timeout)).ok();
        let result = self.read_until(event);
        self.socket
            .get_ref()
            .set_read_timeout(Some(self.default_timeout))
            .ok();
        result
    }

    fn call_ticketed(&mut self, event: &str, params: Value, ticket: &str) -> BridgeResult<Value> {
        let mut obj = match params {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                return Err(BridgeError::BadParams(format!(
                    "params for {event} must be a JSON object, got {other}"
                )))
            }
        };
        obj.insert("ticket".into(), json!(ticket));
        self.send_request(event, Value::Object(obj))?;
        self.read_until_ticketed(event, ticket)
    }

    fn drain_events(&mut self) -> Vec<Value> {
        let mut out: Vec<Value> = self.pending_events.drain(..).collect();
        if self.socket.get_mut().set_nonblocking(true).is_err() {
            return out;
        }
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(value) = serde_json::from_str::<Value>(text.as_str()) {
                        out.push(value);
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let _ = self.socket.get_mut().set_nonblocking(false);
        out
    }
}

/// One emucap-assigned breakpoint's PPSSPP-side identity, so `clear_breakpoint`/`list_breakpoints`
/// can find the right `cpu.breakpoint.remove`/`memory.breakpoint.remove` call and `poll_events` can
/// classify a stop as a hit on this breakpoint.
#[derive(Debug, Clone)]
struct PpssppBreakpoint {
    /// "exec" (routes through `cpu.breakpoint.*`) or "read"/"write" (routes through
    /// `memory.breakpoint.*`).
    kind: String,
    address: u64,
    /// Watched byte length — 1 (unused) for "exec", the `memory.breakpoint.add` `size` for a
    /// memory breakpoint.
    length: u64,
    /// Last-seen `memory.breakpoint.list` `hits` count for this breakpoint (memory kind only) — a
    /// hit-count delta is how `poll_events` attributes a stop to a specific memory breakpoint,
    /// since the stop event's `pc` is the accessing instruction, not the watched address.
    last_hits: u64,
}

pub struct PpssppBridge<T> {
    ws: T,
    bps: BTreeMap<u64, PpssppBreakpoint>,
    next_bp: u64,
    /// Stop events already drained from the transport but held back by a `poll_events`
    /// `breakpoint_id` filter that did not match them — returned on a later unfiltered/matching
    /// poll (mirrors the NDS bridge's `events` field).
    events: Vec<Value>,
    /// Content image path for `get_rom_info` — PPSSPP's own WS API never exposes a path or hash
    /// (`game.status`'s `game` object is just `{id, version, title}`), so this bridge computes it
    /// locally, same as the NDS/PC-98 bridges' `EMUCAP_CONTENT`.
    content: Option<PathBuf>,
    /// Identity-guard fields the launcher passes via `EMUCAP_NAME`/`EMUCAP_SESSION_TOKEN`
    /// (`src/launch/ppsspp.rs`) — `hello` echoes them back so `emucap-mcp`'s TCP handshake
    /// (`live/tcp.rs`'s `handshake_stream`) can confirm this bridge is the one it just spawned and
    /// not a stale/foreign process holding the port (mirrors the NDS/PC-98 bridges' `BridgeEnv`).
    name: Option<String>,
    session_token: Option<String>,
    launch_id: Option<String>,
    /// Monotonic counter minting a unique `ticket` for each timed `input.buttons.press`, so the
    /// bridge can correlate a delayed release ack to the exact press that issued it (see
    /// `press_buttons` / `WsTransport::call_ticketed`).
    next_ticket: u64,
}

impl<T: WsTransport> PpssppBridge<T> {
    pub fn new(ws: T) -> Self {
        let mut bridge = Self::with_identity(
            ws,
            std::env::var_os("EMUCAP_CONTENT").map(PathBuf::from),
            std::env::var("EMUCAP_NAME").ok(),
            std::env::var("EMUCAP_SESSION_TOKEN").ok(),
        );
        bridge.launch_id = std::env::var("EMUCAP_LAUNCH_ID").ok();
        bridge
    }

    /// Explicit-content constructor — `new()` reads `EMUCAP_CONTENT` from the process environment
    /// (set by the launcher alongside the PPSSPP debugger port); this lets tests supply the
    /// content path directly instead of mutating process-global env. `name`/`session_token` are
    /// left unset (use `with_identity` to exercise `hello`'s echo).
    pub fn with_content(ws: T, content: Option<PathBuf>) -> Self {
        Self::with_identity(ws, content, None, None)
    }

    /// Full constructor threading the identity-guard fields (`name`/`session_token`) alongside
    /// `content` — lets tests exercise `hello`'s echo without mutating process env.
    pub fn with_identity(
        ws: T,
        content: Option<PathBuf>,
        name: Option<String>,
        session_token: Option<String>,
    ) -> Self {
        Self {
            ws,
            bps: BTreeMap::new(),
            next_bp: 1,
            events: Vec::new(),
            content,
            name,
            session_token,
            launch_id: None,
            next_ticket: 1,
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
            "dump_memory" => self.dump_memory(&req.params),
            "get_state" => self.get_state(&req.params),
            "disassemble" => self.disassemble(&req.params),
            "set_breakpoint" => self.set_breakpoint(&req.params),
            "clear_breakpoint" => self.clear_breakpoint(&req.params),
            "list_breakpoints" => self.list_breakpoints(),
            "clear_all_breakpoints" => self.clear_all_breakpoints(),
            "step" => self.step(&req.params),
            "step_instructions" => self.step_instructions(&req.params),
            "pause" => self.pause(&req.params),
            "resume" => self.resume(&req.params),
            "poll_events" => self.poll_events(&req.params),
            "screenshot" => self.screenshot(),
            "set_input" => self.set_input(&req.params),
            "press_buttons" => self.press_buttons(&req.params),
            "save_state" => self.save_state(&req.params),
            "load_state" => self.load_state(&req.params),
            "reset" => self.reset(&req.params),
            other if UNSUPPORTED_METHODS.contains(&other) => {
                Err(BridgeError::Unsupported(other.into()))
            }
            other => Err(BridgeError::UnknownMethod(other.into())),
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

    fn status(&mut self) -> BridgeResult<Value> {
        let version = self.ws.call("version", json!({}))?;
        let game_status = self.ws.call("game.status", json!({}))?;
        // `game.status`'s own `paused` field is `GetUIState() == UISTATE_PAUSEMENU` (the GUI pause
        // menu) — unrelated to the CPU debugger's halt state, and stays `false` even while the CPU
        // is stopped at a breakpoint (verified empirically). `cpu.status.stepping` is the real
        // debugger-halt indicator pause/resume/step_instructions act on, so `state` is derived from
        // that, not from `game.status`.
        let stepping = self.cpu_is_stepping()?;
        let ppsspp_version = version
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        Ok(json!({
            "connected": true,
            "system": "psp",
            "adapter": "ppsspp-rust-ws",
            "backend": "ppsspp-debugger-ws",
            "debugger": true,
            "state": if stepping { "frozen" } else { "running" },
            "methods": METHODS,
            "memory_types": MEMORY_TYPES,
            "capability_notes": capability_notes(),
            "ppsspp_version": ppsspp_version,
            "game": game_status.get("game").cloned().unwrap_or(Value::Null),
            "input_buttons": psp_input_buttons_json(),
        }))
    }

    /// Identity-guard handshake — the connect-time surface a caller inspects before trusting the
    /// bridge. Unlike `status`, `methods` here is the same truthful `METHODS` list (not a live
    /// probe), since PPSSPP's WebSocket needs no per-request emulator round-trip to answer it.
    /// Echoes `name`/`session_token` (when the launcher set them via `EMUCAP_NAME`/
    /// `EMUCAP_SESSION_TOKEN`) so `emucap-mcp`'s TCP handshake can verify this bridge is the one it
    /// just spawned — without this echo the identity guard rejects every connection attempt with
    /// `IdentityMismatch` (mirrors the NDS/PC-98 bridges' `hello`).
    fn hello(&self) -> BridgeResult<Value> {
        let mut result = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "psp",
            "adapter": "ppsspp-rust-ws",
            "backend": "ppsspp-debugger-ws",
            "debugger": true,
            "methods": METHODS,
            "memory_types": MEMORY_TYPES,
            "capability_notes": capability_notes(),
            "input_buttons": psp_input_buttons_json(),
        });
        let obj = result.as_object_mut().expect("hello is an object");
        if let Some(name) = &self.name {
            obj.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.session_token {
            obj.insert("session_token".into(), json!(token));
        }
        if let Some(launch_id) = &self.launch_id {
            obj.insert("launch_id".into(), json!(launch_id));
        }
        Ok(result)
    }

    /// `memory.read {address, size}` → `{base64}`. `memory_type` resolves to an absolute PSP
    /// address (today only `main`, `PSP_MAIN_RAM_BASE + offset`); the base64 payload is decoded and
    /// re-emitted as hex to match the other adapters' `read_memory` wire shape.
    fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let length = required_num(params, "length")? as usize;
        if length > MAX_READ_LEN {
            return Err(BridgeError::BadParams(format!(
                "read length {length:#x} exceeds the {MAX_READ_LEN:#x} cap — read a large region in chunks (advance the start address)"
            )));
        }
        let addr = route_main_address(params, length as u64)?;
        let result = self
            .ws
            .call("memory.read", json!({ "address": addr, "size": length }))?;
        let b64 = result
            .get("base64")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BridgeError::Emulator("memory.read: reply had no base64 field".into())
            })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|err| {
                BridgeError::Emulator(format!("memory.read: base64 decode failed: {err}"))
            })?;
        Ok(json!({ "hex": hex::encode(bytes) }))
    }

    /// hex → base64 → `memory.write {address, base64}`. Same `memory_type` → absolute-address
    /// routing as `read_memory`.
    fn write_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let hexstr = required_str(params, "hex")?;
        if hexstr.len() % 2 != 0 {
            return Err(BridgeError::BadParams("hex must have even length".into()));
        }
        let bytes =
            hex::decode(hexstr).map_err(|_| BridgeError::BadParams("hex decode failed".into()))?;
        let addr = route_main_address(params, bytes.len() as u64)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        self.ws
            .call("memory.write", json!({ "address": addr, "base64": b64 }))?;
        Ok(json!({ "written": bytes.len() }))
    }

    /// Read exactly `len` bytes of `main` (user RAM) starting at `offset` into the region, via
    /// `memory.read` (base64 → raw bytes). `dump_memory` streams the whole region through this in
    /// `MAX_READ_LEN` chunks; the caller keeps `[offset, offset+len)` within `PSP_MAIN_RAM_SIZE`, so
    /// no out-of-region read reaches PPSSPP. A reply that decodes to fewer (or more) bytes than
    /// requested is a short read: it is rejected here so `dump_memory` fails cleanly rather than
    /// writing a `main.bin` smaller than the `regions.json` it advertises.
    fn read_main_bytes(&mut self, offset: u64, len: usize) -> BridgeResult<Vec<u8>> {
        let addr = PSP_MAIN_RAM_BASE.checked_add(offset).ok_or_else(|| {
            BridgeError::BadParams(format!("main address overflow at offset {offset:#x}"))
        })?;
        let result = self
            .ws
            .call("memory.read", json!({ "address": addr, "size": len }))?;
        let b64 = result
            .get("base64")
            .and_then(Value::as_str)
            .ok_or_else(|| BridgeError::Emulator("memory.read: reply had no base64 field".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|err| {
                BridgeError::Emulator(format!("memory.read: base64 decode failed: {err}"))
            })?;
        if bytes.len() != len {
            return Err(BridgeError::Emulator(format!(
                "memory.read at {addr:#x}: requested {len} bytes but PPSSPP returned {} (short read)",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    /// Bulk-export every PSP memory region under `path` as `<name>.bin` region files +
    /// `regions.json` + a `state.json` register snapshot — so huge memory goes to files, not inline,
    /// and `emucap diff` / the cross-ROM diff recipe read PSP dumps with the same loader as every
    /// other adapter (`src/analysis/dump.rs`, the `regions.json` shape's single source of truth).
    /// Mirrors the PC-98 bridge's `dump_memory`: each region is streamed via `memory.read` in
    /// `MAX_READ_LEN` chunks bounded to `PSP_MAIN_RAM_SIZE` (today only `main`, user RAM). `state.json`
    /// is the unwrapped `get_state` map — the same file the MCP host writes uniformly for any adapter
    /// (`src/live/tools.rs`), so a direct bridge dump is already diff-ready.
    fn dump_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        // The MCP host (src/live/tools.rs) hands us its own `dump-staging` sibling dir and does the
        // single atomic swap of the finished staging into the final dump location, so the bridge
        // writes region files directly into `path` with no dir-level swap of its own — mirroring the
        // NDS bridge. Each region streams to a `.partial` temp that is size-verified then atomically
        // renamed into `<name>.bin`, and `regions.json` is written last, so a short/failed
        // `memory.read` mid-stream leaves no truncated `.bin` and no manifest advertising one — the
        // host discards this whole staging dir on error.
        std::fs::create_dir_all(&path)?;
        let metas = self.build_dump(&path)?;
        // state.json is written by the MCP host uniformly for every adapter, so the bridge writes
        // only the region .bins + regions.json — matching the PC-98/NDS bridges (no redundant
        // get_state round-trip here that the host would immediately overwrite).
        Ok(json!({ "path": path.display().to_string(), "regions": metas.len() }))
    }

    /// Stream every `MEMORY_TYPES` region into `<name>.bin` under `dir` and write `regions.json`,
    /// verifying each `.bin` ends up exactly the region size. Each region streams to a `.partial`
    /// temp that is renamed into place only after its size is verified, and any read/write error
    /// discards that temp before propagating — so a partial dump leaves no truncated `.bin`. Returns
    /// the region metadata written.
    fn build_dump(&mut self, dir: &Path) -> BridgeResult<Vec<Value>> {
        let mut metas = Vec::new();
        for &name in MEMORY_TYPES {
            let (base, size) = match name {
                "main" => (PSP_MAIN_RAM_BASE, PSP_MAIN_RAM_SIZE),
                other => {
                    return Err(BridgeError::Emulator(format!(
                        "dump_memory: no extent known for region {other}"
                    )))
                }
            };
            let tmp_path = dir.join(format!(".{name}.bin.partial"));
            if let Err(e) = self.stream_region_to(&tmp_path, size) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e);
            }
            let written = std::fs::metadata(&tmp_path)?.len();
            if written != size {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(BridgeError::Emulator(format!(
                    "dump_memory: {name}.bin is {written:#x} bytes, expected region size {size:#x}"
                )));
            }
            std::fs::rename(&tmp_path, dir.join(format!("{name}.bin")))?;
            metas.push(json!({
                "name": name,
                "memory_type": name,
                "base_address": base,
                "size": size,
            }));
        }
        std::fs::write(dir.join("regions.json"), serde_json::to_vec(&metas)?)?;
        Ok(metas)
    }

    /// Stream `size` bytes of `main` (user RAM) into the file at `path`, reading PPSSPP in
    /// `MAX_READ_LEN` chunks. Any short/failed `memory.read` propagates so `build_dump` can discard
    /// the partial file.
    fn stream_region_to(&mut self, path: &Path, size: u64) -> BridgeResult<()> {
        let mut file = File::create(path)?;
        let mut offset = 0u64;
        while offset < size {
            let chunk = MAX_READ_LEN.min((size - offset) as usize);
            let bytes = self.read_main_bytes(offset, chunk)?;
            file.write_all(&bytes)?;
            // Advance by what was actually read (read_main_bytes already guarantees == chunk).
            offset += bytes.len() as u64;
        }
        file.flush()?;
        Ok(())
    }

    /// `cpu.getAllRegs` → the `id:0` ("GPR") category flattened into `cpu.<name>: value` (MIPS GPRs
    /// plus the fork-appended synthetic `pc`/`hi`/`lo`, per `CPUCoreSubscriber.cpp`). FPU/VFPU
    /// categories are out of scope for v1 — PSP has one CPU context, so unlike the NDS bridge's
    /// per-core `{"cpu": ..., "state": {...}}` this returns just `{"state": {...}}`.
    fn get_state(&mut self, _params: &Value) -> BridgeResult<Value> {
        Ok(json!({ "state": self.fetch_cpu_state()? }))
    }

    /// `cpu.getAllRegs` → the `id:0` ("GPR") category flattened into `cpu.<name>: value` (MIPS GPRs
    /// plus the fork-appended synthetic `pc`/`hi`/`lo`, per `CPUCoreSubscriber.cpp`). Shared by
    /// `get_state`, `step_instructions` (final state after stepping), and `poll_events` (regs on a
    /// stop event, which PPSSPP's `cpu.stepping` event never carries itself).
    fn fetch_cpu_state(&mut self) -> BridgeResult<Value> {
        let result = self.ws.call("cpu.getAllRegs", json!({}))?;
        let categories = result
            .get("categories")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BridgeError::Emulator("cpu.getAllRegs: reply had no categories array".into())
            })?;
        let gpr = categories
            .iter()
            .find(|c| c.get("id").and_then(Value::as_u64) == Some(0))
            .ok_or_else(|| {
                BridgeError::Emulator("cpu.getAllRegs: reply had no id:0 (GPR) category".into())
            })?;
        let names = gpr
            .get("registerNames")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BridgeError::Emulator("cpu.getAllRegs: GPR category had no registerNames".into())
            })?;
        let values = gpr
            .get("uintValues")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BridgeError::Emulator("cpu.getAllRegs: GPR category had no uintValues".into())
            })?;
        let mut state = serde_json::Map::new();
        for (name, value) in names.iter().zip(values.iter()) {
            if let (Some(name), Some(value)) = (name.as_str(), value.as_u64()) {
                state.insert(format!("cpu.{name}"), json!(value));
            }
        }
        Ok(Value::Object(state))
    }

    /// `memory.disasm {address, count}` → `[{addr, bytes, text}]`. `address` is a raw absolute PSP
    /// address (e.g. from `get_state`'s `cpu.pc`) — unlike `read_memory` this does not add a
    /// `memory_type` base, matching the NDS bridge's `disassemble` convention. `bytes` re-emits
    /// PPSSPP's `encoding` (the instruction word, MIPS is little-endian) as little-endian in-memory
    /// hex; `text` joins the mnemonic (`name`) and its formatted operands (`params`).
    fn disassemble(&mut self, params: &Value) -> BridgeResult<Value> {
        let addr = absolute_address(params)?;
        let count = optional_num(params, "count")?.unwrap_or(8).clamp(1, 4096);
        let result = self
            .ws
            .call("memory.disasm", json!({ "address": addr, "count": count }))?;
        let lines = result
            .get("lines")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BridgeError::Emulator("memory.disasm: reply had no lines array".into())
            })?;
        let mut instructions = Vec::with_capacity(lines.len());
        for line in lines {
            let addr = line.get("address").and_then(Value::as_u64).unwrap_or(0);
            let encoding = line.get("encoding").and_then(Value::as_u64).unwrap_or(0) as u32;
            let name = line.get("name").and_then(Value::as_str).unwrap_or("");
            let params = line.get("params").and_then(Value::as_str).unwrap_or("");
            let text = if params.is_empty() {
                name.to_string()
            } else {
                format!("{name} {params}")
            };
            instructions.push(json!({
                "addr": addr,
                "bytes": hex::encode(encoding.to_le_bytes()),
                "text": text,
            }));
        }
        Ok(json!({ "instructions": instructions }))
    }

    /// `cpu.status.stepping` — the real CPU-debugger halt indicator (see the `status()` note on why
    /// `game.status.paused` is not it).
    fn cpu_is_stepping(&mut self) -> BridgeResult<bool> {
        let status = self.ws.call("cpu.status", json!({}))?;
        Ok(status
            .get("stepping")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// kind `exec` → `cpu.breakpoint.add {address, enabled, condition?}`; kind `read`/`write` →
    /// `memory.breakpoint.add {address, size, read/write, condition?}`. Address resolution follows the
    /// kind. An exec `address`/`start` is a raw absolute PSP address — a PC straight from `get_state`'s
    /// `cpu.pc` or `disassemble` — and `memory_type` is ignored (a PC is not a `main` offset and is not
    /// always inside `main` RAM; PPSSPP's cpu breakpoint takes an absolute address either way). A
    /// read/write `address`/`start` is symmetric with `read_memory`/`write_memory`: a `memory_type`
    /// region offset routed through the same `route_main_address` (→ `PSP_MAIN_RAM_BASE + offset`,
    /// out-of-range rejected), so the watchpoint lands where `read_memory` reads instead of at a raw
    /// low address that never fires. `pc_min`/`pc_max` (optional) compile
    /// into a PPSSPP `condition` expression
    /// (`"(pc >= ..) && (pc <= ..)"`); an explicit `condition` string is passed through verbatim and
    /// ANDed with any pc_min/pc_max clauses — PPSSPP parses/validates it and a bad expression comes
    /// back as an `emulator_error`, not a silently-ignored one. `pause_on_hit` (default true) maps to
    /// PPSSPP's `enabled` (a `false` value is honored — unlike the NDS/GDB bridge, PPSSPP natively
    /// supports a log-only, non-pausing breakpoint). `auto_savestate`/`snapshot`/value filters
    /// (`value`/`value_mask`/`value_len`) are unsupported and rejected
    /// rather than silently ignored.
    fn set_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("exec")
            .to_string();
        if !matches!(kind.as_str(), "exec" | "read" | "write") {
            return Err(BridgeError::BadParams(format!(
                "psp bridge supports exec/read/write breakpoints (kind=exec|read|write); got kind={kind}"
            )));
        }
        if params.get("auto_savestate").and_then(Value::as_bool) == Some(true) {
            return Err(BridgeError::Unsupported(
                "psp bridge: auto_savestate is unsupported".into(),
            ));
        }
        if params
            .get("snapshot")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            return Err(BridgeError::Unsupported(
                "psp bridge: snapshot is unsupported — read_memory after the hit instead"
                    .into(),
            ));
        }
        for opt in ["value", "value_mask", "value_len"] {
            if params.get(opt).is_some() {
                return Err(BridgeError::Unsupported(format!(
                    "psp bridge: {opt} is unsupported — use pc_min/pc_max or a raw condition expression instead"
                )));
            }
        }
        // Watched span: 1 for an exec point (a single PC, no routing), the memory breakpoint's
        // `length` for read/write — used for both the routed bounds check and the memcheck size below.
        let route_len = if kind == "exec" {
            1
        } else {
            optional_num(params, "length")?.unwrap_or(1).max(1)
        };
        // Reject a range exec point (start != end): PPSSPP's cpu breakpoint is a single address, not
        // a span. `end` and `start` are compared as-is — both are raw absolute addresses (an exec
        // breakpoint does not route, unlike a read/write watchpoint), so the comparison is direct.
        if kind == "exec" {
            if let Some(end) = optional_num(params, "end")? {
                if end != region_offset(params)? {
                    return Err(BridgeError::Unsupported(
                        "psp bridge: range exec breakpoints are unsupported — single address only (start==end)"
                            .into(),
                    ));
                }
            }
        }
        let address = route_breakpoint_address(&kind, params, route_len)?;
        let pause_on_hit = params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let condition = breakpoint_condition(params)?;

        if kind == "exec" {
            // Range/end check already done above, in caller offset coordinates, before routing.
            // A same-address exec duplicate is accepted (not refused) and refcounted by
            // clear_breakpoint: an exec hit is attributed by PC == address, so duplicates are
            // semantically equivalent — unlike a memory read/write pair on one range, which the shared
            // memcheck cannot tell apart (refused below). This mirrors the memory branch, which also
            // accepts same-kind duplicates without comparing options; PPSSPP's single cpu breakpoint
            // holds the last-written enabled/condition, so a caller wanting distinct conditions uses
            // distinct addresses or clears the existing one first.
            let mut req = json!({ "address": address, "enabled": pause_on_hit });
            if let Some(cond) = &condition {
                req["condition"] = json!(cond);
            }
            self.ws.call("cpu.breakpoint.add", req)?;
            let id = self.next_bp;
            self.next_bp += 1;
            self.bps.insert(
                id,
                PpssppBreakpoint {
                    kind: kind.clone(),
                    address,
                    length: 1,
                    last_hits: 0,
                },
            );
            Ok(json!({ "id": id, "kind": kind, "address": address }))
        } else {
            let length = route_len;
            // PPSSPP keeps ONE memcheck per (address, size) with a single shared hit counter and no
            // per-access attribution. A read and a write breakpoint on the SAME range would collapse
            // into that one memcheck, so a hit could not be told apart between the two bridge ids
            // (enrich_stop would credit whichever sorts first). Refuse the ambiguous pair rather than
            // advertise a disambiguation the shared counter cannot provide. Same-kind duplicates are
            // fine (equivalent) and are refcounted by clear_breakpoint.
            if let Some(existing_kind) = self
                .bps
                .values()
                .find(|bp| {
                    bp.kind != "exec"
                        && bp.kind != kind
                        && bp.address == address
                        && bp.length == length
                })
                .map(|bp| bp.kind.clone())
            {
                return Err(BridgeError::BadParams(format!(
                    "psp bridge: a {existing_kind} breakpoint already watches {address:#x}+{length}; PPSSPP \
                     shares one memcheck and hit counter per (address, size), so a {kind} breakpoint on the \
                     same range could not be distinguished from it on a hit. Clear the existing one first, or \
                     watch a different address/size."
                )));
            }
            let mut req = json!({
                "address": address,
                "size": length,
                "enabled": pause_on_hit,
                "read": kind == "read",
                "write": kind == "write",
            });
            if let Some(cond) = &condition {
                req["condition"] = json!(cond);
            }
            self.ws.call("memory.breakpoint.add", req)?;
            // Seed `last_hits` from PPSSPP's ACTUAL current `numHits` for this memcheck, read back
            // from `memory.breakpoint.list`, not from a bridge-side sibling. `enrich_stop` attributes
            // a stop to the first memory breakpoint whose `hits` grew since its `last_hits`, so a
            // mismatch between `last_hits` and PPSSPP's live counter either fabricates a hit (seed too
            // low) or swallows a real one (seed too high). PPSSPP keeps ONE memcheck per address/size:
            // it PRESERVES `numHits` when a duplicate re-adds an existing memcheck, but RESETS it to 0
            // when the memcheck is removed and later recreated (`Core/Debugger/Breakpoints.cpp`). A
            // sibling's `last_hits` is stale in the clear-then-re-add case — clearing one duplicate
            // removes the shared memcheck (numHits→gone), so a still-tracked sibling holds 1 while a
            // freshly re-added memcheck starts at 0; inheriting that 1 would miss the first real hit.
            // Reading the live counter is the only value that matches what `enrich_stop` compares
            // against, and it also covers the plain duplicate case (list returns the preserved count).
            // If the read-back fails or the entry is absent, fall back to 0 — the bp was still added,
            // and 0 favors reporting a possibly-stale hit over silently missing a real one.
            let last_hits = self
                .ws
                .call("memory.breakpoint.list", json!({}))
                .ok()
                .and_then(|list| {
                    list.get("breakpoints")
                        .and_then(Value::as_array)
                        .and_then(|entries| {
                            entries.iter().find_map(|e| {
                                let matches = e.get("address").and_then(Value::as_u64)
                                    == Some(address)
                                    && e.get("size").and_then(Value::as_u64) == Some(length);
                                matches
                                    .then(|| e.get("hits").and_then(Value::as_u64))
                                    .flatten()
                            })
                        })
                })
                .unwrap_or(0);
            let id = self.next_bp;
            self.next_bp += 1;
            self.bps.insert(
                id,
                PpssppBreakpoint {
                    kind: kind.clone(),
                    address,
                    length,
                    last_hits,
                },
            );
            Ok(json!({ "id": id, "kind": kind, "address": address, "length": length }))
        }
    }

    fn clear_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let id = required_num(params, "id")?;
        let bp = self
            .bps
            .get(&id)
            .cloned()
            .ok_or_else(|| BridgeError::BadParams(format!("unknown breakpoint id: {id}")))?;
        if bp.kind == "exec" {
            // PPSSPP keeps ONE cpu breakpoint per address; several bridge ids may map to it (a
            // duplicate set_breakpoint, or a retry after a lost response). Only tear it down when THIS
            // id is the last bridge exec breakpoint on that address — otherwise removing it would
            // silently disarm the still-tracked survivor (it would stay in list_breakpoints but never
            // halt again). exec breakpoints are single-address (no size), so the survivor check is by
            // address alone — the same refcount discipline as the memory branch below.
            let survivor_shares_address = self.bps.iter().any(|(&other, ob)| {
                other != id && ob.kind == "exec" && ob.address == bp.address
            });
            if !survivor_shares_address {
                self.ws
                    .call("cpu.breakpoint.remove", json!({ "address": bp.address }))?;
            }
        } else {
            // PPSSPP keeps ONE memcheck per (address, size); several bridge ids may share it (same-kind
            // duplicates). Only tear the memcheck down when THIS id was the last bridge breakpoint on
            // that range — otherwise removing it would silently disarm the still-tracked siblings (they
            // would stay in list_breakpoints but never stop again). Cross-kind coexistence is refused
            // at add time, so any survivor shares this range's access mode: leaving the memcheck as-is
            // is exactly the union of the remaining read/write modes, so no re-add is needed.
            let survivor_shares_range = self.bps.iter().any(|(&other, ob)| {
                other != id
                    && ob.kind != "exec"
                    && ob.address == bp.address
                    && ob.length == bp.length
            });
            if !survivor_shares_range {
                self.ws.call(
                    "memory.breakpoint.remove",
                    json!({ "address": bp.address, "size": bp.length }),
                )?;
            }
        }
        self.bps.remove(&id);
        Ok(json!({ "cleared": id }))
    }

    fn list_breakpoints(&self) -> BridgeResult<Value> {
        let mut rows = Vec::new();
        for (id, bp) in &self.bps {
            let mut row = json!({ "id": id, "kind": bp.kind, "address": bp.address });
            if bp.kind != "exec" {
                row["length"] = json!(bp.length);
            }
            rows.push(row);
        }
        Ok(json!({ "breakpoints": rows }))
    }

    fn clear_all_breakpoints(&mut self) -> BridgeResult<Value> {
        let mut cleared = Vec::new();
        for id in self.bps.keys().copied().collect::<Vec<_>>() {
            if self.clear_breakpoint(&json!({ "id": id })).is_ok() {
                cleared.push(id);
            }
        }
        Ok(json!({ "cleared": cleared }))
    }

    /// `cpu.stepping` — no-op (idempotent) if the CPU is already stepping, since PPSSPP's own
    /// `WebSocketCPUStepping` silently does nothing when already stepping (no `Core_Break`, no
    /// state-change, so no ack event ever arrives) — calling it unconditionally would hang the
    /// bridge waiting for an ack that never comes.
    fn pause(&mut self, _params: &Value) -> BridgeResult<Value> {
        if !self.cpu_is_stepping()? {
            self.ws.call("cpu.stepping", json!({}))?;
        }
        Ok(json!({ "state": "frozen" }))
    }

    /// `cpu.resume` — no-op (idempotent) if the CPU is already running, mirroring `pause` (PPSSPP's
    /// `WebSocketCPUResume` fails with "CPU not stepping" when called on a running CPU).
    fn resume(&mut self, _params: &Value) -> BridgeResult<Value> {
        if self.cpu_is_stepping()? {
            self.ws.call("cpu.resume", json!({}))?;
        }
        Ok(json!({ "state": "running" }))
    }

    /// Wire method `step` — both MCP step tools arrive here (same as the NDS bridge): the
    /// instruction-step tool sends `{frames:n, unit:"instructions"}`, the frame-step tool sends
    /// `{frames:n}` with no `unit`. PPSSPP has no frame-advance primitive (see the adapter README),
    /// so only the instruction-unit case is honored — a frame-step request is rejected rather than
    /// silently reinterpreted as an instruction count (which would make a 60-frame advance step 60
    /// instructions and derail freeze-step/tap).
    /// `unit:"instructions"` (and the lenient bare `step` with no unit and no `frames`) route to the
    /// same `cpu.stepInto` logic as the `step_instructions` wire method.
    ///
    /// Advertisement: this wire method is *not* in `METHODS` (so the MCP's `has("step")` frame-step
    /// composites — `tap`/`tap_sequence`/`hold_until` — stay correctly disabled on PSP, since they
    /// drive frame `step` which PPSSPP cannot do), and it is *not* claimed as "planned" either
    /// (frame-step is a permanent gap, not a pending feature). The stepping that does work is
    /// advertised as `step_instructions` in `METHODS` plus `step_units == ["instructions"]`.
    /// The dispatch arm stays because the shared MCP protocol delivers instruction stepping *as*
    /// wire `step {unit:"instructions"}` (see `src/live/tools.rs`), and because dispatching it lets
    /// a frame-step request return a precise `unsupported` rather than `unknown_method`.
    fn step(&mut self, params: &Value) -> BridgeResult<Value> {
        match params.get("unit").and_then(Value::as_str) {
            Some("instructions") => {}
            Some(other) => {
                return Err(BridgeError::Unsupported(format!(
                    "step unit={other} (psp bridge steps by instructions only — PPSSPP has no frame-advance)"
                )));
            }
            None => {
                if params.get("frames").is_some() {
                    return Err(BridgeError::Unsupported(
                        "psp bridge: frame step unsupported — PPSSPP has no frame-advance primitive. \
                         Use step_instructions (instruction-unit stepping) instead."
                            .into(),
                    ));
                }
            }
        }
        self.step_instructions(params)
    }

    /// `cpu.stepInto`, called `count` times (PPSSPP has no step-count parameter — see
    /// `SteppingSubscriber.cpp`). Ensures the CPU is stepping first: `cpu.stepInto` on a *running*
    /// CPU just pauses it without executing anything (`WebSocketSteppingState::Into`'s
    /// `if (!Core_IsStepping()) { Core_Break(...); return; }` branch) — real single-instruction
    /// stepping only happens once already stepping. Each `cpu.stepInto` acks via a `cpu.stepping`
    /// event (a different name — see the module doc), so this rides `call_and_wait_for`, not `call`.
    /// The final `pc`/`state` come from a fresh `cpu.getAllRegs`, not the ack event's own `pc` field
    /// (undocumented-accurate only "while stepping", and this bridge does not depend on its
    /// precision).
    fn step_instructions(&mut self, params: &Value) -> BridgeResult<Value> {
        let count = step_count(params)?;
        if !self.cpu_is_stepping()? {
            self.ws.call("cpu.stepping", json!({}))?;
        }
        for _ in 0..count {
            self.ws
                .call_and_wait_for("cpu.stepInto", json!({}), "cpu.stepping")?;
        }
        let state = self.fetch_cpu_state()?;
        let pc = state.get("cpu.pc").and_then(Value::as_u64);
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "count": count,
            "pc": pc,
            "state": state,
        }))
    }

    /// Drain PPSSPP's spontaneous events, keep the `cpu.stepping` stops (a breakpoint hit or a
    /// stepping-request completion — PPSSPP does not distinguish them at the wire, see the module
    /// doc), and normalize each into `{type, pc, ticks, regs, [breakpoint_id, kind, address, id]}`.
    /// Non-stop events (`cpu.resume`, `input.*`, log lines, ...) are dropped, not queued — mirroring
    /// the NDS bridge dropping its own SIGINT stops. `breakpoint_id` filters like the NDS bridge: a
    /// non-matching event is held in `self.events` for a later poll instead of being dropped.
    fn poll_events(&mut self, params: &Value) -> BridgeResult<Value> {
        // Validate the `breakpoint_id` filter BEFORE draining the transport (which is destructive) or
        // touching `self.events`: a malformed filter must fail without consuming — and thereby losing
        // forever — already-buffered breakpoint-hit events.
        let filter_id = optional_num(params, "breakpoint_id")?;
        let raw = self.ws.drain_events();
        let mut fresh = Vec::new();
        // Count the spontaneous events this drain actually discards (log lines, `cpu.resume`,
        // `input.*`, ... — anything that is not a `cpu.stepping` stop), rather than reporting a
        // hardcoded 0. A `breakpoint_id` filter below does NOT drop — it holds non-matching stops
        // in `self.events` for a later poll — so those are not counted here.
        let mut dropped = 0u64;
        for event in raw {
            if event.get("event").and_then(Value::as_str) != Some("cpu.stepping") {
                dropped += 1;
                continue;
            }
            fresh.push(self.enrich_stop(event)?);
        }
        let mut all = std::mem::take(&mut self.events);
        all.append(&mut fresh);

        let mut out = Vec::new();
        for event in all {
            let matches_filter = match filter_id {
                Some(fid) => event.get("breakpoint_id").and_then(Value::as_u64) == Some(fid),
                None => true,
            };
            if matches_filter {
                out.push(event);
            } else {
                self.events.push(event);
            }
        }
        Ok(json!({ "events": out, "dropped": dropped }))
    }

    /// Build the base `{type:"stop", pc, ticks, regs}` shape from a raw `cpu.stepping` event, then
    /// classify it as a breakpoint hit if possible. An exec breakpoint is matched directly (the
    /// event's `pc` equals the breakpoint address). A memory breakpoint cannot be matched that way
    /// — the event's `pc` is the accessing instruction's address, not the watched address — so it is
    /// attributed via a `memory.breakpoint.list` hit-count delta: the first tracked memory
    /// breakpoint whose `hits` grew since the last check is reported as the source. Simultaneous
    /// memory breakpoint hits are a known best-effort limitation (only one is attributed per event).
    fn enrich_stop(&mut self, event: Value) -> BridgeResult<Value> {
        let pc = event.get("pc").and_then(Value::as_u64);
        let ticks = event.get("ticks").cloned().unwrap_or(Value::Null);
        let mut out = json!({ "type": "stop", "pc": pc, "ticks": ticks });
        match self.fetch_cpu_state() {
            Ok(state) => out["regs"] = state,
            Err(err) => out["regs_error"] = json!(err.to_string()),
        }

        if let Some(pc) = pc {
            if let Some((&id, _)) = self
                .bps
                .iter()
                .find(|(_, bp)| bp.kind == "exec" && bp.address == pc)
            {
                mark_breakpoint_hit(&mut out, id, "exec", pc);
                return Ok(out);
            }
        }

        if self.bps.values().any(|bp| bp.kind != "exec") {
            if let Ok(list) = self.ws.call("memory.breakpoint.list", json!({})) {
                if let Some(entries) = list.get("breakpoints").and_then(Value::as_array) {
                    let mut hit = None;
                    for (&id, bp) in self.bps.iter_mut() {
                        if bp.kind == "exec" {
                            continue;
                        }
                        let Some(entry) = entries.iter().find(|e| {
                            e.get("address").and_then(Value::as_u64) == Some(bp.address)
                                && e.get("size").and_then(Value::as_u64) == Some(bp.length)
                        }) else {
                            continue;
                        };
                        let hits = entry.get("hits").and_then(Value::as_u64).unwrap_or(0);
                        if hits > bp.last_hits {
                            bp.last_hits = hits;
                            if hit.is_none() {
                                hit = Some((id, bp.kind.clone(), bp.address));
                            }
                        }
                    }
                    if let Some((id, kind, address)) = hit {
                        mark_breakpoint_hit(&mut out, id, &kind, address);
                    }
                }
            }
        }
        Ok(out)
    }

    /// `emucap.screenshot` — the fork's GE-stepping-driving variant of stock
    /// `gpu.buffer.screenshot`; unlike the stock command (which fails with "Neither CPU or GPU is
    /// stepping" unless a screenshot request happens to land while already GE-stepping),
    /// `emucap.screenshot` forces GE stepping itself so a capture works while the game is
    /// running. Known v1 limitation: this only works while the CPU is actually *running* — if the
    /// CPU is halted for the debugger (`cpu.stepping`/breakpoint stop), the EmuThread never
    /// reaches a vsync to enter GE stepping, so the fork's own 5s wait would time out and the
    /// underlying `emucap.screenshot` request would fail loudly ~5s later. A halted core is
    /// rejected up front instead (mirroring `press_buttons`' halted-CPU guard) so a caller gets a
    /// fast, clear error instead of a multi-second stall — resume first, or use `get_state`/
    /// `poll_events` while frozen. Requests the default `type:"uri"` reply (a
    /// `data:image/png;base64,...` URI, same shape as `gpu.buffer.screenshot`) and decodes it to
    /// the uniform `{png_base64, width, height}`.
    fn screenshot(&mut self) -> BridgeResult<Value> {
        if self.cpu_is_stepping()? {
            return Err(BridgeError::BadParams(
                "screenshot needs a running emulator — emucap.screenshot drives GE stepping, \
                 which only progresses while the CPU is running; while halted for the debugger \
                 it would stall for PPSSPP's own ~5s wait then fail (resume first)."
                    .into(),
            ));
        }
        let result = self.ws.call("emucap.screenshot", json!({}))?;
        let uri = result.get("uri").and_then(Value::as_str).ok_or_else(|| {
            BridgeError::Emulator("emucap.screenshot: reply had no uri field".into())
        })?;
        let b64 = uri.strip_prefix("data:image/png;base64,").ok_or_else(|| {
            let head: String = uri.chars().take(32).collect();
            BridgeError::Emulator(format!(
                "emucap.screenshot: unexpected uri prefix: {head:?}"
            ))
        })?;
        let width = result
            .get("width")
            .and_then(Value::as_u64)
            .unwrap_or(PSP_SCREEN_WIDTH);
        let height = result
            .get("height")
            .and_then(Value::as_u64)
            .unwrap_or(PSP_SCREEN_HEIGHT);
        Ok(json!({
            "png_base64": b64,
            "format": "png",
            "width": width,
            "height": height,
        }))
    }

    /// `input.buttons.send {buttons: {<psp_name>: bool}}` — a full replacement of the held button
    /// set (every emucap button name is written explicitly true/false), matching the NDS bridge's
    /// full-mask `set_input` semantics: an empty `buttons` list releases everything currently held
    /// (the `tools.rs` combo/tap helpers rely on that to clean up after a press). PPSSPP's own
    /// `input.buttons.send` is itself a *partial* update — unlisted keys are left alone — so
    /// writing every uniform button explicitly is what turns it into a full "set", not a merge.
    /// Both `__CtrlUpdateButtons` and `req.Respond()` are synchronous (no frame wait), so this
    /// works regardless of whether the CPU is running or halted.
    fn set_input(&mut self, params: &Value) -> BridgeResult<Value> {
        let requested = button_list(params.get("buttons"))?;
        let mut buttons_obj = serde_json::Map::new();
        for name in PSP_INPUT_BUTTONS {
            let psp_name = psp_button_name(name).expect("PSP_INPUT_BUTTONS names all map");
            buttons_obj.insert(psp_name.into(), json!(requested.iter().any(|r| r == name)));
        }
        self.ws.call(
            "input.buttons.send",
            json!({ "buttons": Value::Object(buttons_obj) }),
        )?;
        Ok(json!({ "buttons": requested }))
    }

    /// `input.buttons.press {button, duration}` for one requested button. PPSSPP has no multi-button
    /// timed-press command (`ButtonsPress` takes exactly one `button`), so accepting a combo would
    /// silently serialize it and violate the common same-frame-window contract. Multi-button lists
    /// are rejected before any WS mutation until a fork-owned combo command exists. PPSSPP acks the
    /// press asynchronously under the *same* event name once `duration` frames have elapsed and
    /// the button auto-releases (`WebSocketInputState::Broadcast`), so this rides `call_ticketed`
    /// (the ack name matches the request name): each press carries a unique `ticket` PPSSPP echoes
    /// on its release ack, so a late ack from a *different* press can't be misattributed to this one.
    /// That auto-release never fires while the core is halted (frames only advance while running).
    /// A halted core is rejected up front (mirroring the NDS bridge's frozen-timed-input guard), but
    /// a breakpoint can still halt the CPU *mid-press* — after the pre-check passes — stranding the
    /// ack; on that timeout this releases every input (so the button doesn't stay held) and returns
    /// a clear error, and the stale ack is ignored by ticket correlation. `frames` is capped at
    /// `MAX_PRESS_FRAMES` for the same reason `read_memory` caps `length`: an uncapped hold blocks
    /// the call past the bridge's own WS read timeout.
    fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        let requested = button_list(params.get("buttons"))?;
        if requested.is_empty() {
            return Err(BridgeError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        if requested.len() > 1 {
            return Err(BridgeError::BadParams(
                "PPSSPP press_buttons currently supports exactly one button: stock PPSSPP has no atomic timed-combo command, and sequential presses would violate the simultaneous frame-window contract. Use set_input for an explicit persistent combo and set_input([]) to release it, or send single-button pulses."
                    .into(),
            ));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        if frames > MAX_PRESS_FRAMES {
            return Err(BridgeError::BadParams(format!(
                "press_buttons frames {frames} exceeds the {MAX_PRESS_FRAMES} cap (~4s at 60fps) \
                 — a longer hold risks PPSSPP's ack arriving after the bridge's own 8s WS read \
                 timeout, which then misattributes the late reply to an unrelated request; hold \
                 longer with repeated press_buttons calls or set_input instead."
            )));
        }
        if self.cpu_is_stepping()? {
            return Err(BridgeError::BadParams(
                "press_buttons needs a running emulator — while the CPU is halted for the \
                 debugger, frames never advance, so PPSSPP's timed press never auto-releases \
                 (resume first, or use set_input to hold instead)."
                    .into(),
            ));
        }
        let name = &requested[0];
        let psp_name = psp_button_name(name).expect("validated by button_list");
        let ticket = self.mint_ticket();
        // Ticket-correlated so a stale ack from a *previous* press (one whose release was
        // stranded when a breakpoint halted the CPU mid-press, then fired late on resume) can't
        // satisfy this call — it carries the earlier ticket and is queued/ignored.
        if let Err(err) = self.ws.call_ticketed(
            "input.buttons.press",
            json!({ "button": psp_name, "duration": frames }),
            &ticket,
        ) {
            // The press ack only fires after `duration` frames elapse. If a breakpoint
            // halts the CPU before then, frames stop, the auto-release never runs, and this
            // read times out with the button still held. Release everything best-effort so
            // the button doesn't stay stuck, then surface a clear error. The late ack (this
            // ticket) is ignored by every later ticketed read.
            let _ = self.release_all_inputs();
            if is_timeout_error(&err) {
                return Err(BridgeError::Emulator(format!(
                    "press_buttons({name}) timed out waiting for the timed release — the \
                     CPU likely halted (breakpoint) mid-press so frames stopped advancing. \
                     Inputs were released; resume and retry, or hold with set_input instead."
                )));
            }
            return Err(err);
        }
        Ok(json!({ "buttons": requested, "frames": frames }))
    }

    /// Mint a unique ticket string for a correlated request (see `press_buttons`).
    fn mint_ticket(&mut self) -> String {
        let n = self.next_ticket;
        self.next_ticket += 1;
        format!("emucap-{n}")
    }

    /// Release every held button — `input.buttons.send` with all buttons explicitly false. Used to
    /// recover after a timed press is interrupted mid-flight (the button was pressed but its timed
    /// release never ran). Synchronous on the PPSSPP side, so it works even while the CPU is halted.
    fn release_all_inputs(&mut self) -> BridgeResult<Value> {
        let mut buttons_obj = serde_json::Map::new();
        for name in PSP_INPUT_BUTTONS {
            let psp_name = psp_button_name(name).expect("PSP_INPUT_BUTTONS names all map");
            buttons_obj.insert(psp_name.into(), json!(false));
        }
        self.ws.call(
            "input.buttons.send",
            json!({ "buttons": Value::Object(buttons_obj) }),
        )
    }

    /// Write a PPSSPP savestate to `path` via the emucap fork's `savestate.save` (stock PPSSPP
    /// exposes no WebSocket savestate command — `SaveState::Save`/`Load` are async and normally
    /// only serviced while the EmuThread is stepping; the fork's handler breaks the CPU into
    /// stepping if running, waits for the save to complete, then restores the prior run state).
    fn save_state(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = required_str(params, "path")?.to_string();
        // Dedicated read budget above the fork's 15s save wait (see SAVESTATE_READ_TIMEOUT) — the
        // default 8s would time out mid-save and desync the channel.
        self.ws.call_with_timeout(
            "savestate.save",
            json!({ "path": path.clone() }),
            SAVESTATE_READ_TIMEOUT,
        )?;
        Ok(json!({ "path": path, "status": "completed" }))
    }

    /// Restore a PPSSPP savestate from `path` via the emucap fork's `savestate.load`.
    fn load_state(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = required_str(params, "path")?.to_string();
        // Same dedicated budget as save_state — the fork's load handler shares the 15s wait.
        self.ws.call_with_timeout(
            "savestate.load",
            json!({ "path": path.clone() }),
            SAVESTATE_READ_TIMEOUT,
        )?;
        Ok(json!({ "path": path, "status": "completed" }))
    }

    /// Power-cycle via `game.reset`. The emucap fork's headless build performs a *real* reboot on
    /// its emu-thread run loop (`PSP_Shutdown` + re-init from the same content) — stock headless
    /// dropped `REQUEST_GAME_RESET` on the floor (`System_PostUIMessage` was an empty stub), so
    /// `game.reset` was a silent no-op. The fork's WS handler now blocks its ack until the reboot
    /// actually completes, so this call gets a synchronous "rebooted" acknowledgement; it just needs
    /// a read budget above that reboot wait (a commercial title reloads modules for a few seconds),
    /// the same way `save_state` outlasts the fork's save wait. Bridge-side breakpoints are left
    /// tracked as-is: PPSSPP's breakpoints live in a WS-side global registry (`g_breakpoints`) that
    /// survives the reset, so `self.bps` stays in sync (the next run-loop `g_breakpoints.Frame()`
    /// re-arms them against the freshly booted code). Mirroring the initial launch, the headless
    /// reboot leaves the CPU halted at the fresh boot entry (state `frozen`) so a caller can re-arm
    /// breakpoints before running — resume to run it. `post_reset_pc` reports that boot-entry pc as
    /// verifiable evidence the machine really rebooted (a no-op would leave the pc progressing deep
    /// in-game).
    ///
    /// Only the headless fork blocks the ack — a display:true GUI session does not (its
    /// `game.reset` posts a UI message and returns immediately, and the async reboot keeps the core
    /// running rather than halting). So the ack alone does not prove the reboot completed: this
    /// confirms the halted state (`wait_for_reset_halt`) before claiming `completed`. When the core
    /// is halted the reboot is confirmed and `post_reset_pc` is the boot entry; when it is still
    /// running the reboot is in flight, so `reset` reports `status:"rebooting"` and withholds a
    /// `post_reset_pc` — the live pc there is a stale in-game value, not reset evidence. `status` is
    /// the single source of truth for whether the core halted (`completed`) or is still rebooting.
    fn reset(&mut self, _params: &Value) -> BridgeResult<Value> {
        self.ws
            .call_with_timeout("game.reset", json!({}), RESET_READ_TIMEOUT)?;
        if !self.wait_for_reset_halt()? {
            // The core never halted: the reboot is running asynchronously (display:true GUI). Report
            // that instead of a false "completed" with the still-in-game pc — get_state /
            // poll_events track the reboot as it settles.
            return Ok(json!({ "status": "rebooting" }));
        }
        let mut result = json!({ "status": "completed" });
        // Halted at the fresh boot entry — surface that pc as verifiable reset evidence.
        if let Ok(state) = self.fetch_cpu_state() {
            if let Some(pc) = state.get("cpu.pc").and_then(Value::as_u64) {
                result["post_reset_pc"] = json!(pc);
            }
        }
        Ok(result)
    }

    /// Poll `cpu.status.stepping` after a `game.reset` ack to confirm the reboot left the CPU halted
    /// at the fresh boot entry. Returns `true` on the first poll that reads halted (the headless
    /// path, which acks already-halted, returns immediately with no sleep); returns `false` if the
    /// core is still running after `RESET_HALT_POLLS` tries (a display:true GUI session, whose async
    /// reboot keeps the core running). See `reset`.
    fn wait_for_reset_halt(&mut self) -> BridgeResult<bool> {
        for attempt in 0..RESET_HALT_POLLS {
            if self.cpu_is_stepping()? {
                return Ok(true);
            }
            if attempt + 1 < RESET_HALT_POLLS {
                std::thread::sleep(RESET_HALT_POLL_INTERVAL);
            }
        }
        Ok(false)
    }

    /// `game.status` for the running disc's id/version/title plus a locally computed sha1 of the
    /// content image at `EMUCAP_CONTENT` — PPSSPP's WS API never exposes a content path or hash
    /// itself (`game.status`'s `game` object is just `{id, version, title}`, see
    /// `GameSubscriber.cpp`). Shape mirrors the NDS bridge's `get_rom_info`
    /// (`name`/`path`/`sha1`/`size`/`media_type`); `sha1` is what `emucap-mcp`'s
    /// `normalize_rom_sha1` promotes to the uniform `rom_sha1` field.
    fn get_rom_info(&mut self) -> BridgeResult<Value> {
        let content = self.content.clone().ok_or_else(|| {
            BridgeError::BadParams("EMUCAP_CONTENT is not set for get_rom_info".into())
        })?;
        if !content.is_file() {
            return Err(BridgeError::BadParams(format!(
                "content image not found: {}",
                content.display()
            )));
        }
        let game_status = self.ws.call("game.status", json!({}))?;
        Ok(json!({
            "system": "psp",
            "adapter": "ppsspp-rust-ws",
            "name": content.file_name().and_then(|s| s.to_str()).unwrap_or(""),
            "path": absolute_display(&content),
            "sha1": sha1_file(&content)?,
            "size": content.metadata()?.len(),
            "media_type": content.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase(),
            "game": game_status.get("game").cloned().unwrap_or(Value::Null),
        }))
    }
}

/// True when `err` is a socket read that timed out (the transport's per-read budget elapsed) — as
/// opposed to a PPSSPP-reported error or a protocol/parse failure. A tungstenite read timeout
/// surfaces as `Ws(Io(WouldBlock|TimedOut))`; `FakeWs` models it as `Io(WouldBlock)`.
fn is_timeout_error(err: &BridgeError) -> bool {
    fn is_timeout_io(e: &std::io::Error) -> bool {
        matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    }
    match err {
        BridgeError::Io(e) => is_timeout_io(e),
        BridgeError::Ws(tungstenite::Error::Io(e)) => is_timeout_io(e),
        _ => false,
    }
}

fn capability_notes() -> Value {
    // `planned_methods` discloses every real emucap tool name this bridge doesn't dispatch yet —
    // both concretely planned (`PLANNED_METHODS`) and platform-gapped (`UNSUPPORTED_METHODS`, which
    // resolve to an `unsupported` error rather than `unknown_method`) — so a caller can see the
    // not-yet-here surface without a trial call.
    let mut planned: Vec<&str> = PLANNED_METHODS.to_vec();
    planned.extend_from_slice(UNSUPPORTED_METHODS);
    json!({
        "backend": "ppsspp-debugger-ws",
        "rust_bridge": true,
        "implemented_methods": METHODS,
        "planned_methods": planned,
        "screenshot": true,
        "input": true,
        "press_buttons_max_simultaneous": 1,
        "frame_step": false,
        "step_units": ["instructions"],
        "breakpoints": true,
        "watch_register": false,
        "trace": false,
        "state_restore": true,
        "disassemble": true,
        "call_stack": false,
    })
}

fn error_kind(err: &BridgeError) -> &'static str {
    match err {
        BridgeError::BadParams(_) => "bad_params",
        BridgeError::UnknownMethod(_) => "unknown_method",
        BridgeError::Unsupported(_) => "unsupported",
        BridgeError::Emulator(_) => "emulator_error",
        BridgeError::Io(_) | BridgeError::Json(_) | BridgeError::Ws(_) => "bridge_error",
    }
}

/// Mark a `poll_events` stop event as a hit on breakpoint `id`, matching the NDS bridge's
/// `breakpoint_hit` shape.
fn mark_breakpoint_hit(event: &mut Value, id: u64, kind: &str, address: u64) {
    if let Some(obj) = event.as_object_mut() {
        obj.insert("type".into(), json!("breakpoint_hit"));
        obj.insert("kind".into(), json!(kind));
        obj.insert("address".into(), json!(address));
        obj.insert("id".into(), json!(id));
        obj.insert("breakpoint_id".into(), json!(id));
    }
}

/// Compile `set_breakpoint`'s structured `pc_min`/`pc_max` filters plus an optional raw `condition`
/// string into a single PPSSPP `condition` expression (`Core/Debugger/WebSocket/BreakpointSubscriber.cpp`
/// parses/evaluates it via `initExpression`/`parseExpression`; a bad expression comes back as an
/// `emulator_error`, not a silent no-op). Returns `None` when no filter was given (PPSSPP breaks
/// unconditionally then).
fn breakpoint_condition(params: &Value) -> BridgeResult<Option<String>> {
    let mut clauses = Vec::new();
    if let Some(raw) = params.get("condition").and_then(Value::as_str) {
        if !raw.trim().is_empty() {
            clauses.push(format!("({raw})"));
        }
    }
    let pc_min = optional_num(params, "pc_min")?;
    let pc_max = optional_num(params, "pc_max")?;
    if let (Some(min), Some(max)) = (pc_min, pc_max) {
        if min > max {
            return Err(BridgeError::BadParams("pc_min must be <= pc_max".into()));
        }
    }
    if let Some(min) = pc_min {
        clauses.push(format!("(pc >= 0x{min:x})"));
    }
    if let Some(max) = pc_max {
        clauses.push(format!("(pc <= 0x{max:x})"));
    }
    if clauses.is_empty() {
        Ok(None)
    } else {
        Ok(Some(clauses.join(" && ")))
    }
}

/// `step_instructions`' instruction count — accepts `count`, `n`, or `frames` (the MCP's
/// step-by-instructions tool sends `frames`; see the NDS bridge's identical `step_count`), defaulting
/// to 1 and never 0 (a 0-count step would be a silent no-op).
fn step_count(params: &Value) -> BridgeResult<u64> {
    let count = match optional_num(params, "count")? {
        Some(count) => count,
        None => match optional_num(params, "n")? {
            Some(n) => n,
            None => optional_num(params, "frames")?.unwrap_or(1),
        },
    };
    Ok(count.max(1))
}

/// Resolve a `read_memory`/`write_memory` request's absolute PSP address from `memory_type`
/// (default `main`, the only region today) + `address`/`start` offset, bounding `[offset, offset+len)`
/// to the region. `len` is the access length (read `length`, write byte count).
fn route_main_address(params: &Value, len: u64) -> BridgeResult<u64> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("main");
    if !MEMORY_TYPES.contains(&memory_type) {
        return Err(BridgeError::BadParams(format!(
            "unsupported memory_type: {memory_type}; valid: {}",
            MEMORY_TYPES.join(", ")
        )));
    }
    let offset = region_offset(params)?;
    // `[offset, offset+len)` must stay within `main` (user RAM). An offset past the region would be
    // forwarded to PPSSPP as an aliased/other region, so a `main` write could corrupt non-`main`
    // memory while the bridge reports success. Reject it (checked add, no wrap); read and write both
    // route here.
    if !matches!(offset.checked_add(len.max(1)), Some(end) if end <= PSP_MAIN_RAM_SIZE) {
        return Err(BridgeError::BadParams(format!(
            "{memory_type} access out of range: offset {offset:#x}+{len:#x} exceeds region size {PSP_MAIN_RAM_SIZE:#x}"
        )));
    }
    PSP_MAIN_RAM_BASE.checked_add(offset).ok_or_else(|| {
        BridgeError::BadParams(format!(
            "{memory_type} address overflow at offset {offset:#x}"
        ))
    })
}

fn region_offset(params: &Value) -> BridgeResult<u64> {
    if let Some(value) = optional_num(params, "address")? {
        return Ok(value);
    }
    if let Some(value) = optional_num(params, "start")? {
        return Ok(value);
    }
    Err(BridgeError::BadParams(
        "missing required param: address".into(),
    ))
}

/// Absolute PSP address for `disassemble` — a raw address (e.g. `cpu.pc` from `get_state`), no
/// `memory_type` base added (unlike `read_memory`/`write_memory`).
fn absolute_address(params: &Value) -> BridgeResult<u64> {
    if let Some(value) = optional_num(params, "address")? {
        return Ok(value);
    }
    if let Some(value) = optional_num(params, "start")? {
        return Ok(value);
    }
    Err(BridgeError::BadParams(
        "missing required param: address".into(),
    ))
}

/// Resolve a `set_breakpoint` address by kind. An exec breakpoint's `address`/`start` is a raw
/// absolute PSP address — a PC straight from `get_state`'s `cpu.pc` or `disassemble` — so
/// `memory_type` is ignored (a PC is not a `main`-region offset and is not always inside `main` RAM;
/// PPSSPP's cpu breakpoint takes an absolute address either way). A read/write watchpoint's
/// `address`/`start` is symmetric with `read_memory`/`write_memory`: a `memory_type` region offset
/// routed through the same `route_main_address` those two use (→ `PSP_MAIN_RAM_BASE + offset`, with
/// the identical out-of-range rejection), so it lands where `read_memory` reads. `len` is the watched
/// span (the memory breakpoint's `length`) and bounds `[offset, offset+len)` to the region.
fn route_breakpoint_address(kind: &str, params: &Value, len: u64) -> BridgeResult<u64> {
    if kind == "exec" {
        absolute_address(params)
    } else {
        route_main_address(params, len)
    }
}

fn required_num(params: &Value, key: &str) -> BridgeResult<u64> {
    let value = params
        .get(key)
        .ok_or_else(|| BridgeError::BadParams(format!("missing required param: {key}")))?;
    parse_num(value).ok_or_else(|| BridgeError::BadParams(format!("invalid numeric param: {key}")))
}

fn optional_num(params: &Value, key: &str) -> BridgeResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| BridgeError::BadParams(format!("invalid numeric param: {key}"))),
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

fn required_str<'a>(params: &'a Value, key: &str) -> BridgeResult<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::BadParams(format!("missing required param: {key}")))
}

/// sha1 of a content image's bytes, for `get_rom_info` (matches the NDS/PC-98 bridges' own
/// `sha1_file`).
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

fn absolute_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeWs {
        replies: VecDeque<(String, Value)>,
        pending_events: VecDeque<Value>,
        /// Every `(event, params)` pair seen, in order — lets a test assert the bridge computed
        /// the right PPSSPP request (address/size/base64/...), not just the right event name.
        calls: Vec<(String, Value)>,
        /// Read budget each `call_with_timeout` was invoked with, keyed by event — lets a test
        /// assert the bridge threads the extended savestate budget rather than the default.
        call_timeouts: Vec<(String, Duration)>,
        /// Models PPSSPP replies that arrive slowly: event name → the minimum read budget under
        /// which the reply arrives in time. A `call_with_timeout` with a shorter budget times out
        /// (a `bridge_error`) when the read budget is too short — reproducing the desync.
        slow_replies: HashMap<String, Duration>,
        /// Event names whose `call_ticketed` should time out (a `WouldBlock` read) without consuming
        /// a reply — models a breakpoint halting the CPU mid-press so the timed release ack never
        /// fires, reproducing the desync/stuck-button.
        timeout_events: std::collections::HashSet<String>,
    }

    impl FakeWs {
        fn with(replies: &[(&str, Value)]) -> Self {
            Self {
                replies: replies
                    .iter()
                    .map(|(event, reply)| (event.to_string(), reply.clone()))
                    .collect(),
                ..Default::default()
            }
        }

        /// Queue a spontaneous event as if it arrived on the wire ahead of any request — models
        /// PPSSPP's unprompted `cpu.stepping`/`cpu.resume`/log/input notifications for `poll_events`.
        fn push_event(&mut self, event: Value) {
            self.pending_events.push_back(event);
        }
    }

    impl WsTransport for FakeWs {
        fn call(&mut self, event: &str, params: Value) -> Result<Value, BridgeError> {
            self.calls.push((event.to_string(), params));
            let Some((expected, reply)) = self.replies.pop_front() else {
                return Err(BridgeError::Emulator(format!(
                    "unexpected fake WS call: {event}"
                )));
            };
            assert_eq!(event, expected);
            Ok(reply)
        }

        fn call_and_wait_for(
            &mut self,
            event: &str,
            params: Value,
            expect_event: &str,
        ) -> Result<Value, BridgeError> {
            self.calls.push((event.to_string(), params));
            let Some((expected, reply)) = self.replies.pop_front() else {
                return Err(BridgeError::Emulator(format!(
                    "unexpected fake WS call: {event} (awaiting {expect_event})"
                )));
            };
            assert_eq!(expect_event, expected);
            Ok(reply)
        }

        fn call_with_timeout(
            &mut self,
            event: &str,
            params: Value,
            timeout: Duration,
        ) -> Result<Value, BridgeError> {
            self.call_timeouts.push((event.to_string(), timeout));
            // A slow reply only arrives if the read budget outlasts its required wait; too small a
            // budget times out like a socket read (a `bridge_error`), without consuming the
            // reply — reproducing the desync.
            if let Some(required) = self.slow_replies.get(event).copied() {
                if timeout < required {
                    return Err(BridgeError::Io(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "fake ws read timed out before the slow reply arrived",
                    )));
                }
            }
            self.call(event, params)
        }

        fn call_ticketed(
            &mut self,
            event: &str,
            params: Value,
            ticket: &str,
        ) -> Result<Value, BridgeError> {
            let mut obj = match params {
                Value::Object(map) => map,
                Value::Null => serde_json::Map::new(),
                other => panic!("ticketed params must be an object, got {other}"),
            };
            obj.insert("ticket".into(), json!(ticket));
            self.calls.push((event.to_string(), Value::Object(obj)));
            if self.timeout_events.contains(event) {
                // No reply consumed — exactly like a real socket read that times out with the
                // press ack still stranded on PPSSPP's side.
                return Err(BridgeError::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "fake ws ticketed read timed out (press interrupted)",
                )));
            }
            // Model `read_until_ticketed`: skip (queue) any reply whose event or echoed ticket does
            // not match, so a stale off-ticket ack can never satisfy this call.
            loop {
                let Some((expected, reply)) = self.replies.pop_front() else {
                    return Err(BridgeError::Emulator(format!(
                        "unexpected fake WS ticketed call: {event}"
                    )));
                };
                let reply_ticket = reply.get("ticket").and_then(Value::as_str);
                if expected == event && reply_ticket == Some(ticket) {
                    return Ok(reply);
                }
                self.pending_events.push_back(reply);
            }
        }

        fn drain_events(&mut self) -> Vec<Value> {
            self.pending_events.drain(..).collect()
        }
    }

    #[test]
    fn status_reports_connected_and_version() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("version", json!({"event":"version","version":"v1.17"})),
            (
                "game.status",
                json!({"event":"game.status","game":{"id":"ULJS00001","title":"Tales"},"paused":false}),
            ),
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(1, "status", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["system"], "psp");
    }

    #[test]
    fn status_reports_stepping_cpu_as_frozen_even_when_game_status_paused_is_false() {
        // game.status.paused is GetUIState()==UISTATE_PAUSEMENU (the GUI pause menu) — it stays
        // false even while the CPU is halted at a breakpoint. `state` must come from
        // cpu.status.stepping, not game.status.paused.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("version", json!({"event":"version","version":"v1.17"})),
            (
                "game.status",
                json!({"event":"game.status","game":{"id":"ULJS00097","title":"Tales of Destiny 2"},"paused":false}),
            ),
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0x08804128u32,"ticks":0}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(1, "status", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["state"], "frozen");
        assert_eq!(result["ppsspp_version"], "v1.17");
        assert_eq!(result["memory_types"][0], "main");
    }

    #[test]
    fn unknown_method_reports_unknown_method_kind() {
        // A genuinely unknown wire method (typo/garbage) must be unknown_method, distinct from the
        // `unsupported` kind reserved for real-but-ungapped emucap tool names.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, "florble", json!({})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "unknown_method");
    }

    #[test]
    fn step_frame_request_is_unsupported_not_reinterpreted_as_instructions() {
        // The MCP frame-step tool sends wire `step` with `{frames:n}` and no `unit`. PPSSPP has no
        // frame-advance, so this must be rejected — not silently stepped as n instructions.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, "step", json!({"frames": 60})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "unsupported");
    }

    #[test]
    fn step_wire_method_with_instructions_unit_dispatches_to_stepping() {
        // The MCP instruction-step tool (`step_instructions`) reaches the bridge as wire method
        // `step` with `{frames:n, unit:"instructions"}` (same as the NDS bridge) — it must route to
        // the cpu.stepInto path, not error as an unknown/ungapped method.
        let regs = json!({
            "event": "cpu.getAllRegs",
            "categories": [{
                "id": 0, "name": "GPR",
                "registerNames": ["pc"],
                "uintValues": [0x0880_4004u32],
                "floatValues": ["0.0"],
            }],
        });
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "cpu.stepping",
                json!({"event":"cpu.stepping","pc":0x0880_4004u32,"ticks":0}),
            ),
            ("cpu.getAllRegs", regs),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "step",
            json!({"frames": 1, "unit": "instructions"}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["unit"], "instructions");
        assert_eq!(result["count"], 1);
        assert_eq!(result["pc"], 0x0880_4004u32);
        // Straight into a single cpu.stepInto (already stepping, so no pre-pause).
        assert_eq!(bridge.ws.calls[1].0, "cpu.stepInto");
    }

    #[test]
    fn hello_advertises_psp_surface_and_truthful_methods() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, "hello", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["system"], "psp");
        assert_eq!(result["adapter"], "ppsspp-rust-ws");
        assert_eq!(result["backend"], "ppsspp-debugger-ws");
        assert_eq!(result["debugger"], true);
        assert_eq!(result["memory_types"], json!(["main"]));

        let methods = result["methods"].as_array().unwrap();
        for wanted in [
            "hello",
            "status",
            "get_rom_info",
            "read_memory",
            "write_memory",
            "dump_memory",
            "get_state",
            "disassemble",
            "set_breakpoint",
            "clear_breakpoint",
            "list_breakpoints",
            "clear_all_breakpoints",
            "step_instructions",
            "pause",
            "resume",
            "poll_events",
            "screenshot",
            "set_input",
            "press_buttons",
            "save_state",
            "load_state",
            "reset",
        ] {
            assert!(methods.iter().any(|m| m == wanted), "missing {wanted}");
        }
        // Frame-based `step` must not be advertised as callable (PPSSPP has no frame advance), so
        // the MCP's `has("step")` frame-step composites stay off on PSP.
        assert!(
            !methods.iter().any(|m| m == "step"),
            "should not advertise frame-based step"
        );

        let caps = &result["capability_notes"];
        assert_eq!(caps["disassemble"], true);
        assert_eq!(caps["breakpoints"], true);
        // Stepping IS available, at instruction granularity only — this is the disclosure of
        // the step capability, so `step` is *not* listed as a "planned"/not-yet-callable method.
        assert_eq!(caps["step_units"], json!(["instructions"]));
        assert_eq!(caps["screenshot"], true);
        assert_eq!(caps["input"], true);
        assert_eq!(caps["state_restore"], true);

        // capability_notes.planned_methods discloses real emucap tool names not dispatched today
        // (all platform-gapped → an "unsupported"). Frame `step` is NOT here: it is a
        // permanent gap conveyed by step_units, not a pending feature — advertising it as planned
        // while wire `step {unit:instructions}` is dispatched-and-working would misrepresent it.
        let planned = caps["planned_methods"].as_array().unwrap();
        assert!(
            !planned.iter().any(|m| m == "step"),
            "frame `step` must not be advertised as a planned/not-yet-callable method"
        );
        for undispatched in [
            "run_frames",
            "probe",
            "find_pattern",
            "watch_register",
            "set_trace",
            "get_trace",
            "break_on_reset",
        ] {
            assert!(
                planned.iter().any(|m| m == undispatched),
                "planned_methods missing {undispatched}"
            );
        }
        // dump_memory is implemented now — it must be a callable method, not advertised as planned.
        assert!(
            methods.iter().any(|m| m == "dump_memory"),
            "dump_memory must be advertised as a callable method"
        );
        assert!(
            !planned.iter().any(|m| m == "dump_memory"),
            "dump_memory must not be advertised as planned/unsupported once implemented"
        );
    }

    #[test]
    fn hello_echoes_session_token_and_name_when_launcher_set_them() {
        // The launcher (`src/launch/ppsspp.rs`) sets EMUCAP_NAME/EMUCAP_SESSION_TOKEN on the
        // bridge process; emucap-mcp's TCP handshake (`live/tcp.rs`) sends a "hello" and rejects
        // the connection with IdentityMismatch unless the reply echoes back the same
        // session_token. `with_identity` supplies both without mutating process env.
        let mut bridge = PpssppBridge::with_identity(
            FakeWs::with(&[]),
            None,
            Some("psp_session".to_string()),
            Some("tok-abc123".to_string()),
        );
        let resp = bridge.handle_request(Request::new(1, "hello", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["name"], "psp_session");
        assert_eq!(result["session_token"], "tok-abc123");
    }

    #[test]
    fn hello_omits_name_and_session_token_when_unset() {
        let mut bridge = PpssppBridge::with_content(FakeWs::with(&[]), None);
        let resp = bridge.handle_request(Request::new(1, "hello", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert!(result.get("name").is_none());
        assert!(result.get("session_token").is_none());
    }

    #[test]
    fn unsupported_whole_methods_return_unsupported_not_unknown_method() {
        // Real emucap tool names with no PPSSPP WS/fork primitive behind them yet must report the
        // "unsupported" kind, not "unknown_method" (reserved for genuine typos).
        for name in [
            "run_frames",
            "probe",
            "find_pattern",
            "watch_register",
            "set_trace",
            "get_trace",
            "break_on_reset",
        ] {
            let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
            let resp = bridge.handle_request(Request::new(1, name, json!({})));
            assert!(!resp.ok, "{name} unexpectedly succeeded");
            assert_eq!(
                resp.error.unwrap().kind,
                "unsupported",
                "{name} should be unsupported, not unknown_method"
            );
            assert!(bridge.ws.calls.is_empty(), "{name} must not call PPSSPP");
        }
    }

    #[test]
    fn read_memory_maps_main_offset_to_absolute_address_and_decodes_hex() {
        let payload = [0xde_u8, 0xad, 0xbe, 0xef];
        let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "memory.read",
            json!({"event":"memory.read","base64": b64}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "read_memory",
            json!({"memory_type": "main", "address": 0x4000, "length": 4}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["hex"], "deadbeef");

        let (event, params) = &bridge.ws.calls[0];
        assert_eq!(event, "memory.read");
        assert_eq!(params["address"], 0x0880_4000u64);
        assert_eq!(params["size"], 4);
    }

    #[test]
    fn read_memory_defaults_memory_type_to_main() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0xAAu8]);
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "memory.read",
            json!({"event":"memory.read","base64": b64}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "read_memory",
            json!({"address": 0, "length": 1}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(bridge.ws.calls[0].1["address"], 0x0880_0000u64);
    }

    #[test]
    fn read_memory_rejects_unsupported_memory_type() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "read_memory",
            json!({"memory_type": "vram", "address": 0, "length": 4}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(
            bridge.ws.calls.is_empty(),
            "must not call PPSSPP for a rejected memory_type"
        );
    }

    #[test]
    fn read_memory_rejects_length_over_cap() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "read_memory",
            json!({"memory_type": "main", "address": 0, "length": 0x30_0000}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(
            bridge.ws.calls.is_empty(),
            "must reject before calling PPSSPP"
        );
    }

    #[test]
    fn write_memory_encodes_hex_to_base64_at_absolute_address() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "memory.write",
            json!({"event":"memory.write"}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "write_memory",
            json!({"memory_type": "main", "address": 0x100, "hex": "aabbccdd"}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["written"], 4);

        let (event, params) = &bridge.ws.calls[0];
        assert_eq!(event, "memory.write");
        assert_eq!(params["address"], 0x0880_0100u64);
        let expected_b64 =
            base64::engine::general_purpose::STANDARD.encode([0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(params["base64"], expected_b64);
    }

    #[test]
    fn write_memory_rejects_odd_length_hex() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "write_memory",
            json!({"memory_type": "main", "address": 0, "hex": "abc"}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn write_memory_rejects_invalid_hex() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "write_memory",
            json!({"memory_type": "main", "address": 0, "hex": "zzzz"}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn read_memory_rejects_offset_past_user_ram_end() {
        // An offset at/past the `main` (user RAM) extent would be forwarded to PPSSPP as an aliased
        // region, so a read there does not read `main`. Reject it before touching the wire.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "read_memory",
            json!({"memory_type": "main", "address": PSP_MAIN_RAM_SIZE, "length": 4}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(
            bridge.ws.calls.is_empty(),
            "must reject an out-of-range read before calling PPSSPP"
        );
    }

    #[test]
    fn write_memory_rejects_offset_len_straddling_user_ram_end() {
        // The last two bytes fit but the write's 4 bytes run two bytes past the region end — a write
        // that spills out of `main` could corrupt non-`main` memory while reporting success. Reject.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "write_memory",
            json!({"memory_type": "main", "address": PSP_MAIN_RAM_SIZE - 2, "hex": "aabbccdd"}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(
            bridge.ws.calls.is_empty(),
            "must reject a straddling write before calling PPSSPP"
        );
    }

    #[test]
    fn read_memory_at_last_in_range_bytes_is_allowed() {
        // [offset, offset+len) ending exactly at the region end is in range — the last valid access.
        let payload = [0x11_u8, 0x22, 0x33, 0x44];
        let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "memory.read",
            json!({"event":"memory.read","base64": b64}),
        )]));
        let offset = PSP_MAIN_RAM_SIZE - 4;
        let resp = bridge.handle_request(Request::new(
            1,
            "read_memory",
            json!({"memory_type": "main", "address": offset, "length": 4}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["hex"], "11223344");
        assert_eq!(bridge.ws.calls[0].1["address"], PSP_MAIN_RAM_BASE + offset);
    }

    #[test]
    fn get_state_flattens_gpr_category_into_cpu_prefixed_map() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.getAllRegs",
            json!({
                "event": "cpu.getAllRegs",
                "categories": [
                    {
                        "id": 0,
                        "name": "GPR",
                        "registerNames": ["zero", "sp", "ra", "pc"],
                        "uintValues": [0, 0x08900000u32, 0x08900010u32, 0x08900020u32],
                        "floatValues": ["0.000000", "0.000000", "0.000000", "0.000000"],
                    },
                    {
                        "id": 1,
                        "name": "FPU",
                        "registerNames": ["f0"],
                        "uintValues": [999],
                        "floatValues": ["nan"],
                    },
                ],
            }),
        )]));
        let resp = bridge.handle_request(Request::new(1, "get_state", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let state = &resp.result.unwrap()["state"];
        assert_eq!(state["cpu.zero"], 0);
        assert_eq!(state["cpu.sp"], 0x08900000u32);
        assert_eq!(state["cpu.ra"], 0x08900010u32);
        assert_eq!(state["cpu.pc"], 0x08900020u32);
        // FPU/VFPU categories are out of scope for v1 — must not leak in under any prefix.
        assert!(state.get("cpu.f0").is_none());
    }

    #[test]
    fn get_state_errors_when_gpr_category_missing() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.getAllRegs",
            json!({"event": "cpu.getAllRegs", "categories": []}),
        )]));
        let resp = bridge.handle_request(Request::new(1, "get_state", json!({})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "emulator_error");
    }

    #[test]
    fn disassemble_maps_lines_to_addr_bytes_text() {
        let syscall: u32 = 0x0000_000c;
        let jr_ra: u32 = 0x03e0_0008;
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "memory.disasm",
            json!({
                "event": "memory.disasm",
                "range": {"start": 0x0880_4000u32, "end": 0x0880_4008u32},
                "lines": [
                    {"address": 0x0880_4000u32, "encoding": syscall, "name": "syscall", "params": ""},
                    {"address": 0x0880_4004u32, "encoding": jr_ra, "name": "jr", "params": "ra"},
                ],
                "branchGuides": [],
            }),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "disassemble",
            json!({"address": 0x0880_4000u32, "count": 2}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        let insns = result["instructions"].as_array().unwrap();
        assert_eq!(insns.len(), 2);
        assert_eq!(insns[0]["addr"], 0x0880_4000u64);
        assert_eq!(insns[0]["text"], "syscall");
        assert_eq!(insns[0]["bytes"], hex::encode(syscall.to_le_bytes()));
        assert_eq!(insns[1]["addr"], 0x0880_4004u64);
        assert_eq!(insns[1]["text"], "jr ra");
        assert_eq!(insns[1]["bytes"], hex::encode(jr_ra.to_le_bytes()));

        let (event, params) = &bridge.ws.calls[0];
        assert_eq!(event, "memory.disasm");
        assert_eq!(params["address"], 0x0880_4000u64);
        assert_eq!(params["count"], 2);
    }

    #[test]
    fn disassemble_requires_address() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, "disassemble", json!({"count": 2})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }

    // --- set_breakpoint / clear_breakpoint / list_breakpoints / clear_all_breakpoints ---

    #[test]
    fn set_breakpoint_exec_calls_cpu_breakpoint_add_and_tracks_id() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.breakpoint.add",
            json!({"event":"cpu.breakpoint.add"}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x0880_4128u32}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["id"], 1);
        assert_eq!(result["kind"], "exec");
        assert_eq!(result["address"], 0x0880_4128u32);

        let (event, params) = &bridge.ws.calls[0];
        assert_eq!(event, "cpu.breakpoint.add");
        assert_eq!(params["address"], 0x0880_4128u32);
        assert_eq!(params["enabled"], true);
        assert!(params.get("condition").is_none());
    }

    #[test]
    fn set_breakpoint_read_calls_memory_breakpoint_add_with_size_and_read_flag() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "memory.breakpoint.add",
                json!({"event":"memory.breakpoint.add"}),
            ),
            // The add reads back the live hit counter to seed last_hits (fresh memcheck → 0).
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
            ),
        ]));
        // A read/write watchpoint takes a memory_type offset (symmetric with read_memory), so offset
        // 0x100 in `main` resolves to PSP_MAIN_RAM_BASE + 0x100 = 0x0880_0100.
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "read", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["kind"], "read");
        assert_eq!(result["length"], 4);

        let (event, params) = &bridge.ws.calls[0];
        assert_eq!(event, "memory.breakpoint.add");
        assert_eq!(params["address"], 0x0880_0100u32);
        assert_eq!(params["size"], 4);
        assert_eq!(params["read"], true);
        assert_eq!(params["write"], false);
    }

    #[test]
    fn set_breakpoint_write_defaults_length_to_one() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "memory.breakpoint.add",
                json!({"event":"memory.breakpoint.add"}),
            ),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0200u32, "size": 1, "hits": 0}]}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": 0x200}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let (_, params) = &bridge.ws.calls[0];
        assert_eq!(params["address"], 0x0880_0200u32);
        assert_eq!(params["size"], 1);
        assert_eq!(params["write"], true);
        assert_eq!(params["read"], false);
    }

    #[test]
    fn set_breakpoint_memory_type_main_routes_offset_and_rejects_out_of_range() {
        // A read/write watchpoint's memory_type:"main" + offset resolves exactly like read_memory
        // (PSP_MAIN_RAM_BASE + offset) instead of a raw low address that never fires, and an offset
        // that leaves the region is rejected before any WS call — symmetric with read/write_memory.
        // (Exec breakpoints take an absolute PC and do NOT route; see
        // set_breakpoint_exec_ignores_memory_type_and_arms_at_raw_pc.)
        let offset = 0x4000u64;
        let expected = PSP_MAIN_RAM_BASE + offset;
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "memory.breakpoint.add",
                json!({"event": "memory.breakpoint.add"}),
            ),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": expected, "size": 1, "hits": 0}]}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": offset}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["address"], expected);
        assert_eq!(bridge.ws.calls[0].0, "memory.breakpoint.add");
        assert_eq!(bridge.ws.calls[0].1["address"], expected);

        // An out-of-range main offset is rejected, not silently armed at a bad address.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            2,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": PSP_MAIN_RAM_SIZE}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(
            bridge.ws.calls.is_empty(),
            "must not arm a breakpoint for an out-of-range main offset"
        );
    }

    #[test]
    fn set_breakpoint_exec_ignores_memory_type_and_arms_at_raw_pc() {
        // Regression (0.5.0 exec-BP contract): the MCP wrapper ALWAYS sends `memory_type` (a required
        // field) and, for a single-address call, end==start. An exec breakpoint's address is a raw PC
        // (absolute, e.g. `cpu.pc` from get_state) — it must NOT be offset-routed like a read/write
        // watchpoint, or a README-documented cpu.pc anchor (0x0880_4128) would be mis-read as a `main`
        // offset (0x0880_4128 > region size) and rejected as out of range. Arm at the raw address;
        // memory_type is ignored.
        let pc = 0x0880_4128u64;
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.breakpoint.add",
            json!({"event": "cpu.breakpoint.add"}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "exec", "memory_type": "main", "start": pc, "end": pc}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["address"], pc);
        assert_eq!(bridge.ws.calls[0].0, "cpu.breakpoint.add");
        assert_eq!(bridge.ws.calls[0].1["address"], pc);

        // A genuine range exec point (end != start) is still rejected — PPSSPP's cpu breakpoint is a
        // single address.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            2,
            "set_breakpoint",
            json!({"kind": "exec", "memory_type": "main", "start": pc, "end": pc + 0x10}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "unsupported");
        assert!(bridge.ws.calls.is_empty());
    }

    #[test]
    fn set_breakpoint_rejects_unsupported_kind() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "access", "address": 0}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(bridge.ws.calls.is_empty());
    }

    #[test]
    fn set_breakpoint_rejects_range_exec_breakpoint() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x100, "end": 0x110}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "unsupported");
        assert!(bridge.ws.calls.is_empty());
    }

    #[test]
    fn set_breakpoint_translates_pc_min_max_into_condition() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.breakpoint.add",
            json!({"event":"cpu.breakpoint.add"}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x100, "pc_min": 0x10, "pc_max": 0x200}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let (_, params) = &bridge.ws.calls[0];
        assert_eq!(params["condition"], "(pc >= 0x10) && (pc <= 0x200)");
    }

    #[test]
    fn set_breakpoint_pause_on_hit_false_maps_to_enabled_false() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.breakpoint.add",
            json!({"event":"cpu.breakpoint.add"}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x100, "pause_on_hit": false}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(bridge.ws.calls[0].1["enabled"], false);
    }

    #[test]
    fn set_breakpoint_rejects_auto_savestate_and_snapshot() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x100, "auto_savestate": true}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "unsupported");

        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x100, "snapshot": ["main:0:4"]}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "unsupported");
    }

    #[test]
    fn clear_breakpoint_exec_calls_cpu_breakpoint_remove() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            (
                "cpu.breakpoint.remove",
                json!({"event":"cpu.breakpoint.remove"}),
            ),
        ]));
        bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x100}),
        ));
        let resp = bridge.handle_request(Request::new(2, "clear_breakpoint", json!({"id": 1})));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["cleared"], 1);
        let (event, params) = &bridge.ws.calls[1];
        assert_eq!(event, "cpu.breakpoint.remove");
        assert_eq!(params["address"], 0x100);
    }

    #[test]
    fn clearing_one_duplicate_exec_breakpoint_keeps_the_survivor_armed() {
        // Two exec breakpoints at the SAME address (a duplicate set_breakpoint, or a retry after a
        // lost response) map to ONE PPSSPP cpu breakpoint. Clearing one bridge id must NOT send
        // cpu.breakpoint.remove while the other id still lives — else the survivor would stay in
        // list_breakpoints but never halt again. Only the LAST duplicate on the address tears it down.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            // Exactly ONE remove — emitted only when the last duplicate is cleared.
            (
                "cpu.breakpoint.remove",
                json!({"event":"cpu.breakpoint.remove"}),
            ),
        ]));
        let a =
            bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x0880_4128u32})));
        let b =
            bridge.handle_request(Request::new(2, "set_breakpoint", json!({"address": 0x0880_4128u32})));
        assert_eq!(a.result.unwrap()["id"], 1);
        assert_eq!(b.result.unwrap()["id"], 2);
        // Clear the first duplicate: survivor bp2 remains, so NO cpu.breakpoint.remove yet.
        let c1 = bridge.handle_request(Request::new(3, "clear_breakpoint", json!({"id": 1})));
        assert!(c1.ok, "{:?}", c1.error);
        assert_eq!(
            bridge.ws.calls.len(),
            2,
            "clearing one duplicate must not disarm the shared PPSSPP breakpoint"
        );
        // The survivor is still armed and listed.
        let list = bridge.handle_request(Request::new(4, "list_breakpoints", json!({})));
        assert_eq!(
            list.result.unwrap()["breakpoints"],
            json!([{"id": 2, "kind": "exec", "address": 0x0880_4128u32}])
        );
        // Clearing the last duplicate finally removes the PPSSPP breakpoint.
        let c2 = bridge.handle_request(Request::new(5, "clear_breakpoint", json!({"id": 2})));
        assert!(c2.ok, "{:?}", c2.error);
        let (event, params) = &bridge.ws.calls[2];
        assert_eq!(event, "cpu.breakpoint.remove");
        assert_eq!(params["address"], 0x0880_4128u32);
    }

    #[test]
    fn clear_breakpoint_memory_calls_memory_breakpoint_remove_with_size() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "memory.breakpoint.add",
                json!({"event":"memory.breakpoint.add"}),
            ),
            // add reads back the live counter to seed last_hits (calls[1]). A write watchpoint routes
            // its memory_type offset 0x200 to PSP_MAIN_RAM_BASE + 0x200 = 0x0880_0200.
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0200u32, "size": 8, "hits": 0}]}),
            ),
            (
                "memory.breakpoint.remove",
                json!({"event":"memory.breakpoint.remove"}),
            ),
        ]));
        bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": 0x200, "length": 8}),
        ));
        let resp = bridge.handle_request(Request::new(2, "clear_breakpoint", json!({"id": 1})));
        assert!(resp.ok, "{:?}", resp.error);
        // calls: [0]=add, [1]=list (seed), [2]=remove.
        let (event, params) = &bridge.ws.calls[2];
        assert_eq!(event, "memory.breakpoint.remove");
        assert_eq!(params["address"], 0x0880_0200u32);
        assert_eq!(params["size"], 8);
    }

    #[test]
    fn clear_breakpoint_unknown_id_is_bad_params() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, "clear_breakpoint", json!({"id": 99})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn list_breakpoints_returns_tracked_entries() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            (
                "memory.breakpoint.add",
                json!({"event":"memory.breakpoint.add"}),
            ),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0200u32, "size": 2, "hits": 0}]}),
            ),
        ]));
        // exec BP takes a raw absolute PC (0x100); the read watchpoint's memory_type offset 0x200
        // routes to PSP_MAIN_RAM_BASE + 0x200 = 0x0880_0200.
        bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x100}),
        ));
        bridge.handle_request(Request::new(
            2,
            "set_breakpoint",
            json!({"kind": "read", "memory_type": "main", "start": 0x200, "length": 2}),
        ));
        let resp = bridge.handle_request(Request::new(3, "list_breakpoints", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let rows = resp.result.unwrap()["breakpoints"].clone();
        assert_eq!(
            rows,
            json!([
                {"id": 1, "kind": "exec", "address": 0x100},
                {"id": 2, "kind": "read", "address": 0x0880_0200u32, "length": 2},
            ])
        );
    }

    #[test]
    fn clear_all_breakpoints_clears_every_tracked_id() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            (
                "cpu.breakpoint.remove",
                json!({"event":"cpu.breakpoint.remove"}),
            ),
            (
                "cpu.breakpoint.remove",
                json!({"event":"cpu.breakpoint.remove"}),
            ),
        ]));
        bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x100})));
        bridge.handle_request(Request::new(2, "set_breakpoint", json!({"address": 0x200})));
        let resp = bridge.handle_request(Request::new(3, "clear_all_breakpoints", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["cleared"], json!([1, 2]));
        let list = bridge.handle_request(Request::new(4, "list_breakpoints", json!({})));
        assert_eq!(list.result.unwrap()["breakpoints"], json!([]));
    }

    // --- pause / resume ---

    #[test]
    fn pause_sends_cpu_stepping_when_running() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "cpu.stepping",
                json!({"event":"cpu.stepping","pc":0x100,"ticks":0}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(1, "pause", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["state"], "frozen");
        assert_eq!(bridge.ws.calls[1].0, "cpu.stepping");
    }

    #[test]
    fn pause_is_a_noop_when_already_stepping() {
        // PPSSPP's WebSocketCPUStepping silently does nothing when already stepping (no state
        // change, so no ack ever arrives) — calling it here would hang the bridge. The FakeWs has
        // no "cpu.stepping" reply queued, so this test would fail loudly if the guard were missing.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.status",
            json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
        )]));
        let resp = bridge.handle_request(Request::new(1, "pause", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["state"], "frozen");
        assert_eq!(bridge.ws.calls.len(), 1, "must not call cpu.stepping again");
    }

    #[test]
    fn resume_sends_cpu_resume_when_stepping() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
            ),
            ("cpu.resume", json!({"event":"cpu.resume"})),
        ]));
        let resp = bridge.handle_request(Request::new(1, "resume", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["state"], "running");
        assert_eq!(bridge.ws.calls[1].0, "cpu.resume");
    }

    #[test]
    fn resume_is_a_noop_when_already_running() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        )]));
        let resp = bridge.handle_request(Request::new(1, "resume", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["state"], "running");
        assert_eq!(bridge.ws.calls.len(), 1, "must not call cpu.resume");
    }

    // --- step_instructions ---

    #[test]
    fn step_instructions_pauses_first_when_running_then_steps_and_reports_state() {
        let regs = json!({
            "event": "cpu.getAllRegs",
            "categories": [{
                "id": 0, "name": "GPR",
                "registerNames": ["zero", "pc"],
                "uintValues": [0, 0x0880_4004u32],
                "floatValues": ["0.0", "0.0"],
            }],
        });
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "cpu.stepping",
                json!({"event":"cpu.stepping","pc":0x0880_4000u32,"ticks":0}),
            ),
            (
                "cpu.stepping",
                json!({"event":"cpu.stepping","pc":0x0880_4004u32,"ticks":0}),
            ),
            ("cpu.getAllRegs", regs),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "step_instructions",
            json!({"count": 1}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["unit"], "instructions");
        assert_eq!(result["count"], 1);
        assert_eq!(result["pc"], 0x0880_4004u32);
        assert_eq!(result["state"]["cpu.pc"], 0x0880_4004u32);

        // First call must be the pre-pause (cpu.status found it running, so a cpu.stepping pause
        // request goes out) followed by exactly one cpu.stepInto for count=1.
        assert_eq!(bridge.ws.calls[1].0, "cpu.stepping");
        assert_eq!(bridge.ws.calls[2].0, "cpu.stepInto");
    }

    #[test]
    fn step_instructions_skips_pre_pause_when_already_stepping() {
        let regs = json!({
            "event": "cpu.getAllRegs",
            "categories": [{
                "id": 0, "name": "GPR",
                "registerNames": ["pc"],
                "uintValues": [0x0880_4008u32],
                "floatValues": ["0.0"],
            }],
        });
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "cpu.stepping",
                json!({"event":"cpu.stepping","pc":0x0880_4004u32,"ticks":0}),
            ),
            (
                "cpu.stepping",
                json!({"event":"cpu.stepping","pc":0x0880_4008u32,"ticks":0}),
            ),
            ("cpu.getAllRegs", regs),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "step_instructions",
            json!({"count": 2}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["count"], 2);
        // No pre-pause cpu.stepping call — straight into two cpu.stepInto calls.
        assert_eq!(bridge.ws.calls[0].0, "cpu.status");
        assert_eq!(bridge.ws.calls[1].0, "cpu.stepInto");
        assert_eq!(bridge.ws.calls[2].0, "cpu.stepInto");
    }

    // --- poll_events ---

    fn gpr_only_pc(pc: u32) -> Value {
        json!({
            "event": "cpu.getAllRegs",
            "categories": [{
                "id": 0, "name": "GPR",
                "registerNames": ["pc"],
                "uintValues": [pc],
                "floatValues": ["0.0"],
            }],
        })
    }

    #[test]
    fn poll_events_ignores_non_stepping_spontaneous_events_and_counts_them_as_dropped() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        bridge
            .ws
            .push_event(json!({"event":"input.analog","stick":"left","x":0.0,"y":0.0}));
        bridge
            .ws
            .push_event(json!({"event":"cpu.resume"}));
        let resp = bridge.handle_request(Request::new(1, "poll_events", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["events"], json!([]));
        // Both discarded spontaneous events must be reported as dropped, not a hardcoded 0.
        assert_eq!(result["dropped"], 2);
    }

    #[test]
    fn poll_events_reports_zero_dropped_when_only_stops_arrive() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[("cpu.getAllRegs", gpr_only_pc(0x123))]));
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x123,"ticks":5}));
        let resp = bridge.handle_request(Request::new(1, "poll_events", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["events"].as_array().unwrap().len(), 1);
        assert_eq!(result["dropped"], 0);
    }

    #[test]
    fn poll_events_reports_generic_stop_when_pc_matches_no_breakpoint() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[("cpu.getAllRegs", gpr_only_pc(0x123))]));
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x123,"ticks":5}));
        let resp = bridge.handle_request(Request::new(1, "poll_events", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let events = resp.result.unwrap()["events"].clone();
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["type"], "stop");
        assert_eq!(events[0]["pc"], 0x123);
        assert_eq!(events[0]["regs"]["cpu.pc"], 0x123);
        assert!(events[0].get("breakpoint_id").is_none());
    }

    #[test]
    fn poll_events_classifies_exec_breakpoint_hit_by_pc_match() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            ("cpu.getAllRegs", gpr_only_pc(0x0880_4004)),
        ]));
        bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"address": 0x0880_4004u32}),
        ));
        bridge.ws.push_event(
            json!({"event":"cpu.stepping","pc":0x0880_4004u32,"ticks":42}),
        );
        let resp = bridge.handle_request(Request::new(2, "poll_events", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let events = resp.result.unwrap()["events"].clone();
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["type"], "breakpoint_hit");
        assert_eq!(events[0]["kind"], "exec");
        assert_eq!(events[0]["address"], 0x0880_4004u32);
        assert_eq!(events[0]["breakpoint_id"], 1);
        assert_eq!(events[0]["id"], 1);
        assert_eq!(events[0]["regs"]["cpu.pc"], 0x0880_4004u32);
    }

    #[test]
    fn poll_events_classifies_memory_breakpoint_hit_via_hits_delta() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "memory.breakpoint.add",
                json!({"event":"memory.breakpoint.add"}),
            ),
            // add reads back the live counter to seed last_hits — fresh memcheck, no hits yet.
            (
                "memory.breakpoint.list",
                json!({
                    "event": "memory.breakpoint.list",
                    "breakpoints": [
                        {"address": 0x0880_0100u32, "size": 4, "hits": 0},
                    ],
                }),
            ),
            ("cpu.getAllRegs", gpr_only_pc(0x0880_9000)),
            (
                "memory.breakpoint.list",
                json!({
                    "event": "memory.breakpoint.list",
                    "breakpoints": [
                        {"address": 0x0880_0100u32, "size": 4, "hits": 1},
                    ],
                }),
            ),
        ]));
        bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        // The stop's pc is the writing instruction's address (0x08809000), not the watched address
        // — exec pc-matching cannot attribute this, so the hits-delta cross-check must.
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x0880_9000u32,"ticks":7}));
        let resp = bridge.handle_request(Request::new(2, "poll_events", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let events = resp.result.unwrap()["events"].clone();
        assert_eq!(events[0]["type"], "breakpoint_hit");
        assert_eq!(events[0]["kind"], "write");
        assert_eq!(events[0]["address"], 0x0880_0100u32);
        assert_eq!(events[0]["breakpoint_id"], 1);

        // A second poll with an unchanged hits count must not re-report the same hit.
        bridge
            .ws
            .replies
            .push_back((
                "cpu.getAllRegs".to_string(),
                gpr_only_pc(0x0880_9010),
            ));
        bridge.ws.replies.push_back((
            "memory.breakpoint.list".to_string(),
            json!({
                "event": "memory.breakpoint.list",
                "breakpoints": [
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1},
                ],
            }),
        ));
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x0880_9010u32,"ticks":9}));
        let resp2 = bridge.handle_request(Request::new(3, "poll_events", json!({})));
        let events2 = resp2.result.unwrap()["events"].clone();
        assert_eq!(events2[0]["type"], "stop", "hits unchanged — not a new hit");
    }

    #[test]
    fn set_breakpoint_duplicate_address_inherits_hit_count_so_no_false_hit() {
        // PPSSPP reuses the existing memcheck and PRESERVES numHits on a re-add. A second
        // breakpoint at an already-hit address/size seeded last_hits=0 would make the very next
        // unrelated stop look like a fresh hit on it. The bridge seeds each add from PPSSPP's live
        // counter (`memory.breakpoint.list`), which for a duplicate returns the preserved hit count.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("memory.breakpoint.add", json!({"event":"memory.breakpoint.add"})),
            // bp1 seed — fresh memcheck, no hits yet.
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
            ),
            ("cpu.getAllRegs", gpr_only_pc(0x0880_9000)),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
            ),
            // bp2 add at the same address/size — seed reads the PRESERVED live count (1).
            ("memory.breakpoint.add", json!({"event":"memory.breakpoint.add"})),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
            ),
            ("cpu.getAllRegs", gpr_only_pc(0x0880_9010)),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
            ),
        ]));
        // bp1, then a real hit takes the shared counter to 1 (attributed to bp1).
        bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x0880_9000u32,"ticks":7}));
        let hit = bridge.handle_request(Request::new(2, "poll_events", json!({})));
        assert_eq!(hit.result.unwrap()["events"][0]["breakpoint_id"], 1);
        // bp2 at the SAME address/size — must inherit the current hit count (1), not reset to 0.
        let add2 = bridge.handle_request(Request::new(
            3,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        assert_eq!(add2.result.unwrap()["id"], 2);
        // An unrelated stop with the counter unchanged at 1 must be a generic stop, not a false hit.
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x0880_9010u32,"ticks":9}));
        let unrelated = bridge.handle_request(Request::new(4, "poll_events", json!({})));
        let ev = unrelated.result.unwrap()["events"][0].clone();
        assert_eq!(
            ev["type"], "stop",
            "an unrelated stop must not be misattributed to the duplicate breakpoint"
        );
        assert!(
            ev.get("breakpoint_id").is_none(),
            "no breakpoint should be credited for an unchanged hit count"
        );
    }

    #[test]
    fn set_breakpoint_after_reclear_seeds_from_live_counter_so_first_hit_not_missed() {
        // Seed logic: a sole breakpoint on a range is hit (shared memcheck
        // numHits→1), then cleared — clearing the LAST bridge id on the range removes the memcheck,
        // so PPSSPP resets its numHits. Re-adding creates a FRESH memcheck at numHits=0. Seeding the
        // re-add from PPSSPP's live counter (0) makes the first real hit (0→1) satisfy
        // `hits > last_hits`; a stale non-zero seed would miss it. (The duplicate-while-live case is
        // covered by set_breakpoint_duplicate_address_inherits_hit_count; the clear no longer nukes a
        // memcheck a surviving duplicate needs — see clearing_one_duplicate_memory_breakpoint_*.)
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            // bp1 add + seed (fresh, hits=0).
            ("memory.breakpoint.add", json!({"event":"memory.breakpoint.add"})),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
            ),
            // First hit poll → shared counter goes to 1 (attributed to bp1).
            ("cpu.getAllRegs", gpr_only_pc(0x0880_9000)),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
            ),
            // clear bp1 → sole id on the range, so the shared memcheck is removed (numHits reset).
            ("memory.breakpoint.remove", json!({"event":"memory.breakpoint.remove"})),
            // bp2 add + seed — PPSSPP recreated the memcheck, so its live counter is back to 0.
            ("memory.breakpoint.add", json!({"event":"memory.breakpoint.add"})),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
            ),
            // The first REAL hit after the reclear → counter 0→1; must be attributed to bp2.
            ("cpu.getAllRegs", gpr_only_pc(0x0880_9020)),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
            ),
        ]));
        bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x0880_9000u32,"ticks":7}));
        let first = bridge.handle_request(Request::new(2, "poll_events", json!({})));
        assert_eq!(first.result.unwrap()["events"][0]["breakpoint_id"], 1);
        // Clear bp1 — sole id on the range, so the memcheck (and its numHits) is torn down.
        let cleared = bridge.handle_request(Request::new(3, "clear_breakpoint", json!({"id": 1})));
        assert!(cleared.ok, "{:?}", cleared.error);
        // Re-add at the same address/size — a fresh memcheck at numHits=0.
        let re_add = bridge.handle_request(Request::new(
            4,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        assert_eq!(re_add.result.unwrap()["id"], 2);
        // The first real hit on the re-added breakpoint must NOT be missed.
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x0880_9020u32,"ticks":11}));
        let real = bridge.handle_request(Request::new(5, "poll_events", json!({})));
        let ev = real.result.unwrap()["events"][0].clone();
        assert_eq!(ev["type"], "breakpoint_hit", "first real hit after reclear was missed");
        assert_eq!(ev["breakpoint_id"], 2);
        assert_eq!(ev["kind"], "write");
        assert_eq!(ev["address"], 0x0880_0100u32);
    }

    #[test]
    fn clearing_one_duplicate_memory_breakpoint_keeps_the_shared_memcheck_for_the_survivor() {
        // PPSSPP keeps ONE memcheck per (address, size); bp1 and bp2 both watch it.
        // Clearing bp1 must NOT tear the shared memcheck down while bp2 still lives — otherwise bp2
        // would stay in list_breakpoints but never stop again. So the first clear sends no
        // memory.breakpoint.remove (a survivor remains) and a later access still attributes a hit to
        // bp2; clearing the LAST duplicate finally removes the memcheck.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            // bp1 add + seed (fresh, 0).
            ("memory.breakpoint.add", json!({"event":"memory.breakpoint.add"})),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
            ),
            // bp2 add + seed (duplicate, still 0).
            ("memory.breakpoint.add", json!({"event":"memory.breakpoint.add"})),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
            ),
            // clear bp1: survivor bp2 remains → NO memory.breakpoint.remove is queued here.
            // a write then hits (counter 0→1) — the survivor bp2 must still be credited.
            ("cpu.getAllRegs", gpr_only_pc(0x0880_9000)),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
            ),
            // clear bp2: now the last id on the range → the memcheck is removed.
            ("memory.breakpoint.remove", json!({"event":"memory.breakpoint.remove"})),
        ]));
        bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "read", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        bridge.handle_request(Request::new(
            2,
            "set_breakpoint",
            json!({"kind": "read", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        // Clear bp1 — bp2 survives, so the shared memcheck must stay (no remove call).
        let cleared = bridge.handle_request(Request::new(3, "clear_breakpoint", json!({"id": 1})));
        assert!(cleared.ok, "{:?}", cleared.error);
        assert!(
            !bridge
                .ws
                .calls
                .iter()
                .any(|(e, _)| e == "memory.breakpoint.remove"),
            "clearing a duplicate must not remove the memcheck the survivor still needs: {:?}",
            bridge.ws.calls
        );
        // bp2 still stops: a write hit is attributed to it, not lost.
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x0880_9000u32,"ticks":7}));
        let hit = bridge.handle_request(Request::new(4, "poll_events", json!({})));
        let ev = hit.result.unwrap()["events"][0].clone();
        assert_eq!(
            ev["type"], "breakpoint_hit",
            "the survivor stopped working after its duplicate was cleared"
        );
        assert_eq!(ev["breakpoint_id"], 2);
        // Clearing the last duplicate finally removes the shared memcheck.
        let cleared2 = bridge.handle_request(Request::new(5, "clear_breakpoint", json!({"id": 2})));
        assert!(cleared2.ok, "{:?}", cleared2.error);
        assert!(
            bridge.ws.calls.iter().any(|(e, p)| e == "memory.breakpoint.remove"
                && p["address"] == 0x0880_0100u32
                && p["size"] == 4),
            "clearing the last duplicate must remove the memcheck: {:?}",
            bridge.ws.calls
        );
    }

    #[test]
    fn set_breakpoint_rejects_read_and_write_on_the_same_range() {
        // A read and a write breakpoint on the SAME (address, size) collapse into one
        // PPSSPP memcheck with one shared hit counter, so a hit could not be told apart between the
        // two bridge ids. Refuse the ambiguous pair rather than advertise a disambiguation PPSSPP
        // cannot provide. (A different size on the same address is a distinct memcheck — allowed.)
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("memory.breakpoint.add", json!({"event":"memory.breakpoint.add"})),
            (
                "memory.breakpoint.list",
                json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
            ),
        ]));
        let read = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "read", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        assert!(read.ok, "{:?}", read.error);
        // A write on the exact same range must be refused before any WS round trip (only the two
        // add/seed calls above are queued — a WS call here would panic on an unexpected event).
        let write = bridge.handle_request(Request::new(
            2,
            "set_breakpoint",
            json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
        ));
        assert!(!write.ok);
        assert_eq!(write.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn poll_events_breakpoint_id_filter_holds_back_non_matching_events() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            ("cpu.getAllRegs", gpr_only_pc(0x100)),
            ("cpu.getAllRegs", gpr_only_pc(0x200)),
        ]));
        bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x100})));
        bridge.handle_request(Request::new(2, "set_breakpoint", json!({"address": 0x200})));
        bridge.ws.push_event(json!({"event":"cpu.stepping","pc":0x100,"ticks":1}));
        bridge.ws.push_event(json!({"event":"cpu.stepping","pc":0x200,"ticks":2}));

        let filtered = bridge.handle_request(Request::new(
            3,
            "poll_events",
            json!({"breakpoint_id": 2}),
        ));
        let events = filtered.result.unwrap()["events"].clone();
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["breakpoint_id"], 2);

        // The id=1 hit was held back, not dropped — an unfiltered poll must still see it.
        let unfiltered = bridge.handle_request(Request::new(4, "poll_events", json!({})));
        let events = unfiltered.result.unwrap()["events"].clone();
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["breakpoint_id"], 1);
    }

    #[test]
    fn poll_events_malformed_filter_errors_without_losing_buffered_hits() {
        // A malformed breakpoint_id must be rejected BEFORE the transport is drained — otherwise the
        // failed request destructively consumes an already-buffered breakpoint-hit event and loses
        // it forever. Only one cpu.getAllRegs reply is queued (for the later valid poll's enrich):
        // the malformed poll must not consume anything from the transport.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
            ("cpu.getAllRegs", gpr_only_pc(0x100)),
        ]));
        bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x100})));
        bridge
            .ws
            .push_event(json!({"event":"cpu.stepping","pc":0x100,"ticks":1}));

        let bad = bridge.handle_request(Request::new(
            2,
            "poll_events",
            json!({"breakpoint_id": "not-a-number"}),
        ));
        assert!(!bad.ok);
        assert_eq!(bad.error.unwrap().kind, "bad_params");

        // The buffered hit survived the failed poll — a subsequent valid poll still surfaces it.
        let good = bridge.handle_request(Request::new(3, "poll_events", json!({})));
        let events = good.result.unwrap()["events"].clone();
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["breakpoint_id"], 1);
    }

    // --- screenshot ---

    #[test]
    fn screenshot_decodes_data_uri_into_uniform_png_base64() {
        let png_bytes = [
            0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0xde, 0xad,
        ];
        let b64 = base64::engine::general_purpose::STANDARD.encode(png_bytes);
        let uri = format!("data:image/png;base64,{b64}");
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "emucap.screenshot",
                json!({"event":"emucap.screenshot","width":480,"height":272,"uri":uri}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(1, "screenshot", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["png_base64"], b64);
        assert_eq!(result["width"], 480);
        assert_eq!(result["height"], 272);
    }

    #[test]
    fn screenshot_defaults_dimensions_when_reply_omits_them() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
        let uri = format!("data:image/png;base64,{b64}");
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "emucap.screenshot",
                json!({"event":"emucap.screenshot","uri":uri}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(1, "screenshot", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["width"], 480);
        assert_eq!(result["height"], 272);
    }

    #[test]
    fn screenshot_rejects_reply_missing_uri_field() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "emucap.screenshot",
                json!({"event":"emucap.screenshot"}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(1, "screenshot", json!({})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "emulator_error");
    }

    #[test]
    fn screenshot_rejects_while_cpu_halted() {
        // emucap.screenshot drives GE stepping, which only progresses while the CPU is running —
        // a halted core must fail fast (bad_params) instead of riding PPSSPP's own ~5s wait to an
        // emulator_error. The FakeWs has no "emucap.screenshot" reply queued, so this test would
        // fail loudly (unexpected fake WS call) if the proactive guard were missing.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.status",
            json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
        )]));
        let resp = bridge.handle_request(Request::new(1, "screenshot", json!({})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert_eq!(
            bridge.ws.calls.len(),
            1,
            "must not call emucap.screenshot while the CPU is halted"
        );
    }

    // --- set_input / press_buttons ---

    #[test]
    fn set_input_sends_full_button_map_with_requested_true_rest_false() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "input.buttons.send",
            json!({"event":"input.buttons.send"}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_input",
            json!({"buttons": ["a", "up"]}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let (event, params) = &bridge.ws.calls[0];
        assert_eq!(event, "input.buttons.send");
        let buttons = &params["buttons"];
        assert_eq!(buttons["cross"], true);
        assert_eq!(buttons["up"], true);
        assert_eq!(buttons["circle"], false);
        assert_eq!(buttons["down"], false);
        assert_eq!(buttons["triangle"], false);
        assert_eq!(buttons["square"], false);
        assert_eq!(buttons["ltrigger"], false);
        assert_eq!(buttons["rtrigger"], false);
        assert_eq!(buttons["start"], false);
        assert_eq!(buttons["select"], false);
        assert_eq!(buttons["left"], false);
        assert_eq!(buttons["right"], false);
    }

    #[test]
    fn set_input_empty_list_releases_every_tracked_button() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "input.buttons.send",
            json!({"event":"input.buttons.send"}),
        )]));
        let resp = bridge.handle_request(Request::new(1, "set_input", json!({"buttons": []})));
        assert!(resp.ok, "{:?}", resp.error);
        let params = &bridge.ws.calls[0].1;
        for psp_name in [
            "cross", "circle", "triangle", "square", "ltrigger", "rtrigger", "start", "select",
            "up", "down", "left", "right",
        ] {
            assert_eq!(
                params["buttons"][psp_name], false,
                "{psp_name} must be released"
            );
        }
    }

    #[test]
    fn set_input_rejects_unknown_button() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "set_input",
            json!({"buttons": ["nonsense"]}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(bridge.ws.calls.is_empty());
    }

    #[test]
    fn press_buttons_maps_uniform_name_to_psp_and_sends_duration() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "input.buttons.press",
                json!({"event":"input.buttons.press","ticket":"emucap-1"}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["a"], "frames": 3}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let (event, params) = &bridge.ws.calls[1];
        assert_eq!(event, "input.buttons.press");
        assert_eq!(params["button"], "cross");
        assert_eq!(params["duration"], 3);
        // The request is ticket-tagged so its delayed release ack can be correlated.
        assert_eq!(params["ticket"], "emucap-1");
    }

    #[test]
    fn press_buttons_rejects_combo_before_any_ws_mutation() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["up", "a"], "frames": 2}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(bridge.ws.calls.is_empty());
    }

    #[test]
    fn press_buttons_defaults_frames_to_one() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "input.buttons.press",
                json!({"event":"input.buttons.press","ticket":"emucap-1"}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["start"]}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(bridge.ws.calls[1].1["duration"], 1);
    }

    #[test]
    fn press_buttons_timeout_releases_inputs_and_surfaces_error() {
        // An exec breakpoint halts the CPU mid-press. The pre-check passed while
        // running, then frames stopped, so PPSSPP's timed release ack never fires and the ticketed
        // read times out (WouldBlock) with the button still held. The bridge must release every
        // input (empty input.buttons.send) and return a clear timeout error, not leave it stuck.
        let mut ws = FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "input.buttons.send",
                json!({"event":"input.buttons.send"}),
            ),
        ]);
        ws.timeout_events.insert("input.buttons.press".into());
        let mut bridge = PpssppBridge::new(ws);
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["a"], "frames": 240}),
        ));
        assert!(!resp.ok, "a timed-out press must not report success");
        assert!(
            resp.error.unwrap().message.contains("timed out"),
            "error should explain the mid-press timeout"
        );
        // calls: cpu.status, the (timed-out) ticketed press, then the release.
        let events: Vec<&str> = bridge.ws.calls.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            events,
            ["cpu.status", "input.buttons.press", "input.buttons.send"]
        );
        // The recovery release drives every button false so nothing stays held.
        let release = &bridge.ws.calls[2].1["buttons"];
        assert_eq!(release["cross"], false);
        assert_eq!(release["up"], false);
    }

    #[test]
    fn press_buttons_ignores_a_stale_off_ticket_ack() {
        // A late ack from an earlier interrupted press (a different ticket) must not satisfy this
        // press — the bridge waits for its own ticket and queues the stale ack as ignored.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "input.buttons.press",
                json!({"event":"input.buttons.press","ticket":"stale-old"}),
            ),
            (
                "input.buttons.press",
                json!({"event":"input.buttons.press","ticket":"emucap-1"}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["a"], "frames": 1}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        // The stale off-ticket ack was skipped (queued), not misattributed to this press.
        let drained = bridge.ws.drain_events();
        assert!(
            drained.iter().any(|e| e.get("ticket") == Some(&json!("stale-old"))),
            "the stale ack should have been queued/ignored, not consumed as this press's reply"
        );
    }

    #[test]
    fn press_buttons_rejects_while_cpu_halted() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "cpu.status",
            json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["a"]}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn press_buttons_rejects_frames_over_cap() {
        // A large hold (e.g. 10s ~= 600 frames at 60fps) would block call() past the bridge
        // binary's own 8s WS read timeout, reproducing the stale-reply misattribution race — must
        // be rejected up front, before any WS round trip (FakeWs has no replies queued at all).
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["a"], "frames": MAX_PRESS_FRAMES + 1}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(
            bridge.ws.calls.is_empty(),
            "must reject before calling PPSSPP"
        );
    }

    #[test]
    fn press_buttons_accepts_frames_at_the_cap() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            (
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
            ),
            (
                "input.buttons.press",
                json!({"event":"input.buttons.press","ticket":"emucap-1"}),
            ),
        ]));
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["a"], "frames": MAX_PRESS_FRAMES}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(bridge.ws.calls[1].1["duration"], MAX_PRESS_FRAMES);
    }

    #[test]
    fn press_buttons_rejects_unknown_button() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(
            1,
            "press_buttons",
            json!({"buttons": ["nonsense"]}),
        ));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(bridge.ws.calls.is_empty());
    }

    #[test]
    fn press_buttons_requires_at_least_one_button() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, "press_buttons", json!({"buttons": []})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(bridge.ws.calls.is_empty());
    }

    // --- save_state / load_state / reset ---

    #[test]
    fn save_state_calls_savestate_save_with_path() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "savestate.save",
            json!({"event":"savestate.save","path":"/tmp/x.ppst"}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "save_state",
            json!({"path": "/tmp/x.ppst"}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["status"], "completed");
        assert_eq!(bridge.ws.calls[0].0, "savestate.save");
        assert_eq!(bridge.ws.calls[0].1["path"], "/tmp/x.ppst");
    }

    #[test]
    fn save_state_requires_path() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, "save_state", json!({})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn call_with_timeout_below_a_slow_reply_budget_times_out_like_the_old_default() {
        // Reproduces the desync directly at the transport: the emucap fork's savestate handler can
        // take up to 15s to reply, so an 8s read budget times out mid-save (a bridge_error) while
        // PPSSPP is still working — but the dedicated savestate budget (>15s) tolerates it.
        let mut ws = FakeWs::with(&[(
            "savestate.save",
            json!({"event":"savestate.save","path":"/tmp/x.ppst","message":"Saved State"}),
        )]);
        ws.slow_replies
            .insert("savestate.save".into(), Duration::from_secs(15));
        // The old 8s default would have surfaced a spurious failure on a save that succeeds.
        assert!(ws
            .call_with_timeout("savestate.save", json!({"path":"/tmp/x.ppst"}), Duration::from_secs(8))
            .is_err());
        // The dedicated budget outlasts the fork's 15s wait, so the reply is consumed cleanly.
        assert!(ws
            .call_with_timeout(
                "savestate.save",
                json!({"path":"/tmp/x.ppst"}),
                SAVESTATE_READ_TIMEOUT
            )
            .is_ok());
    }

    #[test]
    fn save_state_threads_a_read_budget_above_the_forks_15s_wait() {
        // A save that takes ~15s must not time out on the bridge side. The savestate call is given
        // a budget past the fork's 15s wait; with the old 8s default this save would spuriously
        // fail and leave PPSSPP's late reply to desync the next request.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "savestate.save",
            json!({"event":"savestate.save","path":"/tmp/x.ppst","message":"Saved State"}),
        )]));
        bridge
            .ws
            .slow_replies
            .insert("savestate.save".into(), Duration::from_secs(15));
        let resp = bridge.handle_request(Request::new(
            1,
            "save_state",
            json!({"path": "/tmp/x.ppst"}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["status"], "completed");
        let (event, budget) = bridge.ws.call_timeouts.last().unwrap();
        assert_eq!(event, "savestate.save");
        assert!(
            *budget > Duration::from_secs(15),
            "savestate budget {budget:?} must outlast the fork's 15s wait"
        );
    }

    #[test]
    fn load_state_threads_a_read_budget_above_the_forks_15s_wait() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "savestate.load",
            json!({"event":"savestate.load","path":"/tmp/x.ppst","message":"Loaded State"}),
        )]));
        bridge
            .ws
            .slow_replies
            .insert("savestate.load".into(), Duration::from_secs(15));
        let resp = bridge.handle_request(Request::new(
            1,
            "load_state",
            json!({"path": "/tmp/x.ppst"}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let (event, budget) = bridge.ws.call_timeouts.last().unwrap();
        assert_eq!(event, "savestate.load");
        assert!(*budget > Duration::from_secs(15));
    }

    #[test]
    fn load_state_calls_savestate_load_with_path() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "savestate.load",
            json!({"event":"savestate.load","path":"/tmp/x.ppst"}),
        )]));
        let resp = bridge.handle_request(Request::new(
            1,
            "load_state",
            json!({"path": "/tmp/x.ppst"}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["status"], "completed");
        assert_eq!(bridge.ws.calls[0].0, "savestate.load");
        assert_eq!(bridge.ws.calls[0].1["path"], "/tmp/x.ppst");
    }

    #[test]
    fn load_state_requires_path() {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, "load_state", json!({})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn reset_calls_game_reset_with_reboot_budget_and_reports_post_reset_pc() {
        // Headless path: the fork blocks game.reset until the reboot completed and left the core
        // halted at the fresh boot entry, so the halt poll reads stepping on the first check and the
        // bridge reports a confirmed completion with the boot-entry pc.
        let mut bridge = PpssppBridge::new(FakeWs::with(&[
            ("game.reset", json!({"event":"game.reset"})),
            ("cpu.status", json!({"event":"cpu.status","stepping":true,"paused":false})),
            ("cpu.getAllRegs", gpr_only_pc(0x0880_4128)),
        ]));
        let resp = bridge.handle_request(Request::new(1, "reset", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "completed");
        // `status` is the single source of truth for whether the core is halted — no redundant
        // `stopped` boolean derivable from it.
        assert!(result.get("stopped").is_none());
        assert_eq!(result["post_reset_pc"], 0x0880_4128u32);
        assert_eq!(bridge.ws.calls[0].0, "game.reset");
        // game.reset must ride the extended reboot budget, not the default fail-fast read — an 8s
        // read would time out mid-reboot and desync the channel (like save_state).
        assert_eq!(
            bridge.ws.call_timeouts,
            vec![("game.reset".to_string(), RESET_READ_TIMEOUT)]
        );
    }

    #[test]
    fn reset_display_session_reports_async_reboot_not_false_completed_while_running() {
        // display:true GUI session: the fork does NOT block game.reset (only the headless build
        // does), so the ack returns while the reboot is still queued on the GUI pump and the core
        // keeps running. The halt poll never reads stepping, so the bridge must report the async
        // reboot truthfully — NOT a false "completed" with the stale, still-in-game pc.
        let mut replies: Vec<(&str, Value)> = vec![("game.reset", json!({"event":"game.reset"}))];
        for _ in 0..RESET_HALT_POLLS {
            replies.push((
                "cpu.status",
                json!({"event":"cpu.status","stepping":false,"paused":false}),
            ));
        }
        let mut bridge = PpssppBridge::new(FakeWs::with(&replies));
        let resp = bridge.handle_request(Request::new(1, "reset", json!({})));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_ne!(
            result["status"], "completed",
            "must not claim completed while the GUI reboot is still in flight"
        );
        assert_eq!(result["status"], "rebooting");
        // No redundant `stopped` boolean — `status` alone distinguishes rebooting (running) from
        // completed (halted).
        assert!(result.get("stopped").is_none());
        // No boot-entry pc is claimed while the core is still running the pre-reset game — the live
        // pc would be a stale, misleading value, not reset evidence.
        assert!(result.get("post_reset_pc").is_none());
    }

    // --- dump_memory ---

    #[test]
    fn dump_memory_writes_bin_and_regions_under_requested_directory() {
        // `main` (user RAM) streams in `MAX_READ_LEN` chunks; a fixed 0xAB byte per chunk lets the
        // test assert the whole region was concatenated in order. The bridge writes only the region
        // .bins + regions.json — state.json is the MCP host's job (src/live/tools.rs).
        let full_chunk_b64 =
            base64::engine::general_purpose::STANDARD.encode(vec![0xABu8; MAX_READ_LEN]);
        let mut replies: Vec<(&str, Value)> = Vec::new();
        let mut offset = 0u64;
        while offset < PSP_MAIN_RAM_SIZE {
            let chunk = MAX_READ_LEN.min((PSP_MAIN_RAM_SIZE - offset) as usize);
            // The region size is an exact multiple of MAX_READ_LEN, so every chunk is full-size.
            assert_eq!(chunk, MAX_READ_LEN);
            replies.push((
                "memory.read",
                json!({"event": "memory.read", "base64": full_chunk_b64.clone()}),
            ));
            offset += chunk as u64;
        }
        let mut bridge = PpssppBridge::new(FakeWs::with(&replies));
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("dump");
        let resp = bridge.handle_request(Request::new(
            12,
            "dump_memory",
            json!({"path": out.to_str().unwrap()}),
        ));
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["regions"], 1);
        assert_eq!(result["path"], out.display().to_string());

        // main.bin: the whole 24 MiB region, every chunk concatenated in order.
        let bin = std::fs::read(out.join("main.bin")).unwrap();
        assert_eq!(bin.len() as u64, PSP_MAIN_RAM_SIZE);
        assert!(bin.iter().all(|&b| b == 0xAB));

        // regions.json: the canonical RegionMeta shape the cross-ROM diff loader consumes
        // (`src/analysis/dump.rs`).
        let regions: Value =
            serde_json::from_slice(&std::fs::read(out.join("regions.json")).unwrap()).unwrap();
        let arr = regions.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "main");
        assert_eq!(arr[0]["memory_type"], "main");
        assert_eq!(arr[0]["base_address"], PSP_MAIN_RAM_BASE);
        assert_eq!(arr[0]["size"], PSP_MAIN_RAM_SIZE);
    }

    #[test]
    fn dump_memory_short_read_fails_without_writing_a_mismatched_bin() {
        // A `memory.read` reply that decodes to fewer bytes than requested is a short read: the dump
        // must fail rather than publish a `main.bin` smaller than the `PSP_MAIN_RAM_SIZE` its
        // `regions.json` advertises, and it must not leave any partial artifact behind.
        let short_b64 =
            base64::engine::general_purpose::STANDARD.encode(vec![0xABu8; MAX_READ_LEN - 1]);
        let mut bridge = PpssppBridge::new(FakeWs::with(&[(
            "memory.read",
            json!({"event": "memory.read", "base64": short_b64}),
        )]));
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("dump");
        let resp = bridge.handle_request(Request::new(
            12,
            "dump_memory",
            json!({"path": out.to_str().unwrap()}),
        ));
        assert!(!resp.ok, "a short read must fail the dump");
        assert!(
            resp.error.unwrap().message.contains("short read"),
            "the error should name the short read"
        );
        assert!(
            !out.join("main.bin").exists(),
            "a short read must not leave a truncated main.bin"
        );
        assert!(!out.join("regions.json").exists());
    }

    #[test]
    fn dump_memory_midstream_failure_preserves_the_prior_dump() {
        // A re-dump that fails part way through must not clobber a prior good dump with a truncated
        // `main.bin` beside a stale `regions.json`/`state.json`.
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("dump");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            out.join("main.bin"),
            vec![0xCDu8; PSP_MAIN_RAM_SIZE as usize],
        )
        .unwrap();
        std::fs::write(out.join("regions.json"), b"[{\"name\":\"main\"}]").unwrap();
        // state.json is normally written by the host after the bridge returns; a prior one must
        // survive a failed re-dump too.
        std::fs::write(out.join("state.json"), b"{\"cpu.pc\":1}").unwrap();

        // New dump: first chunk reads fine, the second reply arrives with no base64 field → the read
        // errors mid-stream (models a dropped/garbled reply part way through the region).
        let full_chunk_b64 =
            base64::engine::general_purpose::STANDARD.encode(vec![0xABu8; MAX_READ_LEN]);
        let replies: Vec<(&str, Value)> = vec![
            (
                "memory.read",
                json!({"event": "memory.read", "base64": full_chunk_b64}),
            ),
            ("memory.read", json!({"event": "memory.read"})),
        ];
        let mut bridge = PpssppBridge::new(FakeWs::with(&replies));
        let resp = bridge.handle_request(Request::new(
            13,
            "dump_memory",
            json!({"path": out.to_str().unwrap()}),
        ));
        assert!(!resp.ok, "a mid-stream read failure must fail the dump");

        // The prior good dump is intact — byte-for-byte, metadata and all.
        let bin = std::fs::read(out.join("main.bin")).unwrap();
        assert_eq!(bin.len() as u64, PSP_MAIN_RAM_SIZE);
        assert!(
            bin.iter().all(|&b| b == 0xCD),
            "the prior main.bin must be preserved, not overwritten by a truncated new one"
        );
        assert_eq!(
            std::fs::read(out.join("regions.json")).unwrap(),
            b"[{\"name\":\"main\"}]"
        );
        assert!(
            out.join("state.json").exists(),
            "the prior state.json must survive a failed re-dump"
        );
        // The bridge writes region files directly into the requested dir (the host owns the atomic
        // dir swap), so a mid-stream failure must leave no `.partial` region temp behind in it.
        let leftovers: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".partial"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "partial region temp must be cleaned up on failure"
        );
    }

    // --- get_rom_info ---

    #[test]
    fn get_rom_info_reports_sha1_size_and_game_status() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "emucap-ppsspp-bridge-test-{}-reports-sha1.iso",
            std::process::id()
        ));
        std::fs::write(&path, b"hello psp").expect("write temp content");
        let mut bridge = PpssppBridge::with_content(
            FakeWs::with(&[(
                "game.status",
                json!({"event":"game.status","game":{"id":"ULJS00097","title":"Tales of Destiny 2"},"paused":false}),
            )]),
            Some(path.clone()),
        );
        let resp = bridge.handle_request(Request::new(1, "get_rom_info", json!({})));
        std::fs::remove_file(&path).ok();
        assert!(resp.ok, "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["system"], "psp");
        assert_eq!(result["adapter"], "ppsspp-rust-ws");
        // sha1("hello psp"), verified independently via `shasum -a1`.
        assert_eq!(result["sha1"], "51ce64b9e8869767e47fe87d9f13f5c626292273");
        assert_eq!(result["size"], 9);
        assert_eq!(result["game"]["id"], "ULJS00097");
        assert_eq!(result["game"]["title"], "Tales of Destiny 2");
    }

    #[test]
    fn get_rom_info_requires_content_env() {
        let mut bridge = PpssppBridge::with_content(FakeWs::with(&[]), None);
        let resp = bridge.handle_request(Request::new(1, "get_rom_info", json!({})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }

    #[test]
    fn get_rom_info_rejects_missing_content_file() {
        let mut bridge = PpssppBridge::with_content(
            FakeWs::with(&[]),
            Some(std::path::PathBuf::from(
                "/nonexistent/emucap-ppsspp-test.iso",
            )),
        );
        let resp = bridge.handle_request(Request::new(1, "get_rom_info", json!({})));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().kind, "bad_params");
    }
}
