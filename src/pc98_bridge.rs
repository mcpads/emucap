use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

const MAX_READ_CHUNK: usize = 0x4000;
const MAX_FIND_LEN: usize = 128 * 1024;
const MAX_SYNC_TIMED_INPUT_FRAMES: u64 = 120;
const TRACE_CAP: usize = 4096;
const LEGACY_STATE_FORMAT: &str = "emucap-mame-pc98-state-v1";
const STATE_FORMAT: &str = "emucap-mame-pc98-state-v2";
const SAVE_ITEMS_DIR: &str = "saveitems";
const SAVE_ITEMS_MANIFEST: &str = "saveitems/manifest.txt";
const METHODS: &[&str] = &[
    "hello",
    "status",
    "read_memory",
    "find_pattern",
    "dump_memory",
    "get_rom_info",
    "write_memory",
    "get_state",
    "save_state",
    "load_state",
    "probe",
    "poll_events",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "screenshot",
    "set_input",
    "press_buttons",
    "reset",
    "break_on_reset",
    "step",
    "step_instructions",
    "run_frames",
    "disassemble",
    "watch_register",
    "set_trace",
    "get_trace",
    "call_stack",
    "pause",
    "resume",
];

const MEMORY_REGIONS: &[MemoryRegion] = &[
    MemoryRegion {
        name: "cpu",
        base: 0x00000,
        size: 0x100000,
    },
    MemoryRegion {
        name: "gvram_b",
        base: 0xA8000,
        size: 0x8000,
    },
    MemoryRegion {
        name: "gvram_g",
        base: 0xB8000,
        size: 0x8000,
    },
    MemoryRegion {
        name: "gvram_i",
        base: 0xE0000,
        size: 0x8000,
    },
    MemoryRegion {
        name: "gvram_r",
        base: 0xB0000,
        size: 0x8000,
    },
    MemoryRegion {
        name: "physical",
        base: 0x00000,
        size: 0x100000,
    },
    MemoryRegion {
        name: "ram",
        base: 0x00000,
        size: 0x100000,
    },
    MemoryRegion {
        name: "tvram",
        base: 0xA0000,
        size: 0x4000,
    },
];

const I386_REGS: &[&str] = &[
    "eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi", "eip", "eflags", "cs", "ss", "ds",
    "es", "fs", "gs",
];
const I86_REGS: &[&str] = &[
    "ax", "cx", "dx", "bx", "sp", "bp", "si", "di", "ip", "flags", "cs", "ss", "ds", "es",
];
const DUMP_REGION_NAMES: &[&str] = &["ram", "tvram", "gvram_b", "gvram_r", "gvram_g", "gvram_i"];
const PC98_INPUT_BUTTONS: &[&str] = &[
    "enter",
    "esc",
    "space",
    "up",
    "down",
    "left",
    "right",
    "backspace",
    "tab",
    "del",
    "ins",
    "home",
    "help",
    "stop",
    "copy",
    "shift",
    "ctrl",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "vf1",
    "vf2",
    "vf3",
    "vf4",
    "vf5",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
];

#[derive(Debug, Clone, Copy)]
struct MemoryRegion {
    name: &'static str,
    base: u32,
    size: u32,
}

#[derive(Debug, Clone)]
struct SnapshotSpec {
    memory_type: String,
    address: usize,
    length: usize,
}

#[derive(Debug, Clone)]
struct Breakpoint {
    kind: String,
    addr: Option<u64>,
    size: Option<u64>,
    backend: String,
    backend_id: u64,
    condition: Option<String>,
    snapshots: Vec<SnapshotSpec>,
    pause_on_hit: bool,
    register: Option<String>,
    state_key: Option<String>,
    min: Option<u64>,
    max: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("{0}")]
    BadParams(String),
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("{0}")]
    Emulator(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
}

type BridgeResult<T> = Result<T, BridgeError>;

mod transport;
pub use transport::{GdbRspClient, GdbTransport};

#[derive(Debug, Clone, Default)]
pub struct BridgeEnv {
    pub name: Option<String>,
    pub session_token: Option<String>,
    pub launch_id: Option<String>,
    pub content: Option<PathBuf>,
    pub build: Option<String>,
}

impl BridgeEnv {
    pub fn from_process_env() -> Self {
        Self {
            name: std::env::var("EMUCAP_NAME").ok(),
            session_token: std::env::var("EMUCAP_SESSION_TOKEN").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
            content: std::env::var_os("EMUCAP_CONTENT").map(PathBuf::from),
            build: std::env::var("EMUCAP_BUILD_HASH").ok(),
        }
    }
}

pub struct Bridge<G> {
    gdb: G,
    env: BridgeEnv,
    frozen: bool,
    tracing: bool,
    trace_path: Option<PathBuf>,
    events: Vec<Value>,
    bps: BTreeMap<u64, Breakpoint>,
    next_bp: u64,
    input_fields: Option<Vec<String>>,
    /// Count of trailing interrupt-echo stops that `interrupt()` left buffered and that `note_stop`
    /// must drop instead of surfacing as phantom poll_events. PC-98's own pause reports `S05` (same
    /// as a breakpoint), so signal number cannot distinguish it — this counter is the equivalent of
    /// the NDS bridge's `is_interrupt_stop` (which keys off SIGINT `S02`).
    pending_interrupt_stops: u32,
}

impl<G: GdbTransport> Bridge<G> {
    pub fn new(mut gdb: G, env: BridgeEnv) -> Self {
        let frozen = gdb.send("?").is_ok();
        Self {
            gdb,
            env,
            frozen,
            tracing: false,
            trace_path: None,
            events: Vec::new(),
            bps: BTreeMap::new(),
            next_bp: 1,
            input_fields: None,
            pending_interrupt_stops: 0,
        }
    }

    pub fn handle_request(&mut self, req: Request) -> Response {
        let id = req.id;
        let result = match req.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "read_memory" => self.read_memory(&req.params),
            "find_pattern" => self.find_pattern(&req.params),
            "dump_memory" => self.dump_memory(&req.params),
            "get_rom_info" => self.get_rom_info(),
            "write_memory" => self.write_memory(&req.params),
            "get_state" => self.get_state(),
            "save_state" => self.save_state(&req.params),
            "load_state" => self.load_state(&req.params),
            "probe" => self.probe(&req.params),
            "poll_events" => self.poll_events(&req.params),
            "set_breakpoint" => self.set_breakpoint(&req.params),
            "clear_breakpoint" => self.clear_breakpoint(&req.params),
            "list_breakpoints" => self.list_breakpoints(),
            "clear_all_breakpoints" => self.clear_all_breakpoints(),
            "screenshot" => self.screenshot(),
            "set_input" => self.set_input(&req.params),
            "press_buttons" => self.press_buttons(&req.params),
            "reset" => self.reset(),
            "break_on_reset" => self.break_on_reset(&req.params),
            "step" => self.step(&req.params),
            "step_instructions" => self.step_instructions(&req.params),
            "run_frames" => self.run_frames(&req.params),
            "disassemble" => self.disassemble(&req.params),
            "watch_register" => self.watch_register(&req.params),
            "set_trace" => self.set_trace(&req.params),
            "get_trace" => self.get_trace(&req.params),
            "call_stack" => self.call_stack(),
            "pause" => self.pause(),
            "resume" => self.resume(),
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

mod breakpoints;
mod debug_runtime;
mod execution;
mod machine;
mod service;
mod support;
use support::*;

#[cfg(test)]
#[path = "pc98_bridge_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pc98_bridge_temporal_tests.rs"]
mod temporal_tests;
