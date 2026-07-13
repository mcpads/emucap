use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

const MAX_READ_CHUNK: usize = 0x4000;
const MAX_FIND_LEN: usize = 128 * 1024;
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

pub trait GdbTransport {
    fn send(&mut self, payload: &str) -> BridgeResult<String>;
    fn send_no_reply(&mut self, payload: &str) -> BridgeResult<()>;
    fn interrupt(&mut self) -> BridgeResult<String>;
    /// 직전 write 없이 다음 RSP 패킷을 blocking으로 읽는다. `send_cmd`가 stale async stop을
    /// 실제 응답 앞에서 걷어낸 뒤 진짜 응답을 이어 읽을 때 쓴다.
    fn recv_reply(&mut self) -> BridgeResult<String> {
        Err(BridgeError::Emulator("recv_reply unsupported".into()))
    }
    fn get_timeout(&self) -> BridgeResult<Duration> {
        Ok(Duration::from_secs(5))
    }
    fn set_timeout(&mut self, _timeout: Duration) -> BridgeResult<()> {
        Ok(())
    }
    fn recv_nonblocking(&mut self) -> BridgeResult<Option<String>> {
        Ok(None)
    }
}

pub struct GdbRspClient {
    stream: TcpStream,
    buf: VecDeque<u8>,
}

impl GdbRspClient {
    pub fn connect(
        host: &str,
        port: u16,
        timeout: Duration,
        connect_wait: Duration,
    ) -> std::io::Result<Self> {
        let deadline = Instant::now() + connect_wait;
        loop {
            match TcpStream::connect((host, port)) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(timeout))?;
                    stream.set_write_timeout(Some(timeout))?;
                    return Ok(Self {
                        stream,
                        buf: VecDeque::new(),
                    });
                }
                Err(err) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(300));
                    if err.kind() == std::io::ErrorKind::InvalidInput {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn checksum(payload: &[u8]) -> u8 {
        payload.iter().fold(0u8, |sum, b| sum.wrapping_add(*b))
    }

    fn frame(payload: &str) -> Vec<u8> {
        let data = payload.as_bytes();
        let mut out = Vec::with_capacity(data.len() + 4);
        out.push(b'$');
        out.extend_from_slice(data);
        out.push(b'#');
        out.extend_from_slice(format!("{:02x}", Self::checksum(data)).as_bytes());
        out
    }

    fn read_byte(&mut self) -> std::io::Result<u8> {
        if let Some(b) = self.buf.pop_front() {
            return Ok(b);
        }
        let mut chunk = [0u8; 4096];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "GDB connection closed",
            ));
        }
        self.buf.extend(&chunk[..n]);
        Ok(self.buf.pop_front().expect("buffer was just filled"))
    }

    fn write_packet(&mut self, payload: &str) -> std::io::Result<()> {
        let frame = Self::frame(payload);
        self.stream.write_all(&frame)?;
        for _ in 0..8 {
            match self.read_byte()? {
                b'+' => return Ok(()),
                b'-' => self.stream.write_all(&frame)?,
                b'$' => {
                    self.buf.push_front(b'$');
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "GDB packet was not acknowledged",
        ))
    }

    fn read_packet(&mut self) -> std::io::Result<String> {
        while self.read_byte()? != b'$' {}

        let mut raw = Vec::new();
        loop {
            let b = self.read_byte()?;
            if b == b'#' {
                break;
            }
            raw.push(b);
        }
        let mut checksum = [0u8; 2];
        checksum[0] = self.read_byte()?;
        checksum[1] = self.read_byte()?;
        let expected = std::str::from_utf8(&checksum)
            .ok()
            .and_then(|s| u8::from_str_radix(s, 16).ok());
        if expected != Some(Self::checksum(&raw)) {
            let _ = self.stream.write_all(b"-");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB packet checksum mismatch",
            ));
        }
        self.stream.write_all(b"+")?;

        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'}' && i + 1 < raw.len() {
                out.push(raw[i + 1] ^ 0x20);
                i += 2;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

impl GdbTransport for GdbRspClient {
    fn send(&mut self, payload: &str) -> BridgeResult<String> {
        self.write_packet(payload)?;
        Ok(self.read_packet()?)
    }

    fn send_no_reply(&mut self, payload: &str) -> BridgeResult<()> {
        self.write_packet(payload)?;
        Ok(())
    }

    fn recv_reply(&mut self) -> BridgeResult<String> {
        Ok(self.read_packet()?)
    }

    fn interrupt(&mut self) -> BridgeResult<String> {
        self.stream.write_all(&[0x03])?;
        std::thread::sleep(Duration::from_millis(10));
        self.send("?")
    }

    fn get_timeout(&self) -> BridgeResult<Duration> {
        Ok(self
            .stream
            .read_timeout()?
            .unwrap_or(Duration::from_secs(5)))
    }

    fn set_timeout(&mut self, timeout: Duration) -> BridgeResult<()> {
        self.stream.set_read_timeout(Some(timeout))?;
        self.stream.set_write_timeout(Some(timeout))?;
        Ok(())
    }

    fn recv_nonblocking(&mut self) -> BridgeResult<Option<String>> {
        let previous = self.stream.read_timeout()?;
        self.stream.set_nonblocking(true)?;
        let read = {
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "GDB connection closed",
                )),
                Ok(n) => {
                    self.buf.extend(&chunk[..n]);
                    Ok(())
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
                Err(err) => Err(err),
            }
        };
        self.stream.set_nonblocking(false)?;
        self.stream.set_read_timeout(previous)?;
        read?;
        if !self.buf.iter().any(|b| *b == b'$') {
            return Ok(None);
        }
        Ok(Some(self.read_packet()?))
    }
}

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

    fn hello(&self) -> BridgeResult<Value> {
        let mut result = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "pc98",
            "adapter": "mame-pc98-rust-gdb",
            "backend": "lua-gdbstub",
            "debugger": true,
            "methods": METHODS,
            "memory_types": memory_type_names(),
            "region_sizes": region_sizes_json(),
            "capability_notes": {
                "backend": "lua-gdbstub",
                "rust_bridge": true,
                "implemented_methods": METHODS,
                "screenshot": true,
                "input": true,
                "frame_step": true,
                "step_units": ["frames", "instructions"],
                "breakpoints": true,
                "watch_register": true,
                "trace": true,
                "state_restore": state_restore_info(),
            },
            "input_buttons": input_buttons_json(),
        });
        let obj = result.as_object_mut().expect("hello is an object");
        if let Some(name) = &self.env.name {
            obj.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.env.session_token {
            obj.insert("session_token".into(), json!(token));
        }
        if let Some(launch_id) = &self.env.launch_id {
            obj.insert("launch_id".into(), json!(launch_id));
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

    fn status(&mut self) -> BridgeResult<Value> {
        self.drain_stop()?;
        let mut input_buttons = input_buttons_json();
        let available = self.refresh_input_fields();
        if let Some(obj) = input_buttons.as_object_mut() {
            obj.insert("available".into(), json!(available));
        }
        Ok(json!({
            "connected": true,
            "system": "pc98",
            "adapter": "mame-pc98-rust-gdb",
            "backend": "lua-gdbstub",
            "debugger": true,
            "frame": self.current_frame(),
            "state": if self.frozen { "frozen" } else { "running" },
            "memory_types": memory_type_names(),
            "capability_notes": {
                "backend": "lua-gdbstub",
                "rust_bridge": true,
                "implemented_methods": METHODS,
                "screenshot": true,
                "input": true,
                "frame_step": true,
                "step_units": ["frames", "instructions"],
                "breakpoints": true,
                "watch_register": true,
                "trace": true,
                "state_restore": state_restore_info(),
            },
            "input_buttons": input_buttons,
        }))
    }

    fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let length = required_num(params, "length")?;
        let address = region_address(params, length)?;
        let length = length as usize;
        let hex = self.read_abs_hex(address, length)?;
        Ok(json!({ "hex": hex }))
    }

    fn write_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let hexstr = required_str(params, "hex")?;
        if hexstr.len() % 2 != 0 {
            return Err(BridgeError::BadParams("hex must have even length".into()));
        }
        let data =
            hex::decode(hexstr).map_err(|_| BridgeError::BadParams("hex decode failed".into()))?;
        let size = data.len();
        let address = region_address(params, size as u64)?;
        let resp = self.send_cmd(&format!("M{address:x},{size:x}:{hexstr}"))?;
        if resp != "OK" {
            return Err(BridgeError::Emulator(format!(
                "GDB memory write failed: {resp}"
            )));
        }
        Ok(json!({ "written": size }))
    }

    fn find_pattern(&mut self, params: &Value) -> BridgeResult<Value> {
        let memory_type = params
            .get("memory_type")
            .and_then(Value::as_str)
            .unwrap_or("physical");
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let pattern = hex::decode(required_str(params, "hex")?)
            .map_err(|_| BridgeError::BadParams("hex decode failed".into()))?;
        if pattern.is_empty() {
            return Err(BridgeError::BadParams(
                "hex must contain at least one byte".into(),
            ));
        }

        let start = optional_num(params, "start")?.unwrap_or(0) as usize;
        let mut length = optional_num(params, "length")?
            .map(|v| v as usize)
            .unwrap_or_else(|| region.size.saturating_sub(start as u32) as usize);
        if start >= region.size as usize {
            length = 0;
        } else {
            length = length.min(region.size as usize - start);
        }
        let truncated_scan = length > MAX_FIND_LEN;
        let scan_len = length.min(MAX_FIND_LEN);
        let max_matches = optional_num(params, "max_matches")?
            .unwrap_or(256)
            .clamp(1, 4096) as usize;
        let align = optional_num(params, "align")?.unwrap_or(1).max(1) as usize;

        let buf = self.read_region_bytes(memory_type, start, scan_len)?;
        let mut matches = Vec::new();
        let mut truncated_matches = false;
        let mut pos = 0usize;
        while pos <= buf.len().saturating_sub(pattern.len()) {
            let Some(idx) = find_subslice(&buf[pos..], &pattern) else {
                break;
            };
            let off = start + pos + idx;
            if (off - start).is_multiple_of(align) {
                if matches.len() >= max_matches {
                    truncated_matches = true;
                    break;
                }
                matches.push(off);
            }
            pos += idx + 1;
        }

        Ok(json!({
            "matches": matches,
            "count": matches.len(),
            "truncated": truncated_scan || truncated_matches,
            "truncated_scan": truncated_scan,
            "truncated_matches": truncated_matches,
            "scanned": scan_len,
            "start": start,
        }))
    }

    fn dump_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        fs::create_dir_all(&path)?;
        let mut metas = Vec::new();
        for name in DUMP_REGION_NAMES {
            let region = memory_region(name).expect("dump region is declared");
            let out_path = path.join(format!("{name}.bin"));
            let mut file = File::create(out_path)?;
            let mut offset = 0usize;
            while offset < region.size as usize {
                let chunk = MAX_READ_CHUNK.min(region.size as usize - offset);
                file.write_all(&self.read_region_bytes(name, offset, chunk)?)?;
                offset += chunk;
            }
            metas.push(json!({
                "name": name,
                "memory_type": name,
                "base_address": region.base,
                "size": region.size,
            }));
        }
        let regions_path = path.join("regions.json");
        fs::write(&regions_path, serde_json::to_vec(&metas)?)?;
        Ok(json!({ "path": path.display().to_string(), "regions": metas.len() }))
    }

    fn get_state(&mut self) -> BridgeResult<Value> {
        let regs = self.read_regs_hex()?;
        Ok(json!({ "state": state_from_regs_hex(&regs) }))
    }

    fn save_state(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        let out_path = absolute_path(&path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.stop_for_state_restore()?;
        let regs_hex = self.read_regs_hex()?;
        let save_items_dir = unique_temp_dir("emucap_pc98_saveitems_")?;
        // Stage the zip to a sibling `.partial` and rename over out_path only once it is fully
        // written, so a mid-save failure (region read timeout, peer close, ENOSPC, kill) leaves any
        // pre-existing savestate byte-for-byte intact instead of truncating it. Mirrors the NDS
        // dump_memory and PPSSPP dump atomic-swap.
        let partial_path = state_partial_sibling(&out_path)?;
        let result = (|| {
            let mut save_items = self.save_lua_save_items(&save_items_dir)?;
            save_items.insert("dir".into(), json!(SAVE_ITEMS_DIR));
            let save_items_members = save_item_members(&save_items_dir)?;
            let file = File::create(&partial_path)?;
            let mut zip = ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            let mut regions = Vec::new();
            for name in DUMP_REGION_NAMES {
                let region = memory_region(name).expect("dump region is declared");
                let member = format!("{name}.bin");
                zip.start_file(&member, options)?;
                let mut offset = 0usize;
                while offset < region.size as usize {
                    let chunk = MAX_READ_CHUNK.min(region.size as usize - offset);
                    zip.write_all(&self.read_region_bytes(name, offset, chunk)?)?;
                    offset += chunk;
                }
                regions.push(json!({
                    "name": name,
                    "memory_type": name,
                    "base_address": region.base,
                    "size": region.size,
                    "file": member,
                }));
            }
            for (src_path, member) in save_items_members {
                zip.start_file(member, options)?;
                let mut file = File::open(src_path)?;
                std::io::copy(&mut file, &mut zip)?;
            }
            zip.start_file("state.json", options)?;
            let manifest = json!({
                "format": STATE_FORMAT,
                "system": "pc98",
                "adapter": "mame-pc98-rust-gdb",
                "registers_hex": regs_hex,
                "regions": regions,
                "save_items": save_items,
                "state_restore": state_restore_info(),
            });
            zip.write_all(&serde_json::to_vec(&manifest)?)?;
            zip.finish()?;
            fs::rename(&partial_path, &out_path)?;
            let bytes = out_path.metadata()?.len();
            Ok(json!({
                "path": path.display().to_string(),
                "format": STATE_FORMAT,
                "regions": regions.len(),
                "save_items": save_items,
                "bytes": bytes,
                "state_restore": state_restore_info(),
            }))
        })();
        let _ = fs::remove_dir_all(&save_items_dir);
        if result.is_err() {
            // The rename never ran, so out_path (any prior save) is untouched; drop the partial zip.
            let _ = fs::remove_file(&partial_path);
        }
        result
    }

    fn load_state(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        if !path.is_file() {
            return Err(BridgeError::BadParams(format!(
                "save state not found: {}",
                path.display()
            )));
        }
        self.stop_for_state_restore()?;
        let load_items_dir = unique_temp_dir("emucap_pc98_loaditems_")?;
        let result = (|| {
            let file = File::open(&path)?;
            let mut archive = ZipArchive::new(file)?;
            let manifest = read_state_manifest(&mut archive)?;
            let state_format = state_format(&manifest)?;
            let save_items_dir = extract_save_items(&mut archive, &manifest, &load_items_dir)?;
            let regions = read_state_regions(&mut archive, &manifest)?;
            let regs_hex = manifest
                .get("registers_hex")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            drop(archive);
            let save_items_result = match save_items_dir {
                Some(dir) => self.load_lua_save_items(&dir)?,
                None => serde_json::Map::new(),
            };
            self.write_state_regions(&regions)?;
            let mut restore_result = serde_json::Map::new();
            restore_result.insert("restore_strategy".into(), json!("memory_only"));
            restore_result.insert("post_restore_instruction_exact".into(), json!(true));
            if !regs_hex.is_empty() {
                restore_result = self.restore_regs_after_state_load(&regs_hex)?;
            }
            self.frozen = true;
            let mut out = serde_json::Map::new();
            out.insert("path".into(), json!(path.display().to_string()));
            out.insert("format".into(), json!(state_format));
            out.insert("regions".into(), json!(regions.len()));
            out.insert("state_restore".into(), state_restore_info());
            out.extend(save_items_result);
            out.extend(restore_result);
            Ok(Value::Object(out))
        })();
        let _ = fs::remove_dir_all(&load_items_dir);
        result
    }

    fn probe(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "state")?);
        if !path.is_file() {
            return Err(BridgeError::BadParams(format!(
                "save state not found: {}",
                path.display()
            )));
        }
        let frame = match optional_num(params, "frame")? {
            Some(frame) => frame,
            None => optional_num(params, "frames")?.unwrap_or(0),
        };
        let memory_type = params
            .get("memory_type")
            .and_then(Value::as_str)
            .unwrap_or("physical");
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let address = region.base as u64 + required_num(params, "address")?;
        let length = required_num(params, "length")? as usize;
        self.stop_for_state_restore()?;
        let load_items_dir = unique_temp_dir("emucap_pc98_probeitems_")?;
        let result = (|| {
            let file = File::open(&path)?;
            let mut archive = ZipArchive::new(file)?;
            let manifest = read_state_manifest(&mut archive)?;
            let _ = state_format(&manifest)?;
            let save_items_dir = extract_save_items(&mut archive, &manifest, &load_items_dir)?;
            let regions = read_state_regions(&mut archive, &manifest)?;
            let regs_hex = manifest
                .get("registers_hex")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    BridgeError::BadParams("PC-98 probe state is missing registers_hex".into())
                })?
                .to_string();
            drop(archive);
            let save_items_result = match save_items_dir {
                Some(dir) => self.load_lua_save_items(&dir)?,
                None => serde_json::Map::new(),
            };
            self.write_state_regions(&regions)?;
            let mut result = self.register_probe(&regs_hex, frame, address, length)?;
            if let Some(obj) = result.as_object_mut() {
                obj.extend(save_items_result);
            }
            self.frozen = true;
            Ok(result)
        })();
        let _ = fs::remove_dir_all(&load_items_dir);
        result
    }

    fn set_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("exec")
            .to_string();
        let zkind = match kind.as_str() {
            "exec" => "0",
            "write" => "2",
            "read" => "3",
            "access" => "4",
            _ => {
                return Err(BridgeError::BadParams(
                    "MAME PC-98 supports exec/read/write/access breakpoints".into(),
                ))
            }
        };
        let memory_type = params
            .get("memory_type")
            .and_then(Value::as_str)
            .unwrap_or("physical");
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let start = required_num(params, "start")?;
        // `end`는 포함(inclusive) region 오프셋이며, 없으면 start 한 단위다. start보다 작으면 1바이트로 접는다.
        let end = optional_num(params, "end")?.unwrap_or(start).max(start);
        let size = end - start + 1;
        // [start, start+size)가 선택된 region 안이어야 한다 — 유한 region(ram·tvram 등) 밖 offset을
        // region.base로 감싸 MAME setpoint에 넘기면 절대 안 맞을 BP가 조용히 서므로 거부한다
        // (nds_bridge route()의 범위 가드와 동형).
        if !matches!(start.checked_add(size), Some(last) if last <= region.size as u64) {
            return Err(BridgeError::BadParams(format!(
                "{memory_type} breakpoint out of range: offset {start:#x}+{size:#x} exceeds region size {region_size:#x}",
                region_size = region.size
            )));
        }
        let addr = region.base as u64 + start;
        let snapshots = parse_snapshot_specs(params.get("snapshot"))?;
        let condition = breakpoint_condition(params, &kind)?;
        let pause_on_hit = params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let spec = format!(
            "{zkind}|{addr:x}|{size:x}|{}|{condition}",
            if pause_on_hit { 1 } else { 0 }
        );
        let resp = self.lua_cmd_reply("setpoint", Some(&spec))?;
        let (backend, backend_id) = parse_breakpoint_reply(&resp)?;
        let id = self.next_bp;
        self.next_bp += 1;
        self.bps.insert(
            id,
            Breakpoint {
                kind,
                addr: Some(addr),
                size: Some(size),
                backend,
                backend_id,
                condition: empty_to_none(condition),
                snapshots,
                pause_on_hit,
                register: None,
                state_key: None,
                min: None,
                max: None,
            },
        );
        Ok(json!({ "id": id }))
    }

    fn watch_register(&mut self, params: &Value) -> BridgeResult<Value> {
        let raw_reg = params
            .get("register")
            .and_then(Value::as_str)
            .unwrap_or("sp");
        let (expr_reg, state_key) = normalize_debug_register(raw_reg)?;
        let min = optional_num(params, "min")?.unwrap_or(0);
        let max = optional_num(params, "max")?.unwrap_or(0xFFFF_FFFF);
        if min > max {
            return Err(BridgeError::BadParams("min must be <= max".into()));
        }
        let condition = format!("({expr_reg} < {min:X}) || ({expr_reg} > {max:X})");
        let pause_on_hit = params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let spec = format!("{}|{condition}", if pause_on_hit { 1 } else { 0 });
        let resp = self.lua_cmd_reply("setregpoint", Some(&spec))?;
        let backend_id = parse_regpoint_reply(&resp)?;
        let id = self.next_bp;
        self.next_bp += 1;
        self.bps.insert(
            id,
            Breakpoint {
                kind: "reg".into(),
                addr: None,
                size: None,
                backend: "rp".into(),
                backend_id,
                condition: Some(condition),
                snapshots: Vec::new(),
                pause_on_hit,
                register: Some(raw_reg.to_string()),
                state_key: Some(state_key.into()),
                min: Some(min),
                max: Some(max),
            },
        );
        Ok(json!({ "id": id }))
    }

    fn clear_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let id = required_num(params, "id")?;
        let bp = self
            .bps
            .get(&id)
            .cloned()
            .ok_or_else(|| BridgeError::BadParams(format!("unknown breakpoint id: {id}")))?;
        let spec = format!("{}|{}", bp.backend, bp.backend_id);
        let resp = self.lua_cmd_raw("clearpoint", Some(&spec))?;
        if resp != "OK" && resp != "E00" {
            return Err(BridgeError::Emulator(format!(
                "MAME breakpoint clear failed: {resp}"
            )));
        }
        self.bps.remove(&id);
        Ok(json!({ "cleared": id }))
    }

    fn list_breakpoints(&self) -> BridgeResult<Value> {
        let mut rows = Vec::new();
        for (id, bp) in &self.bps {
            if bp.kind == "reg" {
                rows.push(json!({
                    "id": id,
                    "kind": "reg",
                    "register": bp.register.clone(),
                    "min": bp.min,
                    "max": bp.max,
                    "condition": bp.condition.clone(),
                }));
            } else {
                let start = bp.addr.unwrap_or(0);
                let size = bp.size.unwrap_or(1);
                rows.push(json!({
                    "id": id,
                    "kind": bp.kind.clone(),
                    "start": start,
                    "end": start + size.saturating_sub(1),
                    "condition": bp.condition.clone(),
                }));
            }
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

    fn poll_events(&mut self, params: &Value) -> BridgeResult<Value> {
        self.drain_stop()?;
        let saw_reset = self.drain_reset_event()?;
        let filter_id = optional_num(params, "breakpoint_id")?;
        let mut events = Vec::new();
        let mut remaining = Vec::new();
        for mut event in std::mem::take(&mut self.events) {
            if saw_reset
                && event.get("type").and_then(Value::as_str) == Some("stop")
                && event.get("raw").and_then(Value::as_str) == Some("S05")
            {
                continue;
            }
            self.enrich_event(&mut event);
            if let Some(obj) = event.as_object_mut() {
                obj.remove("_pc98_enriched");
            }
            if let Some(filter_id) = filter_id {
                if event.get("id").and_then(Value::as_u64) != Some(filter_id) {
                    remaining.push(event);
                    continue;
                }
            }
            events.push(event);
        }
        self.events = remaining;
        Ok(json!({ "events": events, "dropped": 0 }))
    }

    fn get_rom_info(&self) -> BridgeResult<Value> {
        let content = self.env.content.as_ref().ok_or_else(|| {
            BridgeError::BadParams("EMUCAP_CONTENT is not set for get_rom_info".into())
        })?;
        if !content.is_file() {
            return Err(BridgeError::BadParams(format!(
                "content image not found: {}",
                content.display()
            )));
        }
        Ok(json!({
            "system": "pc98",
            "adapter": "mame-pc98-rust-gdb",
            "name": content.file_name().and_then(|s| s.to_str()).unwrap_or(""),
            "path": absolute_display(content),
            "sha1": sha1_file(content)?,
            "size": content.metadata()?.len(),
            "media_type": content.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase(),
        }))
    }

    fn pause(&mut self) -> BridgeResult<Value> {
        if !self.frozen {
            // interrupt() = 0x03 브레이크 + `?` → 스텁이 두 개의 stop(S05)을 보낸다. interrupt()가 하나를
            // 반환값으로 소비하고 나머지 하나(우리가 만든 인터럽트 에코)가 버퍼에 남는다. pc98 에코는
            // S05라 signal로 실제 BP 히트와 구분 불가하므로, interrupt() 전에 버퍼의 실제 pending stop
            // (직전 pause_on_hit BP 등)을 먼저 이벤트 큐로 걷어낸다(step_instruction_count/frames_op와
            // 동일). 그러면 카운터가 억제할 대상은 인터럽트 자신의 에코 하나뿐이라, 앞서 버퍼된 실제
            // 히트가 카운트되어 드롭되지 않는다.
            self.drain_buffered_stops()?;
            let _ = self.gdb.interrupt()?;
            self.pending_interrupt_stops = self.pending_interrupt_stops.saturating_add(1);
            self.frozen = true;
        }
        Ok(json!({ "state": "frozen" }))
    }

    fn resume(&mut self) -> BridgeResult<Value> {
        if self.frozen {
            self.gdb.send_no_reply("c")?;
            self.frozen = false;
        }
        Ok(json!({ "state": "running" }))
    }

    fn screenshot(&mut self) -> BridgeResult<Value> {
        let state = if self.frozen { "frozen" } else { "running" };
        let frame_before = self.current_frame();
        let path = std::env::temp_dir().join(format!(
            "emucap_pc98_{}_{}.png",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let result = (|| {
            self.lua_cmd("snapshot", Some(path.to_string_lossy().as_ref()))?;
            let data = fs::read(&path)?;
            if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(BridgeError::Emulator(
                    "MAME snapshot did not produce a PNG".into(),
                ));
            }
            let frame_after = self.current_frame();
            let frame_stable = frame_before.is_some() && frame_before == frame_after;
            let mut hasher = Sha256::new();
            hasher.update(&data);
            Ok(json!({
                "png_base64": base64::engine::general_purpose::STANDARD.encode(&data),
                "sha256": format!("{:x}", hasher.finalize()),
                "byte_len": data.len(),
                "state": state,
                "frame_before": frame_before,
                "frame_after": frame_after,
                "frame_stable": frame_stable,
                "freshness": "unverified",
                "frame_binding": "unverified",
            }))
        })();
        let _ = fs::remove_file(&path);
        result
    }

    fn set_input(&mut self, params: &Value) -> BridgeResult<Value> {
        let buttons = normalize_buttons(params.get("buttons"))?;
        if let Err(err) = self.lua_cmd("setinput", Some(&buttons.join(","))) {
            return Err(self.explain_input_failure(err, &buttons));
        }
        Ok(json!({ "buttons": buttons }))
    }

    fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        let buttons = normalize_buttons(params.get("buttons"))?;
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        let arg = format!("{frames}:{}", buttons.join(","));
        let stop = match self.deferred_lua_op("press", &arg, frames) {
            Ok(stop) => stop,
            Err(err) => return Err(self.explain_input_failure(err, &buttons)),
        };
        if let Some(raw) = stop {
            self.frozen = true;
            return Ok(json!({
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": raw,
                "buttons": buttons,
                "frames": frames,
                "frame": self.current_frame(),
            }));
        }
        self.frozen = false;
        Ok(json!({
            "status": "completed",
            "buttons": buttons,
            "frames": frames,
            "frame": self.current_frame(),
            "state": "running",
        }))
    }

    fn refresh_input_fields(&mut self) -> Vec<String> {
        // 머신 ioport에 실제 등록된 키 필드를 조회한다. 버튼 이름은 균일 매핑을 유지하고,
        // 가용성만 머신별로 다르므로 status/에러가 이 목록을 정본으로 노출한다. 구 plugin은
        // 이 쿼리를 몰라 빈 응답→Err이니 빈 목록으로 폴백한다(비-non-empty만 캐시).
        if let Some(cached) = &self.input_fields {
            return cached.clone();
        }
        let fields = self
            .lua_cmd_reply("inputfields", None)
            .ok()
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !fields.is_empty() {
            self.input_fields = Some(fields.clone());
        }
        fields
    }

    fn explain_input_failure(&mut self, err: BridgeError, buttons: &[String]) -> BridgeError {
        // E08 = 이 머신 ioport에 등록되지 않은 키. 어느 버튼이 없고 무엇이 가능한지 이름을 붙여
        // 돌려준다(맨몸 E08 패스스루 금지). plugin이 E08:<key>로 미해결 키를 보고하면 그걸 쓰고,
        // 아니면 가용 목록과 대조해 유추한다.
        let msg = err.to_string();
        let Some(idx) = msg.find("E08") else {
            return err;
        };
        let reported = msg[idx + 3..]
            .trim_start_matches(':')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        let available = self.refresh_input_fields();
        let unavailable: Vec<String> = if !reported.is_empty() {
            vec![reported]
        } else {
            buttons
                .iter()
                .filter(|b| !available.iter().any(|a| a == *b))
                .cloned()
                .collect()
        };
        let avail_str = if available.is_empty() {
            "(unknown; plugin does not report input fields)".to_string()
        } else {
            available.join(", ")
        };
        BridgeError::Emulator(format!(
            "PC-98 key(s) not registered on this machine: {}; available: {}",
            unavailable.join(", "),
            avail_str
        ))
    }

    fn reset(&mut self) -> BridgeResult<Value> {
        self.lua_cmd("reset", None)?;
        Ok(json!({ "reset": "scheduled" }))
    }

    fn break_on_reset(&mut self, params: &Value) -> BridgeResult<Value> {
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.lua_cmd("breakonreset", Some(if enabled { "1" } else { "0" }))?;
        Ok(json!({
            "enabled": enabled,
            "system": "pc98",
            "mode": "machine_reset_notifier",
        }))
    }

    fn step(&mut self, params: &Value) -> BridgeResult<Value> {
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        let unit = params
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("frames");
        if unit == "instructions" {
            return self.step_instruction_count(frames);
        }
        if unit != "frames" {
            return Err(BridgeError::BadParams(format!(
                "unsupported PC-98 step unit: {unit}"
            )));
        }
        let stop = self.frames_op("framestep", frames)?;
        self.frozen = true;
        if let Some(raw) = stop {
            return Ok(json!({
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": raw,
                "frame": self.current_frame(),
            }));
        }
        Ok(json!({
            "status": "completed",
            "unit": "frames",
            "frames": frames,
            "frame": self.current_frame(),
        }))
    }

    fn step_instructions(&mut self, params: &Value) -> BridgeResult<Value> {
        let count = match optional_num(params, "count")? {
            Some(count) => count,
            None => optional_num(params, "frames")?.unwrap_or(1),
        }
        .max(1);
        self.step_instruction_count(count)
    }

    fn run_frames(&mut self, params: &Value) -> BridgeResult<Value> {
        let frames = match optional_num(params, "n")? {
            Some(frames) => frames,
            None => optional_num(params, "frames")?.unwrap_or(1),
        }
        .max(1);
        let stop = self.frames_op("runframes", frames)?;
        if let Some(raw) = stop {
            self.frozen = true;
            return Ok(json!({
                "status": "interrupted",
                "reason": "breakpoint",
                "raw": raw,
                "frame": self.current_frame(),
            }));
        }
        self.frozen = false;
        Ok(json!({
            "status": "completed",
            "frames": frames,
            "frame": self.current_frame(),
            "state": "running",
        }))
    }

    fn disassemble(&mut self, params: &Value) -> BridgeResult<Value> {
        let address = required_num(params, "address")?;
        let count = optional_num(params, "count")?.unwrap_or(8).clamp(1, 256) as usize;
        let byte_len = (count * 16).max(16);
        let path = std::env::temp_dir().join(format!(
            "emucap_pc98_dasm_{}_{}.txt",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let result = {
            let spec = format!("{}|{address:x}|{byte_len:x}", path.to_string_lossy());
            match self.lua_cmd("dasm", Some(&spec)) {
                Ok(_) => match fs::read(&path) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let instructions = parse_dasm_lines(text.lines(), count);
                        if instructions.is_empty() {
                            Err(BridgeError::Emulator(
                                "MAME disassemble produced no instructions".into(),
                            ))
                        } else {
                            Ok(json!({ "instructions": instructions }))
                        }
                    }
                    Err(err) => Err(BridgeError::Io(err)),
                },
                Err(err) => Err(err),
            }
        };
        let _ = fs::remove_file(&path);
        result
    }

    fn set_trace(&mut self, params: &Value) -> BridgeResult<Value> {
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if enabled {
            let path = match &self.trace_path {
                Some(path) => {
                    let _ = fs::remove_file(path);
                    path.clone()
                }
                None => {
                    let path = std::env::temp_dir().join(format!(
                        "emucap_pc98_trace_{}_{}.log",
                        std::process::id(),
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or_default()
                    ));
                    self.trace_path = Some(path.clone());
                    path
                }
            };
            self.lua_cmd("tracestart", Some(path.to_string_lossy().as_ref()))?;
            self.tracing = true;
            return Ok(json!({ "tracing": true, "path": path.display().to_string() }));
        }
        if self.tracing {
            self.lua_cmd("traceflush", None)?;
            self.lua_cmd("tracestop", None)?;
        }
        self.tracing = false;
        Ok(json!({
            "tracing": false,
            "path": self.trace_path.as_ref().map(|p| p.display().to_string()),
        }))
    }

    fn get_trace(&mut self, params: &Value) -> BridgeResult<Value> {
        let count = optional_num(params, "count")?
            .unwrap_or(64)
            .clamp(1, TRACE_CAP as u64) as usize;
        let rows = self.read_trace_rows()?;
        let start = rows.len().saturating_sub(count);
        Ok(json!({
            "trace": rows[start..].to_vec(),
            "tracing": self.tracing,
            "total": rows.len(),
            "path": self.trace_path.as_ref().map(|p| p.display().to_string()),
        }))
    }

    fn call_stack(&mut self) -> BridgeResult<Value> {
        // 트레이싱 중이면 call/ret 트레이스 스캔이 정확하니 그대로 쓴다. 아니면 정지 상태의
        // BP(EBP) 체인을 걸어 트레이스 없이 복원한다 — method 필드로 호출자가 신뢰도를 판단한다.
        if self.tracing {
            self.call_stack_from_trace()
        } else {
            self.call_stack_from_frame_pointer()
        }
    }

    fn call_stack_from_trace(&mut self) -> BridgeResult<Value> {
        let rows = self.read_trace_rows()?;
        let mut stack = Vec::new();
        let mut frames = Vec::new();
        for row in &rows {
            let text = row
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let pc = row.get("pc").and_then(Value::as_u64);
            if text.starts_with("call") {
                if let Some(pc) = pc {
                    stack.push(pc);
                    frames.push(json!({ "pc": pc, "text": row.get("text").cloned().unwrap_or(Value::String(String::new())) }));
                }
            } else if text.starts_with("ret") && !stack.is_empty() {
                stack.pop();
                frames.pop();
            }
        }
        Ok(json!({
            "call_stack": stack,
            "frames": frames,
            "depth": stack.len(),
            "method": "trace",
            "tracing": self.tracing,
            "total": rows.len(),
        }))
    }

    fn call_stack_from_frame_pointer(&mut self) -> BridgeResult<Value> {
        // 표준 BP 프롤로그(push bp; mov bp,sp)를 가정한다 — 모든 루틴이 이를 지키진 않으므로
        // method="frame_pointer"로 알려 호출자가 신뢰도를 판단하게 한다.
        let state = state_from_regs_hex(&self.read_regs_hex()?);
        let get = |name: &str| state.get(name).and_then(Value::as_u64).unwrap_or(0);
        let (ebp, esp, eip, ss) = (
            get("cpu.ebp"),
            get("cpu.esp"),
            get("cpu.eip"),
            get("cpu.ss"),
        );
        // CR0.PE는 RSP 레지스터 셋에 없다. 값 크기로 real16 vs protected32를 추정한다(caveat:
        // 라이브 검증 필요). 모두 16비트 안이면 real16, 아니면 32비트 평면으로 본다.
        let real_mode = ebp <= 0xFFFF && esp <= 0xFFFF && eip <= 0xFFFF;
        let (ptr_size, seg_base, bp_mask) = if real_mode {
            (2usize, ss << 4, 0xFFFFu64)
        } else {
            (4usize, 0u64, 0xFFFF_FFFFu64)
        };
        let mut bp = ebp & bp_mask;
        let mut stack = Vec::new();
        let mut frames = Vec::new();
        for _ in 0..64 {
            if bp == 0 {
                break;
            }
            let base = seg_base.wrapping_add(bp);
            // 1MB+A20 상한을 넘는 주소는 무효로 보고 멈춘다.
            if base.saturating_add(2 * ptr_size as u64) > 0x0011_0000 {
                break;
            }
            let Some(saved_bp) = self.read_ptr_le(base, ptr_size) else {
                break;
            };
            let Some(ret_addr) = self.read_ptr_le(base + ptr_size as u64, ptr_size) else {
                break;
            };
            stack.push(ret_addr);
            frames.push(json!({ "pc": ret_addr, "frame_pointer": bp }));
            if saved_bp <= bp {
                // 비-증가/무효 bp → 프레임 체인 종료.
                break;
            }
            bp = saved_bp & bp_mask;
        }
        Ok(json!({
            "call_stack": stack,
            "frames": frames,
            "depth": stack.len(),
            "method": "frame_pointer",
            "mode": if real_mode { "real16" } else { "protected32" },
            "pointer_size": ptr_size,
            "frame_pointer": ebp & bp_mask,
            "tracing": self.tracing,
        }))
    }

    fn read_ptr_le(&mut self, address: u64, size: usize) -> Option<u64> {
        let hex = self.read_abs_hex(address, size).ok()?;
        little_hex_to_u64(&hex)
    }

    fn step_instruction_count(&mut self, count: u64) -> BridgeResult<Value> {
        for _ in 0..count {
            // s는 정상 응답 자체가 stop이라 send_cmd의 demux(command_expects_stop 아닌 명령만)가
            // 스킵된다. 그래서 s 앞에 낀 stale async stop(직전 framestep/BP 히트)은 send_cmd로도
            // 안 걷혀 s의 응답 자리에 오배달되고, 스텝이 실제로 안 돌고도 완료로 오인돼 off-by-one
            // 디싱크가 남는다. 스텝 전에 버퍼의 stale stop을 이벤트 큐로 걷어낸 뒤(=note_stop) s를
            // send_cmd로 보내 진짜 스텝 완료 stop을 응답으로 받는다(=re-read).
            self.drain_buffered_stops()?;
            let resp = self.send_cmd("s")?;
            if resp.starts_with('E') {
                return Err(BridgeError::Emulator(format!(
                    "GDB instruction step failed: {resp}"
                )));
            }
            if !is_stop_packet(&resp) {
                return Err(BridgeError::Emulator(format!(
                    "GDB instruction step returned unexpected response: {resp}"
                )));
            }
        }
        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "count": count,
        }))
    }

    fn stop_for_state_restore(&mut self) -> BridgeResult<()> {
        self.lua_cmd("stop", None)?;
        self.frozen = true;
        Ok(())
    }

    fn save_lua_save_items(&mut self, path: &Path) -> BridgeResult<serde_json::Map<String, Value>> {
        fs::create_dir_all(path)?;
        let resp = self.lua_cmd_reply("saveitems", Some(path.to_string_lossy().as_ref()))?;
        parse_save_items_response(&resp, "saveitems")
    }

    fn load_lua_save_items(&mut self, path: &Path) -> BridgeResult<serde_json::Map<String, Value>> {
        let parsed = parse_save_items_response(
            &self.lua_cmd_reply("loaditems", Some(path.to_string_lossy().as_ref()))?,
            "loaditems",
        )?;
        let mut out = serde_json::Map::new();
        out.insert(
            "save_items_restored".into(),
            parsed
                .get("items")
                .cloned()
                .unwrap_or_else(|| Value::Number(0.into())),
        );
        out.insert(
            "save_items_skipped".into(),
            parsed
                .get("skipped")
                .cloned()
                .unwrap_or_else(|| Value::Number(0.into())),
        );
        Ok(out)
    }

    fn write_state_regions(&mut self, regions: &[(String, Vec<u8>)]) -> BridgeResult<()> {
        for (memory_type, data) in regions {
            self.write_region_bytes(memory_type, 0, data)?;
        }
        Ok(())
    }

    fn write_region_bytes(
        &mut self,
        memory_type: &str,
        start: usize,
        data: &[u8],
    ) -> BridgeResult<()> {
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk = MAX_READ_CHUNK.min(data.len() - offset);
            let hex = hex::encode(&data[offset..offset + chunk]);
            let address = region.base as u64 + start as u64 + offset as u64;
            let resp = self.send_cmd(&format!("M{address:x},{chunk:x}:{hex}"))?;
            if resp != "OK" {
                return Err(BridgeError::Emulator(format!(
                    "GDB memory write failed: {resp}"
                )));
            }
            offset += chunk;
        }
        Ok(())
    }

    fn restore_regs_after_state_load(
        &mut self,
        regs_hex: &str,
    ) -> BridgeResult<serde_json::Map<String, Value>> {
        let current = self.load_regs_via_lua(regs_hex)?;
        self.frozen = true;
        let target = state_from_regs_hex(regs_hex);
        let exact = state_matches_real_mode_pc(&current, &target);
        let mut out = serde_json::Map::new();
        out.insert("restore_strategy".into(), json!("lua_register_load_hold"));
        out.insert("post_restore_instruction_exact".into(), json!(exact));
        out.insert(
            "observed_register_packet_matches_target".into(),
            json!(exact),
        );
        for key in ["cpu.pc", "cpu.eip", "cpu.cs"] {
            if let Some(value) = current.get(key).cloned() {
                let out_key = match key {
                    "cpu.pc" => "observed_pc",
                    "cpu.eip" => "observed_eip",
                    "cpu.cs" => "observed_cs",
                    _ => key,
                };
                out.insert(out_key.into(), value);
            }
        }
        Ok(out)
    }

    fn load_regs_via_lua(&mut self, regs_hex: &str) -> BridgeResult<Value> {
        let resp = self.lua_cmd_reply("regload", Some(regs_hex))?;
        let Some(regs) = resp.strip_prefix("OK|") else {
            return Err(BridgeError::Emulator(format!(
                "MAME register load failed: {resp}"
            )));
        };
        Ok(state_from_regs_hex(regs))
    }

    fn register_probe(
        &mut self,
        regs_hex: &str,
        frames: u64,
        address: u64,
        length: usize,
    ) -> BridgeResult<Value> {
        let spec = format!("{regs_hex}|{frames}|{address:x}|{length:x}");
        let resp = self.lua_cmd_reply("regprobe", Some(&spec))?;
        let result = parse_register_probe_response(&resp)?;
        let actual = result
            .get("hex")
            .and_then(Value::as_str)
            .map(str::len)
            .unwrap_or(0);
        if actual != length.saturating_mul(2) {
            return Err(BridgeError::Emulator(format!(
                "MAME register probe returned {} bytes, expected {length}",
                actual / 2
            )));
        }
        Ok(result)
    }

    fn read_abs_hex(&mut self, address: u64, length: usize) -> BridgeResult<String> {
        let mut out = String::with_capacity(length.saturating_mul(2));
        let mut offset = 0usize;
        while offset < length {
            let chunk = std::cmp::min(MAX_READ_CHUNK, length - offset);
            // send_cmd 경유로 demux한다(raw send 금지). m 응답 앞에 낀 stale async stop이 이
            // 읽기의 응답 자리에 오배달되면 stop 문자열이 hex로 디코드돼 실패하고 이후 요청이
            // 통째로 off-by-one 디싱크된다. m은 command_expects_stop이 아니라 send_cmd가 앞선
            // stop을 이벤트 큐로 걷어내고 진짜 hex 응답을 이어 읽는다.
            let resp = self.send_cmd_data(&format!("m{:x},{:x}", address + offset as u64, chunk))?;
            if resp.starts_with('E') {
                return Err(BridgeError::Emulator(format!(
                    "GDB memory read failed: {resp}"
                )));
            }
            out.push_str(&resp);
            offset += chunk;
        }
        Ok(out)
    }

    fn read_regs_hex(&mut self) -> BridgeResult<String> {
        let resp = self.send_cmd_data("g")?;
        if resp.starts_with('E') {
            return Err(BridgeError::Emulator(format!(
                "GDB register read failed: {resp}"
            )));
        }
        Ok(resp)
    }

    fn read_region_bytes(
        &mut self,
        memory_type: &str,
        start: usize,
        length: usize,
    ) -> BridgeResult<Vec<u8>> {
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let hex = self.read_abs_hex(region.base as u64 + start as u64, length)?;
        hex::decode(hex).map_err(|_| BridgeError::Emulator("GDB returned invalid hex".into()))
    }

    fn send_cmd(&mut self, payload: &str) -> BridgeResult<String> {
        // framestep/BP/WP 히트의 async stop이 drain 창 밖에서 도착하면 버퍼에 남아, 이 데이터
        // 명령의 응답 자리에 오배달돼 이후 요청/응답이 통째로 off-by-one 디싱크된다. stop을
        // 정상 응답으로 받는 명령이 아니면, 앞선 stale stop을 이벤트 큐로 걷어내고 진짜 응답을
        // 이어 읽는다.
        let mut resp = self.gdb.send(payload)?;
        if !command_expects_stop(payload) {
            while is_stop_packet(&resp) {
                self.note_stop(resp, false);
                resp = self.gdb.recv_reply()?;
            }
        }
        Ok(resp)
    }

    /// 데이터(hex/숫자) 응답을 기대하는 명령용 — send_cmd의 stale-stop demux에 더해 스트림에 낀 stale "OK"도
    /// 걷어낸다. 트레이싱 중 runframes의 frame-target이 pause_on_hit BP 히트와 겹치면 하나의 runframes에
    /// 완료 "OK"와 BP stop이 이중 응답되고, 브리지가 그중 하나를 소비하면 나머지(늦게 도착한 stale "OK")가
    /// 다음 데이터 명령(g 레지스터·m 메모리·qEmucap,frame·기타 lua_cmd_reply 읽기)의 응답 자리에 오배달돼
    /// off-by-one desync된다(get_state가 raw_register_bytes로 깨지고 이후 traceflush가 register 패킷을 받음).
    /// 데이터를 기대하는 호출자만 이 경로를 쓰고(호출자가 의도 선언), "OK"가 유효 응답인 명령(쓰기 M/G,
    /// lua_cmd)은 send_cmd를 그대로 써 정상 OK를 소비한다. 드레인 창 크기에 의존하지 않는 결정론적 재정렬.
    fn send_cmd_data(&mut self, payload: &str) -> BridgeResult<String> {
        debug_assert!(!command_expects_stop(payload));
        let mut resp = self.gdb.send(payload)?;
        // 데이터 응답 앞에 낀 stale stop(이벤트 큐로)과 stale "OK"(폐기)를 모두 걷어내고 진짜 응답을 읽는다.
        loop {
            if is_stop_packet(&resp) {
                self.note_stop(resp, false);
            } else if resp == "OK" {
                // stale 완료 OK — 이 데이터 명령의 유효 응답이 아니므로 폐기(이중 응답의 잔재).
            } else {
                return Ok(resp);
            }
            resp = self.gdb.recv_reply()?;
        }
    }

    fn lua_cmd(&mut self, name: &str, arg: Option<&str>) -> BridgeResult<String> {
        let mut payload = format!("qEmucap,{name}");
        if let Some(arg) = arg {
            payload.push(',');
            payload.push_str(&hex::encode(arg.as_bytes()));
        }
        let resp = self.send_cmd(&payload)?;
        if resp.is_empty() || resp.starts_with('E') {
            return Err(BridgeError::Emulator(format!(
                "MAME Lua command {name} failed: {resp}"
            )));
        }
        if resp != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME Lua command {name} failed: {resp}"
            )));
        }
        Ok(resp)
    }

    fn lua_cmd_reply(&mut self, name: &str, arg: Option<&str>) -> BridgeResult<String> {
        let resp = self.lua_cmd_raw(name, arg)?;
        if resp.is_empty() || resp.starts_with('E') {
            Err(BridgeError::Emulator(format!(
                "MAME Lua command {name} failed: {resp}"
            )))
        } else {
            Ok(resp)
        }
    }

    fn lua_cmd_raw(&mut self, name: &str, arg: Option<&str>) -> BridgeResult<String> {
        let mut payload = format!("qEmucap,{name}");
        if let Some(arg) = arg {
            payload.push(',');
            payload.push_str(&hex::encode(arg.as_bytes()));
        }
        // 주의: lua_cmd_reply 명령 중 clearpoint 등은 bare "OK"를 정상 반환하므로 여기서 send_cmd_data로
        // 드레인하면 안 된다(진짜 OK를 stale로 오인해 hang). stale-OK 드레인은 응답이 절대 bare "OK"가 아닌
        // 데이터 명령(g/m/qEmucap,frame)에서만 명시적으로 send_cmd_data로 한다.
        self.send_cmd(&payload)
    }

    fn current_frame(&mut self) -> Option<u64> {
        // frames_op(runframes/framestep) 직후 run_frames/step 핸들러가 필수로 호출한다 — 그 직전 이벤트
        // (BP 히트가 frame-target과 겹침)의 spurious bare "OK"가 이 frame 응답 자리에 오배달되면, 이후 g가
        // 프레임 10진수를 레지스터 hex로 오소비해 desync된다. frame 응답은 10진수(bare OK가 아님)라
        // send_cmd_data로 앞에 낀 stale bare "OK"를 걷어낸다.
        self.send_cmd_data("qEmucap,frame")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
    }

    fn frames_op(&mut self, name: &str, frames: u64) -> BridgeResult<Option<String>> {
        self.deferred_lua_op(name, &frames.to_string(), frames)
    }

    fn deferred_lua_op(
        &mut self,
        name: &str,
        arg: &str,
        budget_frames: u64,
    ) -> BridgeResult<Option<String>> {
        // s(step)와 마찬가지로 framestep/runframes도 응답 자체가 stop이라 send_cmd의 stale-stop demux가
        // command_expects_stop로 스킵된다. 직전 resume()가 pause_on_hit BP를 물어 남긴 버퍼된 stop이
        // 앞에 끼면 이 프레임 명령의 응답 자리에 오배달돼(drain_immediate_stops가 프레임 결과로 오소비)
        // 프레임을 안 돌리고도 interrupted+frozen로 오인되고 응답 스트림이 desync된다. step_instruction_count
        // 처럼 명령 전에 버퍼의 stale stop을 이벤트 큐로 걷어낸다.
        self.drain_buffered_stops()?;
        let previous = self.gdb.get_timeout()?;
        // 트레이싱 중이면 프레임마다 수십만 명령을 디스어셈+기록하므로 무트레이스 50ms/frame
        // 예산으론 타임아웃→지연 stop이 늦게 도착한다. 트레이스일 때 프레임당 예산을 크게 잡아
        // 지연 응답이 이 recv 창 안에서 매칭되게 한다.
        let per_frame_ms = if self.tracing { 5_000 } else { 50 };
        let timeout = Duration::from_millis(
            5_000u64
                .saturating_add(budget_frames.saturating_mul(per_frame_ms))
                .min(600_000),
        );
        self.gdb.set_timeout(timeout)?;
        let result = self.lua_cmd_allow_stop(name, Some(arg));
        let restore = self.gdb.set_timeout(previous);
        match (result, restore) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(err), Ok(())) => Err(err),
            (Ok(_), Err(err)) => Err(err),
            (Err(err), Err(_)) => Err(err),
        }
    }

    fn lua_cmd_allow_stop(
        &mut self,
        name: &str,
        arg: Option<&str>,
    ) -> BridgeResult<Option<String>> {
        let mut payload = format!("qEmucap,{name}");
        if let Some(arg) = arg {
            payload.push(',');
            payload.push_str(&hex::encode(arg.as_bytes()));
        }
        let resp = self.send_cmd(&payload)?;
        if resp == "OK" {
            return self.drain_immediate_stops();
        }
        if is_stop_packet(&resp) {
            self.note_stop(resp.clone(), false);
            let _ = self.drain_immediate_stops()?;
            return Ok(Some(resp));
        }
        Err(BridgeError::Emulator(format!(
            "MAME Lua command {name} failed: {resp}"
        )))
    }

    fn drain_stop(&mut self) -> BridgeResult<()> {
        if self.frozen {
            return Ok(());
        }
        if let Some(stop) = self.gdb.recv_nonblocking()? {
            if is_stop_packet(&stop) {
                self.note_stop(stop, false);
            }
        }
        Ok(())
    }

    /// 버퍼에 남은 stale async stop을 블로킹 없이 이벤트 큐로 걷어낸다. s처럼 응답 자체가
    /// stop인 명령(command_expects_stop)을 보내기 전에, 앞선 미소비 stop이 그 명령의 응답
    /// 자리에 오배달되는 걸 막는다. drain_stop은 frozen이면 조기 반환하지만 스텝 직전엔 frozen
    /// 중에도 직전 프레임 진행의 지연 stop이 남을 수 있어 별도로 항상 버퍼를 비운다.
    fn drain_buffered_stops(&mut self) -> BridgeResult<()> {
        while let Some(pkt) = self.gdb.recv_nonblocking()? {
            if is_stop_packet(&pkt) {
                self.note_stop(pkt, false);
            } else {
                break;
            }
        }
        Ok(())
    }

    fn drain_immediate_stops(&mut self) -> BridgeResult<Option<String>> {
        let mut first = None;
        for _ in 0..12 {
            match self.gdb.recv_nonblocking()? {
                Some(stop) if is_stop_packet(&stop) => {
                    // note_stop이 인터럽트 에코로 억제하면(true) 이 stop은 우리가 만든 에코일 뿐이므로
                    // 프레임 명령 결과(first)로 오소비하지 않는다.
                    let suppressed = self.note_stop(stop.clone(), false);
                    if !suppressed && first.is_none() {
                        first = Some(stop);
                    }
                }
                Some(_) => return Ok(first),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        Ok(first)
    }

    /// stop을 이벤트 큐에 넣는다. 단, 우리가 pause/interrupt로 주입한 인터럽트 에코 stop은 async
    /// 이벤트가 아니므로 큐에 넣으면 phantom stop으로 샌다 — interrupt()가 남긴 트레일링 stop 개수만큼
    /// 억제한다(pc98 인터럽트는 S05라 signal로 구분 불가 — NDS is_interrupt_stop(S02)에 상응하는 카운터
    /// 방식). 억제했으면 `true`를 반환해 호출부가 이 stop을 명령 결과로 오소비하지 않게 한다.
    fn note_stop(&mut self, stop: String, enrich: bool) -> bool {
        self.frozen = true;
        if self.pending_interrupt_stops > 0 && is_stop_packet(&stop) {
            self.pending_interrupt_stops -= 1;
            return true;
        }
        let mut event = stop_event(&stop);
        if enrich {
            self.enrich_event(&mut event);
        }
        self.events.push(event);
        false
    }

    fn drain_reset_event(&mut self) -> BridgeResult<bool> {
        let resp = self.lua_cmd_reply("pollreset", None)?;
        if resp == "NONE" {
            return Ok(false);
        }
        let Some(rest) = resp.strip_prefix("RESET:") else {
            return Err(BridgeError::Emulator(format!(
                "MAME reset poll failed: {resp}"
            )));
        };
        let (pc_hex, regs_hex) = rest.split_once('|').unwrap_or((rest, ""));
        let mut event = json!({ "type": "reset", "raw": resp });
        if let Some(pc) = little_hex_to_u64(pc_hex) {
            if let Some(obj) = event.as_object_mut() {
                obj.insert("pc".into(), json!(pc));
                obj.insert("address".into(), json!(pc));
            }
        } else if let Some(obj) = event.as_object_mut() {
            obj.insert("pc_error".into(), json!(pc_hex));
        }
        if !regs_hex.is_empty() {
            if let Some(obj) = event.as_object_mut() {
                obj.insert("regs".into(), state_from_regs_hex(regs_hex));
            }
        }
        self.events.push(event);
        Ok(true)
    }

    fn enrich_event(&mut self, event: &mut Value) {
        if event.get("_pc98_enriched").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match event_type.as_str() {
            "stop" => self.enrich_stop_event(event),
            "register_break" => self.enrich_register_event(event),
            "breakpoint_hit" => self.enrich_breakpoint_event(event),
            _ => mark_event_enriched(event),
        }
    }

    fn enrich_stop_event(&mut self, event: &mut Value) {
        self.ensure_event_regs(event);
        let mut pc_values = Vec::new();
        if let Some(regs) = event.get("regs") {
            for key in ["cpu.offset_pc", "cpu.pc"] {
                if let Some(value) = regs.get(key).and_then(Value::as_u64) {
                    pc_values.push(value);
                }
            }
            if event.get("pc").is_none() {
                if let Some(pc) = regs.get("cpu.pc").and_then(Value::as_u64) {
                    set_event_field(event, "pc", json!(pc));
                }
            }
        }
        for (id, bp) in &self.bps {
            if bp.kind == "exec" && bp.addr.is_some_and(|addr| pc_values.contains(&addr)) {
                set_event_field(event, "type", json!("breakpoint_hit"));
                set_event_field(event, "kind", json!("exec"));
                set_event_field(event, "address", json!(bp.addr.unwrap_or(0)));
                set_event_field(event, "id", json!(*id));
                set_event_field(event, "breakpoint_id", json!(*id));
                if bp.pause_on_hit {
                    self.frozen = true;
                }
                if !bp.snapshots.is_empty() && event.get("snapshot").is_none() {
                    let snapshots = bp.snapshots.clone();
                    match self.capture_snapshots(&snapshots) {
                        Ok(snapshot) => set_event_field(event, "snapshot", Value::Array(snapshot)),
                        Err(err) => {
                            set_event_field(event, "snapshot_error", json!(err.to_string()))
                        }
                    }
                }
                break;
            }
        }
        mark_event_enriched(event);
    }

    fn enrich_register_event(&mut self, event: &mut Value) {
        let matched = self.find_regwatch_for_event(event);
        if let Some((id, bp)) = &matched {
            set_event_field(event, "id", json!(*id));
            set_event_field(event, "breakpoint_id", json!(*id));
            set_event_field(event, "register", json!(bp.register.clone()));
            set_event_field(event, "min", json!(bp.min));
            set_event_field(event, "max", json!(bp.max));
            if bp.pause_on_hit {
                self.frozen = true;
            }
        }
        self.ensure_event_regs(event);
        let pc = event
            .get("regs")
            .and_then(|regs| regs.get("cpu.pc"))
            .and_then(Value::as_u64);
        let value = matched.as_ref().and_then(|(_, bp)| {
            bp.state_key.as_ref().and_then(|state_key| {
                event
                    .get("regs")
                    .and_then(|regs| regs.get(state_key))
                    .and_then(Value::as_u64)
            })
        });
        if event.get("pc").is_none() {
            if let Some(pc) = pc {
                set_event_field(event, "pc", json!(pc));
            }
        }
        if let Some(value) = value {
            set_event_field(event, "value", json!(value));
        }
        mark_event_enriched(event);
    }

    fn enrich_breakpoint_event(&mut self, event: &mut Value) {
        let matched = self.find_bp_for_event(event);
        if let Some((id, bp)) = &matched {
            set_event_field(event, "id", json!(*id));
            set_event_field(event, "breakpoint_id", json!(*id));
            if bp.pause_on_hit {
                self.frozen = true;
            }
        }
        self.ensure_event_regs(event);
        if let Some((_, bp)) = &matched {
            if !bp.snapshots.is_empty() && event.get("snapshot").is_none() {
                match self.capture_snapshots(&bp.snapshots) {
                    Ok(snapshot) => set_event_field(event, "snapshot", Value::Array(snapshot)),
                    Err(err) => set_event_field(event, "snapshot_error", json!(err.to_string())),
                }
            }
        }
        mark_event_enriched(event);
    }

    fn ensure_event_regs(&mut self, event: &mut Value) {
        if event.get("regs").is_some() {
            return;
        }
        match self.read_regs_hex() {
            Ok(regs) => set_event_field(event, "regs", state_from_regs_hex(&regs)),
            Err(err) => set_event_field(event, "regs_error", json!(err.to_string())),
        }
    }

    fn capture_snapshots(&mut self, specs: &[SnapshotSpec]) -> BridgeResult<Vec<Value>> {
        let mut out = Vec::new();
        for spec in specs {
            let bytes = self.read_region_bytes(&spec.memory_type, spec.address, spec.length)?;
            out.push(json!({
                "memory_type": spec.memory_type.clone(),
                "address": spec.address,
                "hex": hex::encode(bytes),
            }));
        }
        Ok(out)
    }

    fn find_bp_for_event(&self, event: &Value) -> Option<(u64, Breakpoint)> {
        if event.get("type").and_then(Value::as_str) != Some("breakpoint_hit") {
            return None;
        }
        let event_kind = event.get("kind").and_then(Value::as_str)?;
        let backend_id = event.get("backend_id").and_then(Value::as_u64);
        if let Some(backend_id) = backend_id {
            for (id, bp) in &self.bps {
                if bp.backend_id == backend_id && bp.kind == event_kind {
                    return Some((*id, bp.clone()));
                }
            }
        }
        let event_addr = event.get("address").and_then(Value::as_u64)?;
        for (id, bp) in &self.bps {
            let Some(start) = bp.addr else {
                continue;
            };
            let end = start + bp.size.unwrap_or(1).saturating_sub(1);
            if bp.kind == event_kind && start <= event_addr && event_addr <= end {
                return Some((*id, bp.clone()));
            }
        }
        None
    }

    fn find_regwatch_for_event(&self, event: &Value) -> Option<(u64, Breakpoint)> {
        if event.get("type").and_then(Value::as_str) != Some("register_break") {
            return None;
        }
        let backend_id = event.get("backend_id").and_then(Value::as_u64)?;
        for (id, bp) in &self.bps {
            if bp.kind == "reg" && bp.backend_id == backend_id {
                return Some((*id, bp.clone()));
            }
        }
        None
    }

    fn read_trace_rows(&mut self) -> BridgeResult<Vec<Value>> {
        if self.tracing {
            let _ = self.lua_cmd("traceflush", None);
        }
        let Some(path) = &self.trace_path else {
            return Ok(Vec::new());
        };
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(TRACE_CAP * 4);
        let mut rows = Vec::new();
        for line in &lines[start..] {
            if let Some(row) = parse_trace_line(line) {
                rows.push(row);
            }
        }
        let start = rows.len().saturating_sub(TRACE_CAP);
        Ok(rows[start..].to_vec())
    }
}

fn error_kind(err: &BridgeError) -> &'static str {
    match err {
        BridgeError::BadParams(_) => "bad_params",
        BridgeError::UnknownMethod(_) => "unknown_method",
        BridgeError::Emulator(_) => "emulator_error",
        BridgeError::Io(_) | BridgeError::Json(_) | BridgeError::Zip(_) => "bridge_error",
    }
}

fn memory_type_names() -> Vec<&'static str> {
    MEMORY_REGIONS.iter().map(|r| r.name).collect()
}

fn region_sizes_json() -> Value {
    let mut obj = serde_json::Map::new();
    for region in MEMORY_REGIONS {
        obj.insert(region.name.into(), json!(region.size));
    }
    Value::Object(obj)
}

fn memory_region(name: &str) -> Option<&'static MemoryRegion> {
    MEMORY_REGIONS.iter().find(|r| r.name == name)
}

fn region_address(params: &Value, length: u64) -> BridgeResult<u64> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("physical");
    let region = memory_region(memory_type)
        .ok_or_else(|| BridgeError::BadParams(format!("unsupported memory_type: {memory_type}")))?;
    let offset = required_num(params, "address")?;
    if !matches!(offset.checked_add(length), Some(end) if end <= region.size as u64) {
        return Err(BridgeError::BadParams(format!(
            "{memory_type} access out of range: offset {offset:#x}+{length:#x} exceeds region size {region_size:#x}",
            region_size = region.size
        )));
    }
    (region.base as u64).checked_add(offset).ok_or_else(|| {
        BridgeError::BadParams(format!(
            "{memory_type} address overflow at offset {offset:#x}"
        ))
    })
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

fn find_subslice(buf: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > buf.len() {
        return None;
    }
    buf.windows(needle.len()).position(|w| w == needle)
}

fn input_buttons_json() -> Value {
    json!({
        "system": "pc98",
        "buttons": PC98_INPUT_BUTTONS,
        "aliases": {
            "return": "enter",
            "return_key": "enter",
            "start": "enter",
            "escape": "esc",
            "select": "space",
            "delete": "del",
            "insert": "ins",
            "bksp": "backspace",
            "bs": "backspace",
        },
        "notes": "PC-98 uses keyboard inputs. Prefer enter/esc/space/up/down/left/right plus letter, digit, f1-f10, and vf1-vf5 keys.",
    })
}

fn normalize_buttons(raw: Option<&Value>) -> BridgeResult<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let Some(items) = raw.as_array() else {
        return Err(BridgeError::BadParams("buttons must be a list".into()));
    };
    items
        .iter()
        .map(|value| {
            let key = value
                .as_str()
                .map(|s| s.trim().to_ascii_lowercase())
                .unwrap_or_else(|| value.to_string().trim_matches('"').to_ascii_lowercase());
            let normalized = input_alias(&key).unwrap_or(&key);
            if PC98_INPUT_BUTTONS.contains(&normalized) {
                Ok(normalized.to_string())
            } else {
                Err(BridgeError::BadParams(format!(
                    "unsupported PC-98 key: {key}"
                )))
            }
        })
        .collect()
}

fn input_alias(key: &str) -> Option<&'static str> {
    match key {
        "return" | "return_key" | "start" => Some("enter"),
        "escape" => Some("esc"),
        "select" => Some("space"),
        "delete" => Some("del"),
        "insert" => Some("ins"),
        "bksp" | "bs" => Some("backspace"),
        _ => None,
    }
}

fn is_stop_packet(resp: &str) -> bool {
    resp.starts_with('S') || resp.starts_with('T')
}

/// 이 명령의 정상 RSP 응답 자체가 stop 패킷인 명령인지. continue/step/`?`/vCont 외에,
/// framestep·runframes·press는 프레임 노티파이어가 목표에 도달할 때 stop을 지연 응답으로 보내므로
/// 여기에 포함한다 — 이들 응답의 stop은 stale이 아니라 정상 응답이라 demux하면 안 된다.
fn command_expects_stop(payload: &str) -> bool {
    payload == "c"
        || payload == "s"
        || payload == "?"
        || payload.starts_with('C')
        || payload.starts_with('S')
        || payload.starts_with("vCont")
        || payload.starts_with("qEmucap,framestep")
        || payload.starts_with("qEmucap,runframes")
        || payload.starts_with("qEmucap,press")
}

fn parse_breakpoint_reply(resp: &str) -> BridgeResult<(String, u64)> {
    let (kind, id) = resp
        .split_once(':')
        .ok_or_else(|| BridgeError::Emulator(format!("MAME breakpoint set failed: {resp}")))?;
    let backend = match kind {
        "BP" => "bp",
        "WP" => "wp",
        _ => {
            return Err(BridgeError::Emulator(format!(
                "MAME breakpoint set failed: {resp}"
            )))
        }
    };
    let id = id
        .parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("MAME breakpoint set failed: {resp}")))?;
    Ok((backend.into(), id))
}

fn parse_regpoint_reply(resp: &str) -> BridgeResult<u64> {
    let Some(id) = resp.strip_prefix("RP:") else {
        return Err(BridgeError::Emulator(format!(
            "MAME registerpoint set failed: {resp}"
        )));
    };
    id.parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("MAME registerpoint set failed: {resp}")))
}

fn empty_to_none(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn breakpoint_condition(params: &Value, kind: &str) -> BridgeResult<String> {
    let mut clauses = Vec::new();
    let pc_min = optional_num(params, "pc_min")?;
    let pc_max = optional_num(params, "pc_max")?;
    if let Some(pc_min) = pc_min {
        clauses.push(format!("pc >= {pc_min:X}"));
    }
    if let Some(pc_max) = pc_max {
        clauses.push(format!("pc <= {pc_max:X}"));
    }
    if let (Some(pc_min), Some(pc_max)) = (pc_min, pc_max) {
        if pc_min > pc_max {
            return Err(BridgeError::BadParams("pc_min must be <= pc_max".into()));
        }
    }

    let has_value_filter = params.get("value").is_some()
        || params.get("value_mask").is_some()
        || params.get("value_len").is_some();
    if has_value_filter {
        if kind != "read" && kind != "write" {
            return Err(BridgeError::BadParams(
                "value filters only apply to read/write breakpoints".into(),
            ));
        }
        let value = required_num(params, "value")?;
        let value_len = optional_num(params, "value_len")?.unwrap_or(1);
        if !(1..=4).contains(&value_len) {
            return Err(BridgeError::BadParams(
                "value_len must be 1..4 for MAME PC-98".into(),
            ));
        }
        let all_bits = (1u64 << (value_len * 8)) - 1;
        let mask = optional_num(params, "value_mask")?.unwrap_or(all_bits) & all_bits;
        let value = value & all_bits;
        clauses.push(format!("(wpdata & {mask:X}) == {:X}", value & mask));
    }

    Ok(clauses
        .into_iter()
        .map(|clause| format!("({clause})"))
        .collect::<Vec<_>>()
        .join(" && "))
}

fn parse_snapshot_specs(raw: Option<&Value>) -> BridgeResult<Vec<SnapshotSpec>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = raw.as_array() else {
        return Err(BridgeError::BadParams("snapshot must be a list".into()));
    };
    let mut out = Vec::new();
    for item in items {
        let Some(raw_spec) = item.as_str() else {
            return Err(BridgeError::BadParams(format!(
                "invalid snapshot spec: {item}"
            )));
        };
        let parts: Vec<_> = raw_spec.split(':').collect();
        if parts.len() != 3 {
            return Err(BridgeError::BadParams(format!(
                "invalid snapshot spec: {raw_spec}"
            )));
        }
        if memory_region(parts[0]).is_none() {
            return Err(BridgeError::BadParams(format!(
                "unsupported snapshot memory_type: {}",
                parts[0]
            )));
        }
        let address = parse_num_str(parts[1]).ok_or_else(|| {
            BridgeError::BadParams(format!("invalid snapshot address: {}", parts[1]))
        })? as usize;
        let length = parse_num_str(parts[2]).ok_or_else(|| {
            BridgeError::BadParams(format!("invalid snapshot length: {}", parts[2]))
        })? as usize;
        if length > MAX_READ_CHUNK {
            return Err(BridgeError::BadParams(format!(
                "snapshot length exceeds {MAX_READ_CHUNK} bytes"
            )));
        }
        out.push(SnapshotSpec {
            memory_type: parts[0].into(),
            address,
            length,
        });
    }
    Ok(out)
}

fn normalize_debug_register(raw_reg: &str) -> BridgeResult<(&'static str, &'static str)> {
    let mut key = raw_reg.trim().to_ascii_lowercase();
    if let Some(stripped) = key.strip_prefix("cpu.") {
        key = stripped.into();
    }
    match key.as_str() {
        "eax" | "ax" => Ok(("eax", "cpu.eax")),
        "ecx" | "cx" => Ok(("ecx", "cpu.ecx")),
        "edx" | "dx" => Ok(("edx", "cpu.edx")),
        "ebx" | "bx" => Ok(("ebx", "cpu.ebx")),
        "esp" | "sp" => Ok(("esp", "cpu.esp")),
        "ebp" | "bp" => Ok(("ebp", "cpu.ebp")),
        "esi" | "si" => Ok(("esi", "cpu.esi")),
        "edi" | "di" => Ok(("edi", "cpu.edi")),
        "eip" | "ip" => Ok(("eip", "cpu.eip")),
        "offset_pc" => Ok(("eip", "cpu.offset_pc")),
        "pc" => Ok(("pc", "cpu.pc")),
        "eflags" | "flags" => Ok(("eflags", "cpu.eflags")),
        "cs" => Ok(("cs", "cpu.cs")),
        "ss" => Ok(("ss", "cpu.ss")),
        "ds" => Ok(("ds", "cpu.ds")),
        "es" => Ok(("es", "cpu.es")),
        "fs" => Ok(("fs", "cpu.fs")),
        "gs" => Ok(("gs", "cpu.gs")),
        _ => Err(BridgeError::BadParams(format!(
            "unsupported PC-98 register: {raw_reg}; valid: ax, bx, cx, dx, sp, bp, si, di, ip, pc, flags, cs, ss, ds, es, fs, gs"
        ))),
    }
}

fn stop_event(stop: &str) -> Value {
    let mut event = json!({ "type": "stop", "signal": stop.get(1..3).unwrap_or(""), "raw": stop });
    if !stop.starts_with('T') {
        return event;
    }
    let Some(body) = stop.get(3..) else {
        return event;
    };
    let Some((key, rest)) = body.split_once(':') else {
        return event;
    };
    let mut parts = rest.split(';');
    let raw_hex = parts.next().unwrap_or_default();
    let mut fields = BTreeMap::new();
    for item in parts {
        if let Some((field, value)) = item.split_once(':') {
            fields.insert(field, value);
        }
    }
    let Some(address) = little_hex_to_u64(raw_hex) else {
        return event;
    };
    match key {
        "hwbreak" => {
            set_event_field(&mut event, "type", json!("breakpoint_hit"));
            set_event_field(&mut event, "kind", json!("exec"));
            set_event_field(&mut event, "address", json!(address));
        }
        "watch" => {
            set_event_field(&mut event, "type", json!("breakpoint_hit"));
            set_event_field(&mut event, "kind", json!("write"));
            set_event_field(&mut event, "address", json!(address));
        }
        "rwatch" => {
            set_event_field(&mut event, "type", json!("breakpoint_hit"));
            set_event_field(&mut event, "kind", json!("read"));
            set_event_field(&mut event, "address", json!(address));
        }
        "awatch" => {
            set_event_field(&mut event, "type", json!("breakpoint_hit"));
            set_event_field(&mut event, "kind", json!("access"));
            set_event_field(&mut event, "address", json!(address));
        }
        "reset" => {
            set_event_field(&mut event, "type", json!("reset"));
            set_event_field(&mut event, "pc", json!(address));
            set_event_field(&mut event, "address", json!(address));
        }
        "regwatch" => {
            set_event_field(&mut event, "type", json!("register_break"));
            set_event_field(&mut event, "pc", json!(address));
            set_event_field(&mut event, "address", json!(address));
        }
        _ => {}
    }
    if let Some(idx) = fields.get("idx") {
        match idx.parse::<u64>() {
            Ok(idx) => set_event_field(&mut event, "backend_id", json!(idx)),
            Err(_) => set_event_field(&mut event, "backend_id_error", json!(idx)),
        }
    }
    if let Some(regs_hex) = fields.get("regs") {
        set_event_field(&mut event, "regs", state_from_regs_hex(regs_hex));
    }
    event
}

fn little_hex_to_u64(raw: &str) -> Option<u64> {
    let bytes = hex::decode(raw).ok()?;
    let mut padded = [0u8; 8];
    let len = bytes.len().min(8);
    padded[..len].copy_from_slice(&bytes[..len]);
    Some(u64::from_le_bytes(padded))
}

fn set_event_field(event: &mut Value, key: &str, value: Value) {
    if let Some(obj) = event.as_object_mut() {
        obj.insert(key.into(), value);
    }
}

fn mark_event_enriched(event: &mut Value) {
    set_event_field(event, "_pc98_enriched", json!(true));
}

fn state_restore_info() -> Value {
    json!({
        "format": STATE_FORMAT,
        "scope": "cpu-register-packet-plus-ram-tvram-gvram-plus-mame-save-items",
        "deterministic_replay": true,
        "hidden_device_state": true,
        "save_manager_items": true,
        "save_manager_restore": "best_effort_lua_item_write",
        "post_restore_instruction_exact": true,
        "native_atomic_machine_state_load": false,
        "freeze_strategy": "lua_frozen_socket_service",
        "notes": "PC-98 state bundles restore RAM/TVRAM/GVRAM, MAME save-manager items exposed through Lua, and the i386 register packet.",
    })
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// A sibling `.partial` temp of `dst` (same parent → the later rename stays on one filesystem and is
/// atomic), tagged with this process id and a nanosecond stamp. save_state stages the zip here and
/// renames over `dst` only when complete, so a mid-save failure never truncates a pre-existing save.
fn state_partial_sibling(dst: &Path) -> BridgeResult<PathBuf> {
    let parent = dst.parent().ok_or_else(|| {
        BridgeError::BadParams(format!(
            "save path {} has no parent directory to stage under",
            dst.display()
        ))
    })?;
    let name = dst
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("state.zip");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".{name}.partial.{}.{nanos}", std::process::id())))
}

fn unique_temp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    for attempt in 0..100u32 {
        let path = std::env::temp_dir().join(format!(
            "{prefix}{}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
            attempt
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temp directory",
    ))
}

fn save_item_members(root: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
                continue;
            }
            if path.is_file() {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let member = rel
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((path, format!("{SAVE_ITEMS_DIR}/{member}")));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

fn parse_save_items_response(
    resp: &str,
    command: &str,
) -> BridgeResult<serde_json::Map<String, Value>> {
    let parts: Vec<_> = resp.split('|').collect();
    if parts.len() != 3 || parts[0] != "OK" {
        return Err(BridgeError::Emulator(format!(
            "MAME Lua command {command} failed: {resp}"
        )));
    }
    let items = parts[1]
        .parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("MAME Lua command {command} failed: {resp}")))?;
    let skipped = parts[2]
        .parse::<u64>()
        .map_err(|_| BridgeError::Emulator(format!("MAME Lua command {command} failed: {resp}")))?;
    let mut out = serde_json::Map::new();
    out.insert("items".into(), json!(items));
    out.insert("skipped".into(), json!(skipped));
    Ok(out)
}

fn read_state_manifest<R: Read + Seek>(archive: &mut ZipArchive<R>) -> BridgeResult<Value> {
    let mut file = archive.by_name("state.json")?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}

fn state_format(manifest: &Value) -> BridgeResult<String> {
    let format = manifest
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if format != STATE_FORMAT && format != LEGACY_STATE_FORMAT {
        return Err(BridgeError::BadParams(format!(
            "unsupported PC-98 state format: {format}"
        )));
    }
    Ok(format.into())
}

fn extract_save_items<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &Value,
    target_root: &Path,
) -> BridgeResult<Option<PathBuf>> {
    let Some(save_items) = manifest.get("save_items").and_then(Value::as_object) else {
        return Ok(None);
    };
    let directory = save_items
        .get("dir")
        .and_then(Value::as_str)
        .unwrap_or(SAVE_ITEMS_DIR)
        .trim_matches('/');
    if directory != SAVE_ITEMS_DIR {
        return Err(BridgeError::BadParams(format!(
            "unsupported PC-98 save item directory: {directory}"
        )));
    }
    let names = archive
        .file_names()
        .filter(|name| name.starts_with(&format!("{SAVE_ITEMS_DIR}/")))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !names.iter().any(|name| name == SAVE_ITEMS_MANIFEST) {
        return Err(BridgeError::BadParams(
            "PC-98 save item manifest is missing".into(),
        ));
    }
    let out_dir = target_root.join(SAVE_ITEMS_DIR);
    for name in names {
        let rel = &name[SAVE_ITEMS_DIR.len() + 1..];
        if rel.is_empty() || rel.ends_with('/') {
            continue;
        }
        let parts = rel.split('/').collect::<Vec<_>>();
        if parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == ".." || part.contains('\\'))
        {
            return Err(BridgeError::BadParams(format!(
                "unsafe PC-98 save item member: {name}"
            )));
        }
        let dest = parts
            .iter()
            .fold(out_dir.clone(), |path, part| path.join(part));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut src = archive.by_name(&name)?;
        let mut bytes = Vec::new();
        src.read_to_end(&mut bytes)?;
        fs::write(dest, bytes)?;
    }
    Ok(Some(out_dir))
}

fn read_state_regions<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &Value,
) -> BridgeResult<Vec<(String, Vec<u8>)>> {
    let Some(regions) = manifest.get("regions").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for region in regions {
        let memory_type = region
            .get("memory_type")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BridgeError::BadParams("PC-98 state region missing memory_type".into())
            })?;
        let file_name = region
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| BridgeError::BadParams("PC-98 state region missing file".into()))?;
        let mut file = archive.by_name(file_name)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        out.push((memory_type.into(), bytes));
    }
    Ok(out)
}

fn parse_register_probe_response(resp: &str) -> BridgeResult<Value> {
    if !resp.starts_with("HEX:") {
        return Err(BridgeError::Emulator(format!(
            "MAME register probe failed: {resp}"
        )));
    }
    let mut fields = BTreeMap::new();
    for part in resp.split('|') {
        if let Some((key, value)) = part.split_once(':') {
            fields.insert(key, value);
        }
    }
    let hexstr = fields.get("HEX").copied().unwrap_or_default();
    if hexstr.len() % 2 != 0 {
        return Err(BridgeError::Emulator(format!(
            "MAME register probe returned odd-length hex: {hexstr}"
        )));
    }
    let mut out = serde_json::Map::new();
    out.insert("hex".into(), json!(hexstr));
    out.insert("state_restore".into(), state_restore_info());
    if let Some(frame) = fields.get("FRAME") {
        match frame.parse::<u64>() {
            Ok(frame) => {
                out.insert("frame".into(), json!(frame));
            }
            Err(_) => {
                out.insert("frame_error".into(), json!(frame));
            }
        }
    }
    if let Some(regs_hex) = fields.get("REGS") {
        out.insert("regs".into(), state_from_regs_hex(regs_hex));
    }
    Ok(Value::Object(out))
}

fn state_matches_real_mode_pc(current: &Value, target: &Value) -> bool {
    let current_cs = current
        .get("cpu.cs")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let current_eip = current
        .get("cpu.eip")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let target_cs = target
        .get("cpu.cs")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let target_eip = target
        .get("cpu.eip")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    (current_cs & 0xFFFF) == target_cs && (current_eip & 0xFFFF) == target_eip
}

fn parse_dasm_lines<'a>(lines: impl Iterator<Item = &'a str>, count: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for line in lines {
        if out.len() >= count {
            break;
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let Some((addr_raw, rest_raw)) = raw.split_once(':') else {
            continue;
        };
        let Ok(addr) = u64::from_str_radix(addr_raw.trim(), 16) else {
            continue;
        };
        let rest = rest_raw.trim();
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let mut byte_parts = Vec::new();
        let mut idx = 0usize;
        while idx < parts.len() && is_hex_byte(parts[idx]) {
            byte_parts.push(parts[idx].to_ascii_lowercase());
            idx += 1;
        }
        let text = if idx < parts.len() {
            parts[idx..].join(" ")
        } else {
            rest.to_string()
        };
        let mut item = serde_json::Map::new();
        item.insert("addr".into(), json!(addr));
        item.insert("text".into(), json!(text));
        if !byte_parts.is_empty() {
            item.insert("bytes".into(), json!(byte_parts.join("")));
        }
        out.push(Value::Object(item));
    }
    out
}

fn is_hex_byte(s: &str) -> bool {
    s.len() == 2 && s.as_bytes().iter().all(u8::is_ascii_hexdigit)
}

fn parse_trace_line(line: &str) -> Option<Value> {
    let raw = line.trim();
    if raw.is_empty() {
        return None;
    }
    let Some((left, rest_raw)) = raw.split_once(':') else {
        return Some(json!({ "raw": raw }));
    };
    let token = left.split_whitespace().last().unwrap_or(left).trim();
    if token.len() < 4 || token.len() > 8 || !token.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Some(json!({ "raw": raw }));
    }
    let Ok(pc) = u64::from_str_radix(token, 16) else {
        return Some(json!({ "raw": raw }));
    };
    let rest = rest_raw.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let mut byte_parts = Vec::new();
    let mut idx = 0usize;
    while idx < parts.len() && is_hex_byte(parts[idx]) {
        byte_parts.push(parts[idx].to_ascii_lowercase());
        idx += 1;
    }
    let text = if idx < parts.len() {
        parts[idx..].join(" ")
    } else {
        rest.to_string()
    };
    let mut row = serde_json::Map::new();
    row.insert("pc".into(), json!(pc));
    row.insert("text".into(), json!(text));
    row.insert("raw".into(), json!(raw));
    if !byte_parts.is_empty() {
        row.insert("bytes".into(), json!(byte_parts.join("")));
    }
    Some(Value::Object(row))
}

fn state_from_regs_hex(resp: &str) -> Value {
    let mut state = serde_json::Map::new();
    if resp.len() >= I386_REGS.len() * 8 {
        decode_regs(resp, I386_REGS, 4, &mut state);
        if let Some(eip) = state.get("cpu.eip").and_then(Value::as_u64) {
            state.insert("cpu.offset_pc".into(), json!(eip));
            state.insert(
                "cpu.pc".into(),
                json!(segmented_pc(
                    state.get("cpu.cs").and_then(Value::as_u64).unwrap_or(0),
                    eip
                )),
            );
        }
    } else if resp.len() >= I86_REGS.len() * 4 {
        decode_regs(resp, I86_REGS, 2, &mut state);
        if let Some(ip) = state.get("cpu.ip").and_then(Value::as_u64) {
            state.insert("cpu.offset_pc".into(), json!(ip));
            state.insert(
                "cpu.pc".into(),
                json!(segmented_pc(
                    state.get("cpu.cs").and_then(Value::as_u64).unwrap_or(0),
                    ip
                )),
            );
        }
    } else {
        state.insert("cpu.raw_register_bytes".into(), json!(resp.len() / 2));
    }
    Value::Object(state)
}

fn decode_regs(
    resp: &str,
    names: &[&str],
    width: usize,
    state: &mut serde_json::Map<String, Value>,
) {
    let chars = width * 2;
    for (idx, name) in names.iter().enumerate() {
        let start = idx * chars;
        let end = start + chars;
        if end > resp.len() {
            break;
        }
        if let Ok(bytes) = hex::decode(&resp[start..end]) {
            let mut raw = [0u8; 8];
            raw[..bytes.len()].copy_from_slice(&bytes);
            let value = u64::from_le_bytes(raw);
            state.insert(format!("cpu.{name}"), json!(value));
        }
    }
}

fn segmented_pc(cs: u64, ip: u64) -> u64 {
    ((cs << 4) + ip) & 0xFFFF_FFFF
}

fn sha1_file(path: &Path) -> std::io::Result<String> {
    let mut h = Sha1::new();
    let mut file = File::open(path)?;
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
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
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[derive(Default)]
    struct FakeGdb {
        replies: VecDeque<(String, String)>,
        calls: Vec<String>,
        no_reply: Vec<String>,
        nonblocking: VecDeque<String>,
        /// (trigger payload, stop) pairs: when `send` serves the trigger, the stop is enqueued to
        /// `nonblocking`. Models an async stop that arrives *after* a command (e.g. a frame-target
        /// stop that coincides with a BP hit), which a pre-command drain must not see early.
        nonblocking_after: Vec<(String, String)>,
        /// Trailing interrupt-echo stops the stub leaves buffered after a 0x03 break. `interrupt()`
        /// enqueues these to `nonblocking` so the echo arrives *after* the interrupt, exactly as the
        /// real stub does — a pre-interrupt drain must never mistake them for a pre-existing hit.
        interrupt_echo: Vec<String>,
        timeout: Duration,
        timeouts: Vec<Duration>,
        interrupts: usize,
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

        fn from_pairs(replies: Vec<(String, String)>) -> Self {
            Self {
                replies: replies.into(),
                ..Default::default()
            }
        }

        fn with_nonblocking(mut self, replies: &[&str]) -> Self {
            self.nonblocking = replies.iter().map(|reply| (*reply).into()).collect();
            self
        }

        fn enqueue_nonblocking_after(mut self, trigger: &str, stop: &str) -> Self {
            self.nonblocking_after.push((trigger.into(), stop.into()));
            self
        }

        fn with_interrupt_echo(mut self, echoes: &[&str]) -> Self {
            self.interrupt_echo = echoes.iter().map(|s| (*s).into()).collect();
            self
        }
    }

    impl GdbTransport for FakeGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            self.calls.push(payload.into());
            let Some((expected, reply)) = self.replies.pop_front() else {
                return Err(BridgeError::Emulator(format!(
                    "unexpected fake GDB call: {payload}"
                )));
            };
            assert_eq!(payload, expected);
            if let Some(pos) = self
                .nonblocking_after
                .iter()
                .position(|(trigger, _)| trigger == payload)
            {
                let (_, stop) = self.nonblocking_after.remove(pos);
                self.nonblocking.push_back(stop);
            }
            Ok(reply)
        }

        fn send_no_reply(&mut self, payload: &str) -> BridgeResult<()> {
            self.no_reply.push(payload.into());
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            self.interrupts += 1;
            // The real stub leaves its trailing echo stop(s) buffered *after* the 0x03 break, so
            // enqueue them to nonblocking here rather than pre-seeding them ahead of the interrupt.
            for echo in &self.interrupt_echo {
                self.nonblocking.push_back(echo.clone());
            }
            Ok("S05".into())
        }

        fn get_timeout(&self) -> BridgeResult<Duration> {
            if self.timeout.is_zero() {
                Ok(Duration::from_secs(5))
            } else {
                Ok(self.timeout)
            }
        }

        fn set_timeout(&mut self, timeout: Duration) -> BridgeResult<()> {
            self.timeout = timeout;
            self.timeouts.push(timeout);
            Ok(())
        }

        fn recv_nonblocking(&mut self) -> BridgeResult<Option<String>> {
            Ok(self.nonblocking.pop_front())
        }
    }

    fn i386_regs_hex(values: &[(&str, u32)]) -> String {
        let mut out = Vec::new();
        for name in I386_REGS {
            let value = values
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| *v)
                .unwrap_or(0);
            out.extend_from_slice(&value.to_le_bytes());
        }
        hex::encode(out)
    }

    struct StateSaveGdb {
        regs_hex: String,
        save_items_dir: Option<PathBuf>,
        reads: usize,
        fail_at_read: Option<usize>,
    }

    impl StateSaveGdb {
        fn new(regs_hex: String) -> Self {
            Self {
                regs_hex,
                save_items_dir: None,
                reads: 0,
                fail_at_read: None,
            }
        }

        /// Fake a region read that times out on the Nth `m` read, so save_state fails mid-zip.
        fn failing_at_read(regs_hex: String, fail_at_read: usize) -> Self {
            Self {
                regs_hex,
                save_items_dir: None,
                reads: 0,
                fail_at_read: Some(fail_at_read),
            }
        }
    }

    impl GdbTransport for StateSaveGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            if payload == "qEmucap,stop" {
                return Ok("OK".into());
            }
            if payload == "g" {
                return Ok(self.regs_hex.clone());
            }
            if let Some(hex_path) = payload.strip_prefix("qEmucap,saveitems,") {
                let bytes = hex::decode(hex_path)
                    .map_err(|_| BridgeError::Emulator("bad saveitems path hex".into()))?;
                let path = PathBuf::from(
                    String::from_utf8(bytes)
                        .map_err(|_| BridgeError::Emulator("bad saveitems path utf8".into()))?,
                );
                std::fs::create_dir_all(&path)?;
                std::fs::write(path.join("manifest.txt"), "item\n")?;
                self.save_items_dir = Some(path);
                return Ok("OK|1|0".into());
            }
            if let Some(rest) = payload.strip_prefix('m') {
                let Some((_addr, len_hex)) = rest.split_once(',') else {
                    return Err(BridgeError::Emulator(format!("bad read: {payload}")));
                };
                let len = usize::from_str_radix(len_hex, 16)
                    .map_err(|_| BridgeError::Emulator(format!("bad read len: {payload}")))?;
                self.reads += 1;
                if self.fail_at_read == Some(self.reads) {
                    return Err(BridgeError::Emulator("simulated region read timeout".into()));
                }
                return Ok("00".repeat(len));
            }
            Err(BridgeError::Emulator(format!("unexpected call: {payload}")))
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }
    }

    #[derive(Default)]
    struct StateLoadGdb {
        regs_hex: String,
        writes: Vec<String>,
        regprobe_specs: Vec<String>,
        load_items_dirs: Vec<PathBuf>,
    }

    impl StateLoadGdb {
        fn new(regs_hex: String) -> Self {
            Self {
                regs_hex,
                ..Default::default()
            }
        }
    }

    impl GdbTransport for StateLoadGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            if payload == "qEmucap,stop" {
                return Ok("OK".into());
            }
            if let Some(hex_path) = payload.strip_prefix("qEmucap,loaditems,") {
                let bytes = hex::decode(hex_path)
                    .map_err(|_| BridgeError::Emulator("bad loaditems path hex".into()))?;
                self.load_items_dirs
                    .push(PathBuf::from(String::from_utf8(bytes).map_err(|_| {
                        BridgeError::Emulator("bad loaditems path utf8".into())
                    })?));
                return Ok("OK|1|0".into());
            }
            if payload.starts_with('M') {
                self.writes.push(payload.into());
                return Ok("OK".into());
            }
            if let Some(hex_regs) = payload.strip_prefix("qEmucap,regload,") {
                let bytes = hex::decode(hex_regs)
                    .map_err(|_| BridgeError::Emulator("bad regload hex".into()))?;
                let regs = String::from_utf8(bytes)
                    .map_err(|_| BridgeError::Emulator("bad regload utf8".into()))?;
                assert_eq!(regs, self.regs_hex);
                return Ok(format!("OK|{}", self.regs_hex));
            }
            if let Some(hex_spec) = payload.strip_prefix("qEmucap,regprobe,") {
                let bytes = hex::decode(hex_spec)
                    .map_err(|_| BridgeError::Emulator("bad regprobe hex".into()))?;
                let spec = String::from_utf8(bytes)
                    .map_err(|_| BridgeError::Emulator("bad regprobe utf8".into()))?;
                self.regprobe_specs.push(spec);
                return Ok(format!("HEX:cafe|FRAME:3|REGS:{}", self.regs_hex));
            }
            Err(BridgeError::Emulator(format!("unexpected call: {payload}")))
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }
    }

    fn write_test_state(path: &Path, regs_hex: &str) {
        let file = File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("tvram.bin", options).unwrap();
        zip.write_all(&[0xAA, 0xBB]).unwrap();
        zip.start_file(SAVE_ITEMS_MANIFEST, options).unwrap();
        zip.write_all(b"item\n").unwrap();
        zip.start_file("state.json", options).unwrap();
        let manifest = json!({
            "format": STATE_FORMAT,
            "system": "pc98",
            "adapter": "mame-pc98-gdb",
            "registers_hex": regs_hex,
            "regions": [{
                "name": "tvram",
                "memory_type": "tvram",
                "base_address": 0xA0000,
                "size": 2,
                "file": "tvram.bin",
            }],
            "save_items": {"items": 1, "skipped": 0, "dir": SAVE_ITEMS_DIR},
            "state_restore": state_restore_info(),
        });
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn hello_advertises_only_implemented_rust_methods() {
        let env = BridgeEnv {
            name: Some("pc98".into()),
            session_token: Some("token".into()),
            build: Some("abc123".into()),
            ..Default::default()
        };
        let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), env);
        let response = bridge.handle_request(Request::new(1, "hello", json!({})));
        let result = response.result.unwrap();
        assert_eq!(result["adapter"], "mame-pc98-rust-gdb");
        assert_eq!(result["name"], "pc98");
        assert_eq!(result["session_token"], "token");
        assert_eq!(result["build"], "abc123");
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "read_memory"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "find_pattern"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "screenshot"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "run_frames"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "step_instructions"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "disassemble"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "set_breakpoint"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "poll_events"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "watch_register"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "set_trace"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "get_trace"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "call_stack"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "load_state"));
        assert!(result["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "probe"));
        assert_eq!(
            result["memory_types"],
            json!(["cpu", "gvram_b", "gvram_g", "gvram_i", "gvram_r", "physical", "ram", "tvram"])
        );
    }

    #[test]
    fn read_and_write_memory_map_regions_to_absolute_gdb_addresses() {
        let fake = FakeGdb::with(&[
            ("?", "S05"),
            ("ma0010,4", "01020304"),
            ("Ma0010,2:aabb", "OK"),
        ]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());

        let read = bridge.handle_request(Request::new(
            2,
            "read_memory",
            json!({"memory_type":"tvram","address":"0x10","length":4}),
        ));
        assert_eq!(read.result.unwrap()["hex"], "01020304");

        let write = bridge.handle_request(Request::new(
            3,
            "write_memory",
            json!({"memory_type":"tvram","address":"$10","hex":"aabb"}),
        ));
        assert_eq!(write.result.unwrap()["written"], 2);
    }

    #[test]
    fn read_memory_rejects_access_straddling_region_end() {
        let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), BridgeEnv::default());
        let response = bridge.handle_request(Request::new(
            4,
            "read_memory",
            json!({"memory_type":"tvram","address":"0x3fff","length":2}),
        ));
        assert!(!response.ok);
        let error = response.error.unwrap();
        assert_eq!(error.kind, "bad_params");
        assert!(error.message.contains("tvram access out of range"));
        assert_eq!(bridge.gdb.calls, vec!["?"], "reject before GDB read");
    }

    #[test]
    fn write_memory_rejects_access_straddling_region_end() {
        let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), BridgeEnv::default());
        let response = bridge.handle_request(Request::new(
            5,
            "write_memory",
            json!({"memory_type":"tvram","address":"0x3fff","hex":"aabb"}),
        ));
        assert!(!response.ok);
        let error = response.error.unwrap();
        assert_eq!(error.kind, "bad_params");
        assert!(error.message.contains("tvram access out of range"));
        assert_eq!(bridge.gdb.calls, vec!["?"], "reject before GDB write");
    }

    #[test]
    fn memory_access_ending_exactly_at_region_end_is_allowed() {
        let fake = FakeGdb::with(&[("?", "S05"), ("ma3fff,1", "7f"), ("Ma3fff,1:80", "OK")]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());

        let read = bridge.handle_request(Request::new(
            6,
            "read_memory",
            json!({"memory_type":"tvram","address":"0x3fff","length":1}),
        ));
        assert_eq!(read.result.unwrap()["hex"], "7f");

        let write = bridge.handle_request(Request::new(
            7,
            "write_memory",
            json!({"memory_type":"tvram","address":"0x3fff","hex":"80"}),
        ));
        assert_eq!(write.result.unwrap()["written"], 1);
    }

    #[test]
    fn find_pattern_scans_region_with_match_limit() {
        let mut bridge = Bridge::new(
            FakeGdb::with(&[("?", "S05"), ("m0,8", "aa00aa00aa00aa00")]),
            BridgeEnv::default(),
        );
        let response = bridge.handle_request(Request::new(
            7,
            "find_pattern",
            json!({"memory_type":"ram","start":0,"length":8,"hex":"aa00","max_matches":2}),
        ));
        let result = response.result.unwrap();
        assert_eq!(result["matches"], json!([0, 2]));
        assert_eq!(result["count"], 2);
        assert_eq!(result["truncated_matches"], true);
        assert_eq!(result["truncated"], true);
    }

    #[test]
    fn input_methods_send_lua_commands_with_normalized_buttons() {
        let mut bridge = Bridge::new(
            FakeGdb::with(&[
                ("?", "S05"),
                ("qEmucap,setinput,656e7465722c657363", "OK"),
                ("qEmucap,press,333a612c62", "OK"),
                ("qEmucap,frame", "42"),
                ("qEmucap,reset", "OK"),
                ("qEmucap,breakonreset,31", "OK"),
            ]),
            BridgeEnv::default(),
        );

        let set = bridge.handle_request(Request::new(
            8,
            "set_input",
            json!({"buttons":["start","escape"]}),
        ));
        assert_eq!(set.result.unwrap()["buttons"], json!(["enter", "esc"]));

        let press = bridge.handle_request(Request::new(
            9,
            "press_buttons",
            json!({"buttons":["a","b"],"frames":3}),
        ));
        assert_eq!(
            press.result.unwrap(),
            json!({
                "status":"completed",
                "buttons":["a","b"],
                "frames":3,
                "frame":42,
                "state":"running"
            })
        );

        let reset = bridge.handle_request(Request::new(10, "reset", json!({})));
        assert_eq!(reset.result.unwrap()["reset"], "scheduled");

        let br = bridge.handle_request(Request::new(11, "break_on_reset", json!({"enabled":true})));
        assert_eq!(br.result.unwrap()["mode"], "machine_reset_notifier");
    }

    #[test]
    fn breakpoint_methods_set_list_clear_and_enrich_events() {
        let condition = "(pc >= 2000) && ((wpdata & FF) == 42)";
        let set_spec = format!("3|a0010|5|1|{condition}");
        let clear_spec = "wp|7";
        let regs = i386_regs_hex(&[("eip", 0x1234), ("cs", 0)]);
        let fake = FakeGdb::from_pairs(vec![
            ("?".into(), "S05".into()),
            (
                format!("qEmucap,setpoint,{}", hex::encode(set_spec.as_bytes())),
                "WP:7".into(),
            ),
            (
                format!("qEmucap,clearpoint,{}", hex::encode(clear_spec.as_bytes())),
                "OK".into(),
            ),
            (
                format!(
                    "qEmucap,setpoint,{}",
                    hex::encode("0|a0000|1|1|".as_bytes())
                ),
                "BP:2".into(),
            ),
            ("qEmucap,pollreset".into(), "NONE".into()),
            ("g".into(), regs),
            ("ma0000,2".into(), "aabb".into()),
        ])
        .with_nonblocking(&["T05hwbreak:00000a00;idx:2;"]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());

        let set = bridge.handle_request(Request::new(
            12,
            "set_breakpoint",
            json!({
                "kind": "read",
                "memory_type": "tvram",
                "start": "0x10",
                "end": "0x14",
                "pc_min": "0x2000",
                "value": "0x42"
            }),
        ));
        assert_eq!(set.result.unwrap()["id"], 1);

        let list = bridge.handle_request(Request::new(13, "list_breakpoints", json!({})));
        assert_eq!(
            list.result.unwrap()["breakpoints"],
            json!([{
                "id": 1,
                "kind": "read",
                "start": 0xA0010,
                "end": 0xA0014,
                "condition": condition,
            }])
        );

        let cleared = bridge.handle_request(Request::new(14, "clear_breakpoint", json!({"id": 1})));
        assert_eq!(cleared.result.unwrap()["cleared"], 1);

        let set_exec = bridge.handle_request(Request::new(
            15,
            "set_breakpoint",
            json!({"kind": "exec", "memory_type": "tvram", "start": 0, "snapshot": ["tvram:0:2"]}),
        ));
        assert_eq!(set_exec.result.unwrap()["id"], 2);
        bridge.frozen = false;

        let events = bridge.handle_request(Request::new(16, "poll_events", json!({})));
        let events = events.result.unwrap();
        assert_eq!(events["dropped"], 0);
        assert_eq!(
            events["events"][0],
            json!({
                "type": "breakpoint_hit",
                "signal": "05",
                "raw": "T05hwbreak:00000a00;idx:2;",
                "kind": "exec",
                "address": 0xA0000,
                "backend_id": 2,
                "id": 2,
                "breakpoint_id": 2,
                "regs": {
                    "cpu.eax": 0, "cpu.ecx": 0, "cpu.edx": 0, "cpu.ebx": 0,
                    "cpu.esp": 0, "cpu.ebp": 0, "cpu.esi": 0, "cpu.edi": 0,
                    "cpu.eip": 0x1234, "cpu.eflags": 0, "cpu.cs": 0, "cpu.ss": 0,
                    "cpu.ds": 0, "cpu.es": 0, "cpu.fs": 0, "cpu.gs": 0,
                    "cpu.offset_pc": 0x1234, "cpu.pc": 0x1234,
                },
                "snapshot": [{"memory_type": "tvram", "address": 0, "hex": "aabb"}],
            })
        );
    }

    #[test]
    fn set_breakpoint_rejects_out_of_range_region_offset() {
        // tvram is 0x4000; without the bound, region.base + start lands past the region and MAME's
        // setpoint may silently accept an address that can never fire. Reject before arming.
        let fake = FakeGdb::with(&[("?", "S05")]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        let r = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "exec", "memory_type": "tvram", "start": "0x5000"}),
        ));
        assert!(!r.ok);
        let err = r.error.unwrap();
        assert_eq!(err.kind, "bad_params");
        assert!(
            err.message.contains("out of range") && err.message.contains("tvram"),
            "names the region and the out-of-range condition: {}",
            err.message
        );
        // Nothing was armed on the emulator.
        assert!(!bridge.gdb.calls.iter().any(|c| c.contains("setpoint")));
    }

    #[test]
    fn set_breakpoint_in_range_resolves_to_region_base_plus_offset() {
        // tvram base 0xA0000 + start 0x100 = 0xA0100; the setpoint spec carries the absolute address.
        let set_spec = format!("0|{:x}|1|1|", 0xA0000u64 + 0x100);
        let fake = FakeGdb::from_pairs(vec![
            ("?".into(), "S05".into()),
            (
                format!("qEmucap,setpoint,{}", hex::encode(set_spec.as_bytes())),
                "BP:9".into(),
            ),
        ]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        let r = bridge.handle_request(Request::new(
            1,
            "set_breakpoint",
            json!({"kind": "exec", "memory_type": "tvram", "start": "0x100"}),
        ));
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.result.unwrap()["id"], 1);
    }

    #[test]
    fn watch_register_sets_regpoint_and_reports_value() {
        let spec = "1|(esp < 1000) || (esp > 2000)";
        let regs = i386_regs_hex(&[("esp", 0x3000), ("eip", 0x2222), ("cs", 0)]);
        let fake = FakeGdb::from_pairs(vec![
            ("?".into(), "S05".into()),
            (
                format!("qEmucap,setregpoint,{}", hex::encode(spec.as_bytes())),
                "RP:3".into(),
            ),
            ("qEmucap,pollreset".into(), "NONE".into()),
            ("g".into(), regs),
        ])
        .with_nonblocking(&["T05regwatch:00100000;idx:3;"]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());

        let set = bridge.handle_request(Request::new(
            17,
            "watch_register",
            json!({"register": "sp", "min": "0x1000", "max": "0x2000"}),
        ));
        assert_eq!(set.result.unwrap()["id"], 1);
        bridge.frozen = false;

        let events = bridge.handle_request(Request::new(18, "poll_events", json!({})));
        assert_eq!(
            events.result.unwrap()["events"][0],
            json!({
                "type": "register_break",
                "signal": "05",
                "raw": "T05regwatch:00100000;idx:3;",
                "pc": 0x1000,
                "address": 0x1000,
                "backend_id": 3,
                "id": 1,
                "breakpoint_id": 1,
                "register": "sp",
                "min": 0x1000,
                "max": 0x2000,
                "value": 0x3000,
                "regs": {
                    "cpu.eax": 0, "cpu.ecx": 0, "cpu.edx": 0, "cpu.ebx": 0,
                    "cpu.esp": 0x3000, "cpu.ebp": 0, "cpu.esi": 0, "cpu.edi": 0,
                    "cpu.eip": 0x2222, "cpu.eflags": 0, "cpu.cs": 0, "cpu.ss": 0,
                    "cpu.ds": 0, "cpu.es": 0, "cpu.fs": 0, "cpu.gs": 0,
                    "cpu.offset_pc": 0x2222, "cpu.pc": 0x2222,
                },
            })
        );
    }

    #[test]
    fn run_frames_sends_lua_command_with_scaled_timeout() {
        let fake = FakeGdb::with(&[
            ("?", "S05"),
            ("qEmucap,runframes,33303030", "OK"),
            ("qEmucap,frame", "42"),
        ]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        let response = bridge.handle_request(Request::new(14, "run_frames", json!({"n": 3000})));
        let result = response.result.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["frames"], 3000);
        assert_eq!(result["frame"], 42);
        assert_eq!(result["state"], "running");
        assert!(bridge
            .gdb
            .timeouts
            .iter()
            .any(|t| *t > Duration::from_secs(5)));
        assert_eq!(bridge.gdb.get_timeout().unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn press_buttons_reports_breakpoint_interruption_and_releases_operation() {
        let fake = FakeGdb::with(&[
            ("?", "S05"),
            (
                "qEmucap,press,31303a656e746572",
                "T05hwbreak:01000000;idx:2;",
            ),
            ("qEmucap,frame", "77"),
        ]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        let response = bridge.handle_request(Request::new(
            15,
            "press_buttons",
            json!({"buttons":["start"],"frames":10}),
        ));
        let result = response.result.unwrap();
        assert_eq!(result["status"], "interrupted");
        assert_eq!(result["reason"], "breakpoint");
        assert_eq!(result["raw"], "T05hwbreak:01000000;idx:2;");
        assert_eq!(result["buttons"], json!(["enter"]));
        assert_eq!(result["frames"], 10);
        assert_eq!(result["frame"], 77);
    }

    #[test]
    fn step_frames_returns_interrupted_on_stop_reply() {
        let fake = FakeGdb::with(&[
            ("?", "S05"),
            ("qEmucap,framestep,3130", "T05hwbreak:01000000;idx:2;"),
            ("qEmucap,frame", "77"),
        ]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        let response = bridge.handle_request(Request::new(15, "step", json!({"frames": 10})));
        let result = response.result.unwrap();
        assert_eq!(result["status"], "interrupted");
        assert_eq!(result["reason"], "breakpoint");
        assert_eq!(result["raw"], "T05hwbreak:01000000;idx:2;");
        assert_eq!(result["frame"], 77);
    }

    #[test]
    fn step_frames_drains_immediate_stop_after_ok() {
        // The stop arrives *after* the framestep "OK" (a frame-target that coincides with a BP hit),
        // so drain_immediate_stops must pick it up as the result. Enqueued on the framestep send so
        // the new pre-command drain (which only sees stops buffered *before* the command) can't eat
        // it early.
        let fake = FakeGdb::with(&[
            ("?", "S05"),
            ("qEmucap,framestep,31", "OK"),
            ("qEmucap,frame", "9"),
        ])
        .enqueue_nonblocking_after("qEmucap,framestep,31", "S05");
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        let response = bridge.handle_request(Request::new(16, "step", json!({"frames": 1})));
        let result = response.result.unwrap();
        assert_eq!(result["status"], "interrupted");
        assert_eq!(result["raw"], "S05");
        assert_eq!(result["frame"], 9);
    }

    #[test]
    fn step_instructions_sends_gdb_single_step_count() {
        let fake = FakeGdb::with(&[("?", "S05"), ("s", "S05"), ("s", "S05"), ("s", "S05")]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        let response =
            bridge.handle_request(Request::new(17, "step_instructions", json!({"count": 3})));
        let result = response.result.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["unit"], "instructions");
        assert_eq!(result["count"], 3);
        assert_eq!(
            bridge
                .gdb
                .calls
                .iter()
                .filter(|call| call.as_str() == "s")
                .count(),
            3
        );
    }

    struct DasmGdb;

    impl GdbTransport for DasmGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            let prefix = "qEmucap,dasm,";
            if let Some(hex_spec) = payload.strip_prefix(prefix) {
                let bytes = hex::decode(hex_spec)
                    .map_err(|_| BridgeError::Emulator("bad dasm spec hex".into()))?;
                let spec = String::from_utf8(bytes)
                    .map_err(|_| BridgeError::Emulator("bad dasm spec utf8".into()))?;
                let mut parts = spec.split('|');
                let path = parts
                    .next()
                    .ok_or_else(|| BridgeError::Emulator("missing dasm path".into()))?;
                let address = parts
                    .next()
                    .ok_or_else(|| BridgeError::Emulator("missing dasm address".into()))?;
                let len = parts
                    .next()
                    .ok_or_else(|| BridgeError::Emulator("missing dasm length".into()))?;
                assert_eq!(address, "1000");
                assert_eq!(len, "20");
                std::fs::write(
                    path,
                    "00001000: b8 34 12 mov ax,1234\n00001003: cd 18 int 18\n",
                )?;
                return Ok("OK".into());
            }
            Err(BridgeError::Emulator(format!("unexpected call: {payload}")))
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }
    }

    #[test]
    fn disassemble_uses_lua_dasm_and_parses_instruction_rows() {
        let mut bridge = Bridge::new(DasmGdb, BridgeEnv::default());
        let response = bridge.handle_request(Request::new(
            19,
            "disassemble",
            json!({"address":"0x1000","count":2}),
        ));
        let result = response.result.unwrap();
        assert_eq!(
            result["instructions"],
            json!([
                {"addr": 0x1000, "text": "mov ax,1234", "bytes": "b83412"},
                {"addr": 0x1003, "text": "int 18", "bytes": "cd18"},
            ])
        );
    }

    #[derive(Default)]
    struct TraceGdb {
        path: Option<PathBuf>,
        flushes: usize,
        stops: usize,
    }

    impl TraceGdb {
        fn write_trace(&self) -> BridgeResult<()> {
            let Some(path) = &self.path else {
                return Ok(());
            };
            std::fs::write(
                path,
                concat!(
                    "00001000: e8 00 00 call 2000\n",
                    "00002000: 90 nop\n",
                    "00002001: c3 ret\n",
                    "00001003: e8 00 00 call 3000\n",
                ),
            )?;
            Ok(())
        }
    }

    impl GdbTransport for TraceGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            let prefix = "qEmucap,tracestart,";
            if let Some(hex_path) = payload.strip_prefix(prefix) {
                let bytes = hex::decode(hex_path)
                    .map_err(|_| BridgeError::Emulator("bad trace path hex".into()))?;
                let path = String::from_utf8(bytes)
                    .map_err(|_| BridgeError::Emulator("bad trace path utf8".into()))?;
                self.path = Some(PathBuf::from(path));
                self.write_trace()?;
                return Ok("OK".into());
            }
            if payload == "qEmucap,traceflush" {
                self.flushes += 1;
                self.write_trace()?;
                return Ok("OK".into());
            }
            if payload == "qEmucap,tracestop" {
                self.stops += 1;
                return Ok("OK".into());
            }
            Err(BridgeError::Emulator(format!("unexpected call: {payload}")))
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }
    }

    #[test]
    fn trace_methods_manage_lua_trace_file_and_parse_rows() {
        let mut bridge = Bridge::new(TraceGdb::default(), BridgeEnv::default());
        let started =
            bridge.handle_request(Request::new(20, "set_trace", json!({"enabled": true})));
        let started = started.result.unwrap();
        assert_eq!(started["tracing"], true);
        assert!(started["path"]
            .as_str()
            .unwrap()
            .contains("emucap_pc98_trace_"));

        let trace = bridge.handle_request(Request::new(21, "get_trace", json!({"count": 2})));
        let trace = trace.result.unwrap();
        assert_eq!(trace["tracing"], true);
        assert_eq!(trace["total"], 4);
        assert_eq!(
            trace["trace"],
            json!([
                {"pc": 0x2001, "text": "ret", "raw": "00002001: c3 ret", "bytes": "c3"},
                {"pc": 0x1003, "text": "call 3000", "raw": "00001003: e8 00 00 call 3000", "bytes": "e80000"},
            ])
        );

        let stack = bridge.handle_request(Request::new(22, "call_stack", json!({})));
        let stack = stack.result.unwrap();
        assert_eq!(stack["call_stack"], json!([0x1003]));
        assert_eq!(stack["depth"], 1);
        assert_eq!(
            stack["frames"],
            json!([{"pc": 0x1003, "text": "call 3000"}])
        );

        let stopped =
            bridge.handle_request(Request::new(23, "set_trace", json!({"enabled": false})));
        assert_eq!(stopped.result.unwrap()["tracing"], false);
        assert_eq!(bridge.gdb.stops, 1);
    }

    #[test]
    fn status_drains_nonblocking_stop_when_running() {
        let fake = FakeGdb::with(&[
            ("?", ""),
            ("qEmucap,inputfields", "enter,esc,space,a,b"),
            ("qEmucap,frame", "12"),
        ])
        .with_nonblocking(&["S05"]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        bridge.frozen = false;
        let response = bridge.handle_request(Request::new(18, "status", json!({})));
        let result = response.result.unwrap();
        assert_eq!(result["state"], "frozen");
        assert_eq!(
            result["input_buttons"]["available"],
            json!(["enter", "esc", "space", "a", "b"])
        );
    }

    #[test]
    fn dump_memory_writes_regions_under_requested_directory() {
        let mut replies = vec![("?".to_string(), "S05".to_string())];
        for name in DUMP_REGION_NAMES {
            let region = memory_region(name).unwrap();
            let mut offset = 0usize;
            while offset < region.size as usize {
                let chunk = MAX_READ_CHUNK.min(region.size as usize - offset);
                replies.push((
                    format!("m{:x},{:x}", region.base as usize + offset, chunk),
                    "00".repeat(chunk),
                ));
                offset += chunk;
            }
        }
        let mut bridge = Bridge::new(FakeGdb::from_pairs(replies), BridgeEnv::default());
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("dump");
        let response = bridge.handle_request(Request::new(
            12,
            "dump_memory",
            json!({"path": out.to_str().unwrap()}),
        ));
        assert_eq!(response.result.unwrap()["regions"], DUMP_REGION_NAMES.len());
        let regions: Value =
            serde_json::from_slice(&std::fs::read(out.join("regions.json")).unwrap()).unwrap();
        assert_eq!(regions.as_array().unwrap().len(), DUMP_REGION_NAMES.len());
        assert_eq!(
            std::fs::metadata(out.join("ram.bin")).unwrap().len(),
            memory_region("ram").unwrap().size as u64
        );
        assert_eq!(
            std::fs::metadata(out.join("tvram.bin")).unwrap().len(),
            memory_region("tvram").unwrap().size as u64
        );
    }

    #[test]
    fn save_state_writes_python_compatible_zip_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("state.zip");
        let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234)]);
        let mut bridge = Bridge::new(StateSaveGdb::new(regs.clone()), BridgeEnv::default());

        let response = bridge.handle_request(Request::new(
            24,
            "save_state",
            json!({"path": out.display().to_string()}),
        ));
        let result = response.result.unwrap();
        assert_eq!(result["format"], STATE_FORMAT);
        assert_eq!(
            result["save_items"],
            json!({"items": 1, "skipped": 0, "dir": SAVE_ITEMS_DIR})
        );
        assert!(result["bytes"].as_u64().unwrap() > 0);
        assert!(bridge.gdb.reads > 0);

        let file = File::open(&out).unwrap();
        let mut zip = ZipArchive::new(file).unwrap();
        assert!(zip.by_name(SAVE_ITEMS_MANIFEST).is_ok());
        let manifest = read_state_manifest(&mut zip).unwrap();
        assert_eq!(manifest["format"], STATE_FORMAT);
        assert_eq!(manifest["registers_hex"], regs);
        assert_eq!(
            manifest["regions"].as_array().unwrap().len(),
            DUMP_REGION_NAMES.len()
        );
    }

    #[test]
    fn save_state_preserves_prior_save_on_mid_save_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("state.zip");
        // A pre-existing valid savestate with a distinct byte fingerprint. A mid-save failure must
        // leave it byte-for-byte intact — never truncated by an in-place File::create.
        let prior = b"PRIOR-VALID-SAVESTATE-BYTES".to_vec();
        std::fs::write(&out, &prior).unwrap();

        let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234)]);
        // Fail on the first region read (timeout mid-zip), after the staging file is created.
        let mut bridge = Bridge::new(
            StateSaveGdb::failing_at_read(regs, 1),
            BridgeEnv::default(),
        );
        let response = bridge.handle_request(Request::new(
            60,
            "save_state",
            json!({"path": out.display().to_string()}),
        ));
        assert!(!response.ok, "a mid-save read failure must be reported as an error");

        // The prior savestate survives byte-for-byte (not truncated, not overwritten).
        assert_eq!(
            std::fs::read(&out).unwrap(),
            prior,
            "the pre-existing savestate must survive a mid-save failure"
        );

        // The staging .partial temp is cleaned up, not left behind.
        let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".partial"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the staging .partial temp must be removed: {leftovers:?}"
        );
    }

    #[test]
    fn run_frames_drains_pre_command_stale_stop() {
        // A buffered stop left after a prior resume() that hit a pause_on_hit BP sits ahead of the
        // frames command. Without a pre-command drain, drain_immediate_stops mis-consumes it as the
        // frames result → spurious interrupted+frozen. Draining it first routes it to the event queue
        // and the frames run completes normally (frozen stays false).
        let gdb = FakeGdb::from_pairs(vec![
            ("?".into(), "S05".into()),
            (format!("qEmucap,runframes,{}", hex::encode("3")), "OK".into()),
            ("qEmucap,frame".into(), "42".into()),
        ])
        .with_nonblocking(&["T05hwbreak:00100000;idx:2"]);
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        let response = bridge.handle_request(Request::new(50, "run_frames", json!({"n": 3})));
        let result = response.result.unwrap();
        assert_eq!(
            result["status"], "completed",
            "the buffered stop must not be mis-consumed as the frames result"
        );
        assert_eq!(result["frames"], 3);
        assert_eq!(result["state"], "running");
        assert!(!bridge.frozen, "frozen must stay false after a completed run");
        // The buffered stop was drained to the event queue, not returned as the frames result.
        assert_eq!(bridge.events.len(), 1);
        assert_eq!(bridge.events[0]["raw"], "T05hwbreak:00100000;idx:2");
    }

    #[test]
    fn step_framestep_drains_pre_command_stale_stop() {
        let gdb = FakeGdb::from_pairs(vec![
            ("?".into(), "S05".into()),
            (format!("qEmucap,framestep,{}", hex::encode("2")), "OK".into()),
            ("qEmucap,frame".into(), "7".into()),
        ])
        .with_nonblocking(&["T05hwbreak:00200000;idx:3"]);
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        let response = bridge.handle_request(Request::new(
            51,
            "step",
            json!({"frames": 2, "unit": "frames"}),
        ));
        let result = response.result.unwrap();
        assert_eq!(
            result["status"], "completed",
            "the buffered stop must not be mis-consumed as the framestep result"
        );
        assert_eq!(result["frames"], 2);
        // The buffered stop was drained to the event queue, not returned as the framestep result.
        assert_eq!(bridge.events.len(), 1);
        assert_eq!(bridge.events[0]["raw"], "T05hwbreak:00200000;idx:3");
    }

    #[test]
    fn interrupt_trailing_stop_does_not_produce_phantom_event() {
        // pause() → interrupt() = 0x03 break + `?`, so the stub emits two S05 stops; interrupt()
        // consumes one as its reply and leaves the other buffered (modeled by with_interrupt_echo,
        // which enqueues it *after* the interrupt). That trailing stop is our own interrupt echo,
        // not an async game event — the counter must suppress it so it never surfaces as a phantom.
        let gdb = FakeGdb::with(&[("?", "S05"), ("qEmucap,pollreset", "NONE")])
            .with_interrupt_echo(&["S05"]);
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        bridge.frozen = false; // core running, so pause() actually injects an interrupt
        bridge.pause().unwrap();
        bridge.resume().unwrap();
        let response = bridge.handle_request(Request::new(70, "poll_events", json!({})));
        let events = response.result.unwrap()["events"].as_array().unwrap().clone();
        assert!(
            events.is_empty(),
            "the interrupt echo must not surface as a phantom event: {events:?}"
        );
    }

    #[test]
    fn pause_preserves_real_bp_hit_buffered_before_interrupt() {
        // A pause_on_hit BP fired and its stop is buffered just before pause() injects an interrupt.
        // The echo is S05 — indistinguishable by signal from the real hit — so pause() must drain
        // the real hit to the event queue BEFORE arming the counter; only the interrupt's own echo
        // then remains for the counter to suppress. Counting FIFO-first instead would drop the real
        // hit while the bare echo surfaced as a phantom.
        let regs = i386_regs_hex(&[("eip", 0x1234), ("cs", 0)]);
        let gdb = FakeGdb::from_pairs(vec![
            ("?".into(), "S05".into()),
            ("qEmucap,pollreset".into(), "NONE".into()),
            ("g".into(), regs),
        ])
        .with_nonblocking(&["T05hwbreak:00100000;idx:2"]) // real hit already buffered
        .with_interrupt_echo(&["S05"]); // interrupt's own trailing echo, enqueued after interrupt
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        bridge.frozen = false; // core running, so pause() actually injects an interrupt
        bridge.pause().unwrap();
        bridge.resume().unwrap();
        let response = bridge.handle_request(Request::new(80, "poll_events", json!({})));
        let events = response.result.unwrap()["events"].as_array().unwrap().clone();
        assert_eq!(
            events.len(),
            1,
            "the real BP hit must surface and the echo must not: {events:?}"
        );
        assert_eq!(events[0]["raw"], "T05hwbreak:00100000;idx:2");
        assert_eq!(events[0]["type"], "breakpoint_hit");
    }

    #[test]
    fn drain_immediate_stops_does_not_return_suppressed_echo() {
        // A buffered interrupt echo (counted in pending_interrupt_stops) that lands as a frame
        // command's immediate stop must be suppressed by note_stop, not returned as the frame's
        // stop result — otherwise a completed frame is mis-reported as interrupted+frozen.
        let gdb = FakeGdb::with(&[("?", "S05")]).with_nonblocking(&["S05"]);
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        bridge.pending_interrupt_stops = 1;
        let stop = bridge.drain_immediate_stops().unwrap();
        assert_eq!(
            stop, None,
            "a suppressed interrupt echo must not be returned as a frame stop"
        );
        assert!(
            bridge.events.is_empty(),
            "the echo must not surface as an event either"
        );
        assert_eq!(bridge.pending_interrupt_stops, 0, "the echo was consumed");
    }

    #[test]
    fn load_state_restores_save_items_memory_and_registers() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state.zip");
        let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234)]);
        write_test_state(&state, &regs);
        let mut bridge = Bridge::new(StateLoadGdb::new(regs), BridgeEnv::default());

        let response = bridge.handle_request(Request::new(
            25,
            "load_state",
            json!({"path": state.display().to_string()}),
        ));
        let result = response.result.unwrap();
        assert_eq!(result["format"], STATE_FORMAT);
        assert_eq!(result["regions"], 1);
        assert_eq!(result["save_items_restored"], 1);
        assert_eq!(result["restore_strategy"], "lua_register_load_hold");
        assert_eq!(result["post_restore_instruction_exact"], true);
        assert_eq!(bridge.gdb.writes, vec!["Ma0000,2:aabb"]);
        assert_eq!(bridge.gdb.load_items_dirs.len(), 1);
    }

    #[test]
    fn probe_restores_state_and_uses_lua_register_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state.zip");
        let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234)]);
        write_test_state(&state, &regs);
        let mut bridge = Bridge::new(StateLoadGdb::new(regs.clone()), BridgeEnv::default());

        let response = bridge.handle_request(Request::new(
            26,
            "probe",
            json!({
                "state": state.display().to_string(),
                "frame": 3,
                "memory_type": "tvram",
                "address": 0,
                "length": 2
            }),
        ));
        let result = response.result.unwrap();
        assert_eq!(result["hex"], "cafe");
        assert_eq!(result["frame"], 3);
        assert_eq!(result["save_items_restored"], 1);
        assert_eq!(bridge.gdb.writes, vec!["Ma0000,2:aabb"]);
        assert_eq!(bridge.gdb.regprobe_specs, vec![format!("{regs}|3|a0000|2")]);
    }

    #[test]
    fn get_state_decodes_i386_register_packet_and_segmented_pc() {
        let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234), ("esp", 0xAA55)]);
        let mut bridge = Bridge::new(
            FakeGdb::with(&[("?", "S05"), ("g", &regs)]),
            BridgeEnv::default(),
        );
        let response = bridge.handle_request(Request::new(4, "get_state", json!({})));
        let state = &response.result.unwrap()["state"];
        assert_eq!(state["cpu.eip"], 0x8000);
        assert_eq!(state["cpu.esp"], 0xAA55);
        assert_eq!(state["cpu.pc"], 0x12340 + 0x8000);
    }

    #[test]
    fn get_rom_info_hashes_content_path() {
        let tmp = tempfile::tempdir().unwrap();
        let disk = tmp.path().join("game.hdi");
        std::fs::write(&disk, b"pc98").unwrap();
        let env = BridgeEnv {
            content: Some(disk.clone()),
            ..Default::default()
        };
        let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), env);
        let response = bridge.handle_request(Request::new(5, "get_rom_info", json!({})));
        let result = response.result.unwrap();
        assert_eq!(result["name"], "game.hdi");
        assert_eq!(result["size"], 4);
        assert_eq!(result["media_type"], "hdi");
        assert_eq!(result["sha1"], sha1_file(&disk).unwrap());
    }

    #[test]
    fn unknown_method_uses_protocol_unknown_method_kind() {
        let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), BridgeEnv::default());
        let response = bridge.handle_request(Request::new(6, "not_a_method", json!({})));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "unknown_method");
    }

    struct SnapshotGdb;

    impl GdbTransport for SnapshotGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            if payload == "qEmucap,frame" {
                return Ok("42".into());
            }
            let prefix = "qEmucap,snapshot,";
            if let Some(hex_path) = payload.strip_prefix(prefix) {
                let bytes = hex::decode(hex_path)
                    .map_err(|_| BridgeError::Emulator("bad snapshot path hex".into()))?;
                let path = String::from_utf8(bytes)
                    .map_err(|_| BridgeError::Emulator("bad snapshot path utf8".into()))?;
                std::fs::write(path, b"\x89PNG\r\n\x1a\nfake")?;
                return Ok("OK".into());
            }
            Err(BridgeError::Emulator(format!("unexpected call: {payload}")))
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }
    }

    #[test]
    fn screenshot_returns_png_base64_from_lua_snapshot() {
        let mut bridge = Bridge::new(SnapshotGdb, BridgeEnv::default());
        let response = bridge.handle_request(Request::new(13, "screenshot", json!({})));
        let result = response.result.unwrap();
        assert_eq!(
            result["png_base64"],
            base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfake")
        );
        assert_eq!(
            result["sha256"],
            format!("{:x}", Sha256::digest(b"\x89PNG\r\n\x1a\nfake"))
        );
        assert_eq!(result["byte_len"], 12);
        assert_eq!(result["state"], "frozen");
        assert_eq!(result["frame_before"], 42);
        assert_eq!(result["frame_after"], 42);
        assert_eq!(result["frame_stable"], true);
        assert_eq!(result["freshness"], "unverified");
        assert_eq!(result["frame_binding"], "unverified");
    }

    #[test]
    fn rsp_client_sends_acknowledged_packet_and_decodes_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut b = [0u8; 1];
                stream.read_exact(&mut b).unwrap();
                request.push(b[0]);
                if request.len() >= 4 && request[request.len() - 3] == b'#' {
                    break;
                }
            }
            assert_eq!(std::str::from_utf8(&request).unwrap(), "$g#67");
            stream.write_all(b"+").unwrap();
            let payload = b"OK";
            let frame = format!(
                "$OK#{:02x}",
                payload.iter().fold(0u8, |sum, b| sum.wrapping_add(*b))
            );
            stream.write_all(frame.as_bytes()).unwrap();
            let mut ack = [0u8; 1];
            stream.read_exact(&mut ack).unwrap();
            assert_eq!(ack[0], b'+');
        });

        let mut client = GdbRspClient::connect(
            "127.0.0.1",
            port,
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(client.send("g").unwrap(), "OK");
        handle.join().unwrap();
    }

    // P1: stale async stop이 데이터 명령의 응답 자리에 오배달돼도 send_cmd가 이벤트 큐로
    // 걷어내고 진짜 응답을 이어 읽어 off-by-one 디싱크를 막는지 검증한다.
    #[derive(Default)]
    struct StaleStopGdb {
        stale: Option<String>,
        reply: String,
    }

    impl GdbTransport for StaleStopGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            Ok(self.stale.take().unwrap_or_else(|| self.reply.clone()))
        }

        fn recv_reply(&mut self) -> BridgeResult<String> {
            Ok(self.reply.clone())
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }
    }

    #[test]
    fn send_cmd_demuxes_stale_async_stop_ahead_of_data_reply() {
        let gdb = StaleStopGdb {
            stale: Some("T05".into()),
            reply: "OK".into(),
        };
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        let resp = bridge
            .send_cmd("qEmucap,setinput,656e746572")
            .expect("send_cmd returns the real reply");
        assert_eq!(resp, "OK");
        assert_eq!(bridge.events.len(), 1);
        assert_eq!(bridge.events[0]["type"], "stop");
    }

    // F1: m 읽기(read_abs_hex → read_memory/dump_memory/save_state/find_pattern/probe/call_stack)가
    // send_cmd demux를 경유해, 응답 앞에 낀 stale async stop을 이벤트 큐로 걷어내고 진짜 hex
    // 응답을 반환하는지 검증한다. raw send면 stop이 hex 자리에 오배달돼 디코드 실패 + off-by-one.
    struct StaleStopReadGdb {
        stale: Option<String>,
        hex: String,
        reads: Vec<String>,
    }

    impl GdbTransport for StaleStopReadGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            self.reads.push(payload.into());
            Ok(self.stale.take().unwrap_or_else(|| self.hex.clone()))
        }

        fn recv_reply(&mut self) -> BridgeResult<String> {
            Ok(self.hex.clone())
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }
    }

    #[test]
    fn read_abs_hex_demuxes_stale_stop_ahead_of_memory_reply() {
        let gdb = StaleStopReadGdb {
            stale: Some("T05hwbreak:00100000;idx:1".into()),
            hex: "deadbeef".into(),
            reads: Vec::new(),
        };
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        let hex = bridge
            .read_abs_hex(0x1234, 4)
            .expect("read_abs_hex returns the real hex reply");
        assert_eq!(hex, "deadbeef");
        // stale stop이 hex 응답 자리에 오배달되지 않고 이벤트 큐로 걷혔다.
        assert_eq!(bridge.events.len(), 1);
        assert_eq!(bridge.events[0]["raw"], "T05hwbreak:00100000;idx:1");
        // 데이터 명령이 실제 m 읽기였는지(demux는 m 자체는 건드리지 않는다).
        assert!(
            bridge.gdb.reads.iter().any(|c| c.starts_with('m')),
            "issued an m read: {:?}",
            bridge.gdb.reads
        );
    }

    #[test]
    fn send_cmd_drains_stale_ok_ahead_of_register_read() {
        // 트레이싱 중 runframes가 frame-target에 도달한 순간 BP도 히트하면, frame notifier의 완료 "OK"와
        // note_breakpoint의 BP stop이 하나의 runframes에 이중 응답한다. 브리지가 그중 하나를 소비하면 나머지
        // stale "OK"가 다음 데이터 명령(g=레지스터)의 응답 자리에 오배달돼 off-by-one desync된다
        // (get_state가 raw_register_bytes로 깨지고 이후 traceflush가 register 패킷을 받음). send_cmd는 데이터
        // 읽기 앞의 stale "OK"를 걷어내고 진짜 hex를 재읽기해야 한다.
        let gdb = StaleStopReadGdb {
            stale: Some("OK".into()),
            hex: "00ff0000160000008080".into(), // i386 레지스터 hex(축약)
            reads: Vec::new(),
        };
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        let resp = bridge.send_cmd_data("g").expect("g returns register hex");
        assert_eq!(
            resp, "00ff0000160000008080",
            "g는 stale OK가 아니라 레지스터 hex를 받아야(desync 없음)"
        );
    }

    #[test]
    fn send_cmd_data_drains_stale_ok_ahead_of_frame_read() {
        // run_frames/step은 frames_op 직후 qEmucap,frame(current_frame)을 필수로 부른다. 이 경로도
        // send_cmd_data를 타야 stale bare "OK"가 frame 응답 자리에 오배달(→ 이후 g가 프레임 숫자를 레지스터로
        // 오소비)되는 것을 막는다. g/m 하드코딩이 아닌 명령-의도 기반이라 frame도 커버된다.
        let gdb = StaleStopReadGdb {
            stale: Some("OK".into()),
            hex: "2028".into(), // qEmucap,frame의 10진 프레임 번호
            reads: Vec::new(),
        };
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        let resp = bridge
            .send_cmd_data("qEmucap,frame")
            .expect("frame returns the decimal frame number");
        assert_eq!(resp, "2028", "frame은 stale OK가 아니라 프레임 번호를 받아야");
    }

    #[test]
    fn send_cmd_data_keeps_ok_pipe_data_reply() {
        // 회귀 가드: saveitems/loaditems/regload은 성공 시 "OK|<data>"를 반환한다 — bare "OK"가 아니므로
        // send_cmd_data가 이를 stale로 오인해 드레인하면 안 된다(그러면 hang). "OK|..."는 그대로 반환.
        let gdb = StaleStopReadGdb {
            stale: None,
            hex: "OK|3|0".into(),
            reads: Vec::new(),
        };
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        let resp = bridge
            .send_cmd_data("qEmucap,saveitems,2f74")
            .expect("saveitems returns OK|data");
        assert_eq!(resp, "OK|3|0", "\"OK|...\"는 유효 데이터라 드레인 금지");
    }

    // F3: s(instruction step)는 응답 자체가 stop이라 send_cmd demux가 스킵된다. 스텝 직전에
    // 버퍼의 stale async stop을 걷어내(note_stop) s의 응답 자리 오배달을 막고, 진짜 스텝 완료
    // stop을 응답으로 받아(re-read) instruction step이 유지되는지 검증한다.
    struct StepStaleGdb {
        buffered: VecDeque<String>,
        step_reply: String,
        steps: usize,
    }

    impl GdbTransport for StepStaleGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            if payload == "s" {
                self.steps += 1;
                return Ok(self.step_reply.clone());
            }
            Err(BridgeError::Emulator(format!("unexpected call: {payload}")))
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }

        fn recv_nonblocking(&mut self) -> BridgeResult<Option<String>> {
            Ok(self.buffered.pop_front())
        }
    }

    #[test]
    fn step_instruction_drains_pre_command_stale_stop() {
        let gdb = StepStaleGdb {
            buffered: VecDeque::from(vec!["T05hwbreak:00100000;idx:2".to_string()]),
            step_reply: "S05".into(),
            steps: 0,
        };
        let mut bridge = Bridge::new(gdb, BridgeEnv::default());
        let response =
            bridge.handle_request(Request::new(40, "step_instructions", json!({"count": 1})));
        assert!(response.ok, "instruction step still completes");
        let result = response.result.unwrap();
        assert_eq!(result["unit"], "instructions");
        assert_eq!(result["count"], 1);
        // stale stop이 s의 응답 자리에 오배달되지 않고 이벤트 큐로 걷혔다.
        assert_eq!(bridge.events.len(), 1);
        assert_eq!(bridge.events[0]["raw"], "T05hwbreak:00100000;idx:2");
        // 스텝은 실제로 한 번 실행됐다(stale를 스텝 완료로 오인하지 않음).
        assert_eq!(bridge.gdb.steps, 1);
    }

    // P2: 머신 ioport에 없는 버튼을 눌렀을 때, 어느 버튼이 없고 무엇이 가능한지 이름을 붙여
    // 반환하는지 검증한다(맨몸 E08 패스스루 금지).
    #[test]
    fn set_input_names_unavailable_button_and_lists_machine_fields() {
        let fake = FakeGdb::from_pairs(vec![
            ("?".into(), "S05".into()),
            (
                format!("qEmucap,setinput,{}", hex::encode("help")),
                "E08:help".into(),
            ),
            ("qEmucap,inputfields".into(), "a,b,enter,esc,space".into()),
        ]);
        let mut bridge = Bridge::new(fake, BridgeEnv::default());
        let response =
            bridge.handle_request(Request::new(30, "set_input", json!({"buttons": ["help"]})));
        assert!(!response.ok);
        let msg = response.error.unwrap().message;
        assert!(msg.contains("help"), "names the unavailable button: {msg}");
        assert!(
            msg.contains("enter") && msg.contains("space"),
            "lists the machine-registered fields: {msg}"
        );
    }

    // P3: 트레이스 없이 정지 상태의 BP(EBP) 체인을 걸어 호출 스택을 복원하고, 어느 방법을
    // 썼는지 method 필드로 알리는지 검증한다.
    struct CallStackFpGdb {
        regs_hex: String,
        mem: BTreeMap<u64, u64>,
    }

    impl GdbTransport for CallStackFpGdb {
        fn send(&mut self, payload: &str) -> BridgeResult<String> {
            if payload == "?" {
                return Ok("S05".into());
            }
            if payload == "g" {
                return Ok(self.regs_hex.clone());
            }
            if let Some(rest) = payload.strip_prefix('m') {
                let (addr_hex, len_hex) = rest
                    .split_once(',')
                    .ok_or_else(|| BridgeError::Emulator(format!("bad read: {payload}")))?;
                let addr = u64::from_str_radix(addr_hex, 16)
                    .map_err(|_| BridgeError::Emulator(format!("bad addr: {payload}")))?;
                let len = usize::from_str_radix(len_hex, 16)
                    .map_err(|_| BridgeError::Emulator(format!("bad len: {payload}")))?;
                let value = self.mem.get(&addr).copied().unwrap_or(0);
                return Ok(hex::encode(&value.to_le_bytes()[..len]));
            }
            Err(BridgeError::Emulator(format!("unexpected call: {payload}")))
        }

        fn send_no_reply(&mut self, _payload: &str) -> BridgeResult<()> {
            Ok(())
        }

        fn interrupt(&mut self) -> BridgeResult<String> {
            Ok("S05".into())
        }
    }

    #[test]
    fn call_stack_walks_frame_pointer_chain_without_trace() {
        // eip를 16비트를 넘겨 protected32(포인터 4바이트, 평면 SS)로 판정되게 한다.
        let regs = i386_regs_hex(&[("eip", 0x0010_0000), ("ebp", 0x1000), ("esp", 0x0FF0)]);
        let mem = BTreeMap::from([
            (0x1000u64, 0x1100u64), // saved_bp
            (0x1004u64, 0xAAAAu64), // ret addr, frame 1
            (0x1100u64, 0x0000u64), // saved_bp = 0 → 체인 종료
            (0x1104u64, 0xBBBBu64), // ret addr, frame 2
        ]);
        let mut bridge = Bridge::new(
            CallStackFpGdb {
                regs_hex: regs,
                mem,
            },
            BridgeEnv::default(),
        );
        let response = bridge.handle_request(Request::new(31, "call_stack", json!({})));
        let result = response.result.unwrap();
        assert_eq!(result["method"], "frame_pointer");
        assert_eq!(result["call_stack"], json!([0xAAAA, 0xBBBB]));
        assert_eq!(result["depth"], 2);
    }
}
