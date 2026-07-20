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
//!   step completes (`SteppingSubscriber.cpp`). The fork echoes each request's `ticket` on these
//!   asynchronous stepping/resume acknowledgements, so the transport can correlate them just as it
//!   does ordinary same-name replies and errors.
//! - The `cpu.stepping` event's optional `reason`/`relatedAddress` fields (which would otherwise say
//!   *why* the CPU stopped, e.g. `"cpu.breakpoint"`) are in practice never populated — `Core_Break()`
//!   sets `g_cpuStepCommand.type = CPUStepType::None` in the same breath it stores the reason, which
//!   makes `Core_GetSteppingReason()`'s `!g_cpuStepCommand.empty()` guard immediately false. So a
//!   breakpoint hit and a plain stepping-request completion produce the *same* bare `{pc, ticks}`
//!   event; `poll_events` classifies a hit by matching the event's `pc` against tracked exec
//!   breakpoints (mirroring the NDS bridge) and, for memory breakpoints (which trip on a data access
//!   at some *other* pc), by a `memory.breakpoint.list` hit-count delta.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};
use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

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
/// frames) runs past the bridge binary's 8s WS read timeout (`emucap-ppsspp-bridge.rs`). Tickets keep
/// a late release from being mistaken for another command, but the original call would still fail
/// and need best-effort input cleanup. 240 frames (~4s at 60 fps) leaves a comfortable margin under
/// that timeout for the round trip and any frame-timing jitter; hold a button longer by issuing
/// repeated `press_buttons` calls or `set_input` instead.
const MAX_PRESS_FRAMES: u64 = 240;

/// Dedicated WS read budget for `save_state`/`load_state`, threaded per-call over the bridge's
/// default read timeout (8s, `emucap-ppsspp-bridge.rs`). The emucap fork's `SaveStateSubscriber.cpp`
/// runs the async save/load on the EmuThread and blocks up to `cv.wait_for(..., seconds(15))` before
/// it replies. The 8s default is shorter than that worst case, so a slow (>8s) save/load would time
/// out on the bridge's socket read while PPSSPP is still working, reporting a spurious failure.
/// Ticket correlation prevents that late response from contaminating the next request, while this
/// dedicated budget avoids the false timeout in the first place. It remains bounded so a genuinely
/// wedged save still surfaces an error rather than hanging forever.
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
/// it is merely not-yet-callable; the instruction-granularity capability is advertised on the wire
/// as `step_instructions` and normalized by the MCP to `step` with an instructions-only constraint.
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
mod transport;
pub use transport::{TungsteniteWs, WsTransport};

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
    /// Last persistent replacement applied through this bridge. `None` means ownership is not
    /// observable yet (for example, the bridge attached to an already-running PPSSPP instance).
    /// Once this process performs set/release, reconnecting MCP sessions can recover that state.
    held_buttons: Option<Vec<String>>,
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
            held_buttons: None,
        }
    }

    pub fn backend_terminal(&self) -> bool {
        self.ws.is_terminal()
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
}

/// True when `err` is a socket read that timed out (the transport's per-read budget elapsed) — as
/// opposed to a PPSSPP-reported error or a protocol/parse failure. A tungstenite read timeout
/// surfaces as `Ws(Io(WouldBlock|TimedOut))`; `FakeWs` models it as `Io(WouldBlock)`.
mod debug;
mod input_state;
mod service;
mod support;
use support::*;

#[cfg(test)]
#[path = "ppsspp_bridge_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ppsspp_bridge_temporal_tests.rs"]
mod temporal_tests;

#[cfg(test)]
#[path = "ppsspp_bridge_transport_tests.rs"]
mod transport_tests;
