//! Nintendo DS (DeSmuME) GDB-RSP ↔ emucap wire-protocol bridge.
//!
//! DeSmuME's headless CLI exposes one standard GDB-RSP stub per ARM core (ARM9, ARM7),
//! each on its own TCP port. This bridge speaks emucap's line-JSON protocol on one side and
//! standard RSP (`c`/`s`/`m`/`M`/`g`/`Z0`/`z0`/`?`) to those stubs on the other, routing
//! memory/registers/stepping/breakpoints to the ARM9 or ARM7 connection per request.
//!
//! Transport (`GdbRspClient`, `GdbTransport`, `GdbBridgeEnv`) comes from the adapter-neutral
//! `gdb_rsp` module.
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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::gdb_rsp::{GdbBridgeEnv, GdbError, GdbTransport};
use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

/// Bulk-read chunk size for the GDB `m addr,len` path. A read reply is 2 hex chars per byte written
/// into the DeSmuME stub's fixed `hidden_buffer[BUFMAX]`; a chunk whose reply overruns that buffer
/// segfaults the emulator (the crash the `0006` fork patch also hard-guards against in the `'m'`
/// handler). Kept provably within `GDBSTUB_BUFMAX` — see `read_chunk_fits_gdbstub_reply_buffer`.
const MAX_READ_CHUNK: usize = 0x2000;
/// The DeSmuME fork's GDB-stub reply buffer size (`BUFMAX` in gdbstub.cpp, raised from 2 KiB by the
/// `0006` fork patch). The bridge must never issue an `m` read whose hex reply + framing exceeds it.
const GDBSTUB_BUFMAX: usize = 0x8000;
/// Bytes the stub adds around an `m` reply payload in `hidden_buffer`: the leading `$`, the trailing
/// `#` + two checksum chars, and the NUL. `2*len + GDBSTUB_REPLY_FRAMING` must fit `GDBSTUB_BUFMAX`.
const GDBSTUB_REPLY_FRAMING: usize = 5;
// Compile-time guard: a full MAX_READ_CHUNK read reply must fit the fork's gdbstub reply buffer, or a
// bulk read (dump_memory/find_pattern) overflows `hidden_buffer` and segfaults DeSmuME (the live crash
// this fix targets). Enforced at build time so a future MAX_READ_CHUNK bump cannot silently regress it.
const _: () = assert!(MAX_READ_CHUNK * 2 + GDBSTUB_REPLY_FRAMING <= GDBSTUB_BUFMAX);
/// Bulk-write chunk size for the GDB `M addr,len:hex` path. Unlike a read (whose *reply* fills the
/// stub buffer), a write's inbound *packet* does — 2 hex chars per byte plus the `M`-header — so an
/// over-large write packet overruns the stub's fixed input buffer and DeSmuME silently drops it
/// (lost write + multi-second stall). A larger write is split into chunks this size, all under one
/// freeze so a running core cannot advance and tear it. Provably within `GDBSTUB_BUFMAX` — see below.
const MAX_WRITE_CHUNK: usize = 0x2000;
/// Bytes the `M addr,len:` header (`M` + ≤8 addr hex + `,` + ≤8 len hex + `:`) and the `$..#cc`
/// framing add around a write packet's hex payload — an upper bound. `2*len + GDBSTUB_WRITE_FRAMING`
/// must fit `GDBSTUB_BUFMAX`.
const GDBSTUB_WRITE_FRAMING: usize = 32;
// Compile-time guard: a full MAX_WRITE_CHUNK `M` packet must fit the stub's input buffer, or a
// chunked write overflows it and DeSmuME silently drops the packet (the lost-write defect this cap
// fixes). Enforced at build time so a future MAX_WRITE_CHUNK bump cannot silently regress it.
const _: () = assert!(MAX_WRITE_CHUNK * 2 + GDBSTUB_WRITE_FRAMING <= GDBSTUB_BUFMAX);
/// Cap on a single `read_memory` length (matches the Mesen adapter's READ_CAP). A larger region is
/// read in chunks — an unbounded length would preallocate `length*2` and tie up the bridge on one
/// request. The region-size check in `route` also bounds it, but this caps the 4 GB `arm9`/`arm7` bus.
const MAX_READ_LEN: usize = 0x2_0000;
/// Cap on a single `write_memory` length (mirrors MAX_READ_LEN). A larger write is rejected rather
/// than streamed; within the cap it is split into MAX_WRITE_CHUNK packets.
const MAX_WRITE_LEN: usize = 0x2_0000;
/// Cap on a single `find_pattern` scan window (128 KB, matching the pc98 adapter). The 4 GB
/// `arm9`/`arm7` bus views would otherwise stream the whole address space over the GDB `m` path;
/// a longer request is scanned up to this cap and reported as `truncated_scan`.
const MAX_FIND_LEN: usize = 128 * 1024;
/// The bridge must return a terminal NDJSON response before the outer link's five-second read
/// deadline. DeSmuME's GDB stub cannot stream keepalives while the bridge polls a timed override,
/// so accept only a bounded real-time pulse here; longer holds remain available through
/// `set_input` plus an explicit release.
const MAX_SYNC_TIMED_INPUT_FRAMES: u64 = 120;
const TIMED_INPUT_POLL_INTERVAL: Duration = Duration::from_millis(8);
const TIMED_INPUT_DEADLINE: Duration = Duration::from_millis(4_000);

const METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "read_memory",
    "write_memory",
    "get_state",
    "find_pattern",
    "dump_memory",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimedOverrideTerminal {
    Completed,
    Interrupted { frames_elapsed: u64 },
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
    /// True for a bounded RAM window that `dump_memory` snapshots whole. The `arm9`/`arm7`
    /// full-bus views (4 GB, mostly ROM/MMIO/mirrors) are not dumpable — only the finite `main`
    /// RAM is meaningful for a cross-ROM diff.
    dumpable: bool,
}

/// v1 minimal NDS memory map. `main` is the shared 4 MB Main RAM (read via the ARM9 bus by
/// default); `arm9`/`arm7` expose each core's full bus with absolute addressing (base 0).
const MEMORY_REGIONS: &[NdsRegion] = &[
    NdsRegion {
        name: "main",
        base: 0x0200_0000,
        size: 0x0040_0000,
        cpu: CpuId::Arm9,
        dumpable: true,
    },
    NdsRegion {
        name: "arm9",
        base: 0,
        size: 0x1_0000_0000,
        cpu: CpuId::Arm9,
        dumpable: false,
    },
    NdsRegion {
        name: "arm7",
        base: 0,
        size: 0x1_0000_0000,
        cpu: CpuId::Arm7,
        dumpable: false,
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
    #[error(transparent)]
    Gdb(#[from] GdbError),
}

type NdsResult<T> = Result<T, NdsBridgeError>;

/// One CPU's GDB-RSP connection plus the async-stop bookkeeping for that core.
struct CpuConn<G> {
    id: CpuId,
    gdb: G,
    frozen: bool,
    events: Vec<Value>,
}

pub struct NdsBridge<G> {
    arm9: CpuConn<G>,
    arm7: Option<CpuConn<G>>,
    env: GdbBridgeEnv,
    bps: BTreeMap<u64, NdsBreakpoint>,
    next_bp: u64,
    events: Vec<Value>,
}

impl<G: GdbTransport> NdsBridge<G> {
    pub fn new(arm9: G, arm7: Option<G>, env: GdbBridgeEnv) -> Self {
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
            "find_pattern" => self.find_pattern(&req.params),
            "dump_memory" => self.dump_memory(&req.params),
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

    pub fn backend_terminal(&self) -> bool {
        self.arm9.gdb.is_terminal()
            || self
                .arm7
                .as_ref()
                .is_some_and(|connection| connection.gdb.is_terminal())
    }
}

mod breakpoints;
mod cpu;
mod debug;
mod input_state;
mod service;
mod support;
use support::*;

#[cfg(test)]
#[path = "nds_bridge_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "nds_bridge_temporal_tests.rs"]
mod temporal_tests;
