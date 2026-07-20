//! PCSX2 PINE ↔ emucap wire-protocol bridge.
//!
//! The supported backend is the pinned PCSX2 fork under `adapters/pcsx2`. Stock PINE provides
//! process discovery and game metadata, while the fork adds terminally acknowledged operations for
//! CPU-thread memory access, pause/resume, frame advance, EE registers, disassembly, and path-based
//! savestates. Host API 3 also owns frame-counted controller input, synchronous GS capture,
//! debugger stops, call-stack capture, and reset.
//! The bridge deliberately refuses a stock or older PINE server instead of advertising weaker
//! timing semantics under the same method names.

mod debug;
mod input;
mod memory;
mod video;

use std::collections::BTreeMap;
use std::io::{Read, Write};
#[cfg(windows)]
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};
use debug::Pcsx2Breakpoint;

const PINE_MAX_REPLY: usize = 450_000;
const PCSX2_EE_RAM_SIZE: u64 = 0x0200_0000;
const MAX_MEMORY_TRANSFER: usize = 0x2_0000;
const MAX_INPUT_FRAMES: u64 = 240;
pub const REQUIRED_HOST_API: u32 = 3;

const MSG_VERSION: u8 = 0x08;
const MSG_TITLE: u8 = 0x0b;
const MSG_ID: u8 = 0x0c;
const MSG_UUID: u8 = 0x0d;
const MSG_GAME_VERSION: u8 = 0x0e;
const MSG_STATUS: u8 = 0x0f;

const MSG_EMUCAP_VERSION: u8 = 0x80;
const MSG_EMUCAP_PAUSE: u8 = 0x81;
const MSG_EMUCAP_RESUME: u8 = 0x82;
const MSG_EMUCAP_FRAME_ADVANCE: u8 = 0x83;
const MSG_EMUCAP_EE_STATE: u8 = 0x84;
const MSG_EMUCAP_READ_BYTES: u8 = 0x85;
const MSG_EMUCAP_WRITE_BYTES: u8 = 0x86;
const MSG_EMUCAP_DISASSEMBLE: u8 = 0x87;
const MSG_EMUCAP_SAVE_STATE: u8 = 0x88;
const MSG_EMUCAP_LOAD_STATE: u8 = 0x89;
const MSG_EMUCAP_SET_INPUT: u8 = 0x8a;
const MSG_EMUCAP_INPUT_STATUS: u8 = 0x8b;
const MSG_EMUCAP_PRESS_BUTTONS: u8 = 0x8c;
const MSG_EMUCAP_SCREENSHOT: u8 = 0x8d;
const MSG_EMUCAP_SET_BREAKPOINT: u8 = 0x8e;
const MSG_EMUCAP_CLEAR_BREAKPOINT: u8 = 0x8f;
const MSG_EMUCAP_POLL_EVENTS: u8 = 0x90;
const MSG_EMUCAP_CALL_STACK: u8 = 0x91;
const MSG_EMUCAP_RESET: u8 = 0x92;

const METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "read_memory",
    "write_memory",
    "find_pattern",
    "dump_memory",
    "get_state",
    "pause",
    "resume",
    "step",
    "disassemble",
    "save_state",
    "load_state",
    "screenshot",
    "set_input",
    "press_buttons",
    "set_breakpoint",
    "clear_breakpoint",
    "clear_all_breakpoints",
    "list_breakpoints",
    "poll_events",
    "call_stack",
    "reset",
];

const ACTIVE_EXCEPTIONS: &[&str] = &[
    "pcsx2.execution.instruction-step-absent",
    "pcsx2.state-save.frozen-only",
    "pcsx2.state-load.frozen-only",
    "pcsx2.input-hold.port-zero-only",
    "pcsx2.input-pulse.constraints",
    "pcsx2.breakpoint.pausing-subset",
    "pcsx2.call-stack.frozen-best-effort",
];

const UNSUPPORTED_METHODS: &[&str] = &[
    "run_frames",
    "watch_register",
    "set_trace",
    "get_trace",
    "break_on_reset",
    "probe",
];

#[derive(Debug, thiserror::Error)]
pub enum Pcsx2BridgeError {
    #[error("{0}")]
    BadParams(String),
    #[error("{0}")]
    BadState(String),
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("unsupported on ps2: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Emulator(String),
    #[error("{0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

type BridgeResult<T> = Result<T, Pcsx2BridgeError>;

pub trait PineTransport {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>>;

    /// True once the current PINE stream can no longer preserve frame boundaries. A caller must
    /// replace the bridge/backend generation rather than retrying on the same stream.
    fn is_terminal(&self) -> bool {
        false
    }
}

enum PineStream {
    #[cfg(windows)]
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for PineStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for PineStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

pub struct PineSocket {
    stream: PineStream,
    terminal: bool,
}

impl PineSocket {
    pub fn connect(slot: u16, socket_path: Option<&Path>, timeout: Duration) -> BridgeResult<Self> {
        #[cfg(windows)]
        let stream = {
            let _ = socket_path;
            let stream = TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], slot)),
                timeout,
            )?;
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            PineStream::Tcp(stream)
        };

        #[cfg(unix)]
        let stream = {
            let _ = slot;
            let path = socket_path.ok_or_else(|| {
                Pcsx2BridgeError::BadParams(
                    "PINE socket path is required on this platform".to_string(),
                )
            })?;
            let stream = UnixStream::connect(path)?;
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            PineStream::Unix(stream)
        };

        Ok(Self {
            stream,
            terminal: false,
        })
    }
}

impl PineTransport for PineSocket {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>> {
        if self.terminal {
            return Err(Pcsx2BridgeError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "PINE transport is terminal",
            )));
        }
        let outcome = (|| {
            let packet_len = request
                .len()
                .checked_add(4)
                .and_then(|length| u32::try_from(length).ok())
                .ok_or_else(|| Pcsx2BridgeError::BadParams("PINE request is too large".into()))?;
            self.stream.write_all(&packet_len.to_le_bytes())?;
            self.stream.write_all(request)?;
            self.stream.flush()?;

            let mut header = [0u8; 5];
            self.stream.read_exact(&mut header)?;
            let reply_len =
                u32::from_le_bytes(header[..4].try_into().expect("four bytes")) as usize;
            if !(5..=PINE_MAX_REPLY).contains(&reply_len) {
                return Err(Pcsx2BridgeError::Protocol(format!(
                    "PINE reply length {reply_len} is outside 5..={PINE_MAX_REPLY}"
                )));
            }
            let mut payload = vec![0u8; reply_len - 5];
            self.stream.read_exact(&mut payload)?;
            if header[4] != 0 {
                return Err(Pcsx2BridgeError::Emulator(
                    "PCSX2 rejected the PINE command".into(),
                ));
            }
            Ok(payload)
        })();
        if outcome.as_ref().is_err_and(|error| {
            matches!(
                error,
                Pcsx2BridgeError::Io(_) | Pcsx2BridgeError::Protocol(_)
            )
        }) {
            self.terminal = true;
        }
        outcome
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }
}

pub struct Pcsx2Bridge<T> {
    pine: T,
    content: Option<PathBuf>,
    content_sha1: ContentSha1,
    name: Option<String>,
    session_token: Option<String>,
    launch_id: Option<String>,
    host_api: u32,
    next_breakpoint_id: u64,
    breakpoints: BTreeMap<u64, Pcsx2Breakpoint>,
}

enum ContentSha1 {
    Unavailable,
    Pending(std::thread::JoinHandle<Result<String, String>>),
    Ready(Result<String, String>),
}

impl<T: PineTransport> Pcsx2Bridge<T> {
    pub fn new(pine: T) -> BridgeResult<Self> {
        let content = std::env::var_os("EMUCAP_CONTENT").map(PathBuf::from);
        let mut bridge = Self::with_identity(
            pine,
            content.clone(),
            std::env::var("EMUCAP_NAME").ok(),
            std::env::var("EMUCAP_SESSION_TOKEN").ok(),
        )?;
        if let Some(path) = content {
            bridge.content_sha1 = ContentSha1::Pending(std::thread::spawn(move || {
                crate::rom::sha1_of_file(&path).map_err(|error| error.to_string())
            }));
        }
        bridge.launch_id = std::env::var("EMUCAP_LAUNCH_ID").ok();
        Ok(bridge)
    }

    pub fn with_identity(
        pine: T,
        content: Option<PathBuf>,
        name: Option<String>,
        session_token: Option<String>,
    ) -> BridgeResult<Self> {
        let mut bridge = Self {
            pine,
            content,
            content_sha1: ContentSha1::Unavailable,
            name,
            session_token,
            launch_id: None,
            host_api: 0,
            next_breakpoint_id: 1,
            breakpoints: BTreeMap::new(),
        };
        bridge.host_api = bridge.read_u32_command(MSG_EMUCAP_VERSION, &[])?;
        if bridge.host_api != REQUIRED_HOST_API {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "pcsx2-patch-required: host API {} is incompatible; expected {}",
                bridge.host_api, REQUIRED_HOST_API
            )));
        }
        Ok(bridge)
    }

    pub fn backend_terminal(&self) -> bool {
        self.pine.is_terminal()
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id;
        let result = match request.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "get_rom_info" => self.get_rom_info(),
            "read_memory" => self.read_memory(&request.params),
            "write_memory" => self.write_memory(&request.params),
            "find_pattern" => self.find_pattern(&request.params),
            "dump_memory" => self.dump_memory(&request.params),
            "get_state" => self.get_state(),
            "pause" => self.pause(),
            "resume" => self.resume(),
            "step" => self.step(&request.params),
            "disassemble" => self.disassemble(&request.params),
            "save_state" => self.save_state(&request.params),
            "load_state" => self.load_state(&request.params),
            "screenshot" => self.screenshot(),
            "set_input" => self.set_input(&request.params),
            "press_buttons" => self.press_buttons(&request.params),
            "set_breakpoint" => self.set_breakpoint(&request.params),
            "clear_breakpoint" => self.clear_breakpoint(&request.params),
            "clear_all_breakpoints" => self.clear_all_breakpoints(),
            "list_breakpoints" => self.list_breakpoints(),
            "poll_events" => self.poll_events(),
            "call_stack" => self.call_stack(),
            "reset" => self.reset(),
            other if UNSUPPORTED_METHODS.contains(&other) => {
                Err(Pcsx2BridgeError::Unsupported(other.into()))
            }
            other => Err(Pcsx2BridgeError::UnknownMethod(other.into())),
        };
        match result {
            Ok(value) => Response {
                id,
                ok: true,
                result: Some(value),
                error: None,
            },
            Err(error) => Response {
                id,
                ok: false,
                result: None,
                error: Some(ProtocolError {
                    kind: error_kind(&error).into(),
                    message: error.to_string(),
                }),
            },
        }
    }

    fn command(&mut self, opcode: u8, params: &[u8]) -> BridgeResult<Vec<u8>> {
        let mut request = Vec::with_capacity(params.len() + 1);
        request.push(opcode);
        request.extend_from_slice(params);
        self.pine.transact(&request)
    }

    fn read_u32_command(&mut self, opcode: u8, params: &[u8]) -> BridgeResult<u32> {
        let payload = self.command(opcode, params)?;
        read_u32_exact(&payload)
    }

    fn read_string_command(&mut self, opcode: u8) -> BridgeResult<String> {
        let payload = self.command(opcode, &[])?;
        if payload.len() < 4 {
            return Err(Pcsx2BridgeError::Protocol(
                "PINE string reply is shorter than its length field".into(),
            ));
        }
        let length = u32::from_le_bytes(payload[..4].try_into().expect("four bytes")) as usize;
        if length == 0 || payload.len() != length + 4 {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "PINE string reply length mismatch: field={length}, payload={}",
                payload.len() - 4
            )));
        }
        let bytes = &payload[4..];
        if bytes.last() != Some(&0) {
            return Err(Pcsx2BridgeError::Protocol(
                "PINE string reply is not NUL-terminated".into(),
            ));
        }
        std::str::from_utf8(&bytes[..bytes.len() - 1])
            .map(str::to_owned)
            .map_err(|error| {
                Pcsx2BridgeError::Protocol(format!("PINE string is not UTF-8: {error}"))
            })
    }

    fn hello(&self) -> BridgeResult<Value> {
        let mut value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "ps2",
            "adapter": "pcsx2-rust-pine",
            "backend": "pcsx2-pine-fork",
            "debugger": true,
            "methods": METHODS,
            "memory_types": ["ee"],
            "input_buttons": pcsx2_input_buttons_json(),
            "pcsx2_host_api": self.host_api,
            "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS),
            "execution_limits": {
                "max_sync_advance_count": 15,
                "max_sync_operation_ms": 10000,
                "input_pulse_max_frames": MAX_INPUT_FRAMES,
            },
            "capability_notes": capability_notes(),
        });
        let object = value.as_object_mut().expect("hello object");
        if let Some(name) = &self.name {
            object.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.session_token {
            object.insert("session_token".into(), json!(token));
        }
        if let Some(launch_id) = &self.launch_id {
            object.insert("launch_id".into(), json!(launch_id));
        }
        Ok(value)
    }

    fn status(&mut self) -> BridgeResult<Value> {
        let state = self.emulator_state()?;
        let version = self.read_string_command(MSG_VERSION)?;
        let input_override = self.input_override_info()?;
        Ok(json!({
            "connected": true,
            "system": "ps2",
            "adapter": "pcsx2-rust-pine",
            "backend": "pcsx2-pine-fork",
            "debugger": true,
            "state": state,
            "methods": METHODS,
            "memory_types": ["ee"],
            "input_buttons": pcsx2_input_buttons_json(),
            "input_override": input_override,
            "pcsx2_host_api": self.host_api,
            "pcsx2_version": version,
            "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS),
            "execution_limits": {
                "max_sync_advance_count": 15,
                "max_sync_operation_ms": 10000,
                "input_pulse_max_frames": MAX_INPUT_FRAMES,
            },
            "capability_notes": capability_notes(),
        }))
    }

    fn emulator_state(&mut self) -> BridgeResult<&'static str> {
        match self.read_u32_command(MSG_STATUS, &[])? {
            0 => Ok("running"),
            1 => Ok("frozen"),
            2 => Ok("shutdown"),
            value => Err(Pcsx2BridgeError::Protocol(format!(
                "unknown PCSX2 state value: {value}"
            ))),
        }
    }

    fn get_rom_info(&mut self) -> BridgeResult<Value> {
        let title = self.read_string_command(MSG_TITLE)?;
        let serial = self.read_string_command(MSG_ID)?;
        let crc = self.read_string_command(MSG_UUID)?;
        let game_version = self.read_string_command(MSG_GAME_VERSION)?;
        let (content_sha1, hash_status, hash_error) = self.poll_content_sha1();
        Ok(json!({
            "system": "ps2",
            "title": title,
            "serial": serial,
            "disc_crc": crc,
            "game_version": game_version,
            "path": self.content.as_deref().map(|path| path.display().to_string()),
            "sha1": content_sha1,
            "rom_sha1": content_sha1,
            "hash_status": hash_status,
            "hash_error": hash_error,
        }))
    }

    fn poll_content_sha1(&mut self) -> (Option<String>, &'static str, Option<String>) {
        let finished = matches!(
            &self.content_sha1,
            ContentSha1::Pending(handle) if handle.is_finished()
        );
        if finished {
            let pending = std::mem::replace(&mut self.content_sha1, ContentSha1::Unavailable);
            if let ContentSha1::Pending(handle) = pending {
                self.content_sha1 = ContentSha1::Ready(
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("content hash worker panicked".into())),
                );
            }
        }
        match &self.content_sha1 {
            ContentSha1::Unavailable => (None, "unavailable", None),
            ContentSha1::Pending(_) => (None, "pending", None),
            ContentSha1::Ready(Ok(hash)) => (Some(hash.clone()), "ready", None),
            ContentSha1::Ready(Err(error)) => (None, "error", Some(error.clone())),
        }
    }

    fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let address = routed_ee_address(params, required_num(params, "length")?)?;
        let length = required_num(params, "length")? as usize;
        if length > MAX_MEMORY_TRANSFER {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "read length {length:#x} exceeds {MAX_MEMORY_TRANSFER:#x}; split the request"
            )));
        }
        let mut body = Vec::with_capacity(8);
        body.extend_from_slice(&address.to_le_bytes());
        body.extend_from_slice(&(length as u32).to_le_bytes());
        let payload = self.command(MSG_EMUCAP_READ_BYTES, &body)?;
        if payload.len() != length {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "PCSX2 returned {} bytes for a {length}-byte read",
                payload.len()
            )));
        }
        Ok(json!({ "hex": hex::encode(payload) }))
    }

    fn write_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let raw = required_str(params, "hex")?;
        if raw.len() % 2 != 0 {
            return Err(Pcsx2BridgeError::BadParams(
                "hex must have even length".into(),
            ));
        }
        let bytes = hex::decode(raw)
            .map_err(|_| Pcsx2BridgeError::BadParams("hex decode failed".into()))?;
        if bytes.len() > MAX_MEMORY_TRANSFER {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "write length {:#x} exceeds {MAX_MEMORY_TRANSFER:#x}; split the request",
                bytes.len()
            )));
        }
        let address = routed_ee_address(params, bytes.len() as u64)?;
        let mut body = Vec::with_capacity(8 + bytes.len());
        body.extend_from_slice(&address.to_le_bytes());
        body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(&bytes);
        let written = self.read_u32_command(MSG_EMUCAP_WRITE_BYTES, &body)? as usize;
        if written != bytes.len() {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "PCSX2 acknowledged {written} bytes for a {}-byte write",
                bytes.len()
            )));
        }
        Ok(json!({ "written": written }))
    }

    fn pause(&mut self) -> BridgeResult<Value> {
        self.command(MSG_EMUCAP_PAUSE, &[])?;
        Ok(json!({ "state": "frozen", "status": "completed" }))
    }

    fn resume(&mut self) -> BridgeResult<Value> {
        self.command(MSG_EMUCAP_RESUME, &[])?;
        Ok(json!({ "state": "running", "status": "completed" }))
    }

    fn step(&mut self, params: &Value) -> BridgeResult<Value> {
        match params.get("unit").and_then(Value::as_str) {
            None | Some("frames") => {}
            Some(unit) => {
                return Err(Pcsx2BridgeError::Unsupported(format!(
                    "PCSX2 currently supports only step unit `frames`, got `{unit}`"
                )))
            }
        }
        let count = optional_num(params, "count")?
            .or(optional_num(params, "n")?)
            .or(optional_num(params, "frames")?)
            .unwrap_or(1);
        if !(1..=15).contains(&count) {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "frame step count must be in 1..=15, got {count}"
            )));
        }
        self.require_frozen("step")?;
        self.command(MSG_EMUCAP_FRAME_ADVANCE, &(count as u32).to_le_bytes())?;
        Ok(json!({
            "advanced": count,
            "unit": "frames",
            "state": "frozen",
            "status": "completed",
        }))
    }

    fn get_state(&mut self) -> BridgeResult<Value> {
        let payload = self.command(MSG_EMUCAP_EE_STATE, &[])?;
        const EXPECTED: usize = 4 + (32 * 8) + 8 + 8;
        if payload.len() != EXPECTED {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "EE state reply is {} bytes, expected {EXPECTED}",
                payload.len()
            )));
        }
        let mut cursor = SliceCursor::new(&payload);
        let pc = cursor.u32()? as u64;
        let names = [
            "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5",
            "t6", "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1",
            "gp", "sp", "fp", "ra",
        ];
        let mut state = serde_json::Map::new();
        state.insert("cpu.pc".into(), json!(pc));
        for name in names {
            state.insert(format!("cpu.{name}"), json!(cursor.u64()?));
        }
        state.insert("cpu.hi".into(), json!(cursor.u64()?));
        state.insert("cpu.lo".into(), json!(cursor.u64()?));
        Ok(json!({ "cpu": "ee", "state": Value::Object(state) }))
    }

    fn disassemble(&mut self, params: &Value) -> BridgeResult<Value> {
        let address = required_num_alias(params, &["address", "start"])?;
        let address = u32::try_from(address).map_err(|_| {
            Pcsx2BridgeError::BadParams("disassemble address exceeds the EE address width".into())
        })?;
        if address % 4 != 0 {
            return Err(Pcsx2BridgeError::BadParams(
                "disassemble address must be four-byte aligned".into(),
            ));
        }
        let count = optional_num(params, "count")?.unwrap_or(8);
        if !(1..=256).contains(&count) {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "disassemble count must be in 1..=256, got {count}"
            )));
        }
        let mut body = Vec::with_capacity(8);
        body.extend_from_slice(&address.to_le_bytes());
        body.extend_from_slice(&(count as u32).to_le_bytes());
        let payload = self.command(MSG_EMUCAP_DISASSEMBLE, &body)?;
        let mut cursor = SliceCursor::new(&payload);
        let returned = cursor.u32()? as usize;
        if returned > count as usize {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "PCSX2 returned {returned} instructions for requested count {count}"
            )));
        }
        let mut instructions = Vec::with_capacity(returned);
        for _ in 0..returned {
            let addr = cursor.u32()?;
            let encoding = cursor.u32()?;
            let text_len = cursor.u32()? as usize;
            let text = std::str::from_utf8(cursor.bytes(text_len)?)
                .map_err(|error| {
                    Pcsx2BridgeError::Protocol(format!("disassembly text is not UTF-8: {error}"))
                })?
                .to_owned();
            instructions.push(json!({
                "addr": addr,
                "bytes": hex::encode(encoding.to_le_bytes()),
                "text": text,
            }));
        }
        if !cursor.is_empty() {
            return Err(Pcsx2BridgeError::Protocol(
                "disassembly reply has trailing bytes".into(),
            ));
        }
        Ok(json!({ "cpu": "ee", "instructions": instructions }))
    }

    fn save_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.path_state_command(MSG_EMUCAP_SAVE_STATE, params)?;
        Ok(
            json!({ "saved": required_str(params, "path")?, "state": "frozen", "status": "completed" }),
        )
    }

    fn load_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.path_state_command(MSG_EMUCAP_LOAD_STATE, params)?;
        Ok(
            json!({ "loaded": required_str(params, "path")?, "state": "frozen", "status": "completed" }),
        )
    }

    fn path_state_command(&mut self, opcode: u8, params: &Value) -> BridgeResult<()> {
        let path = Path::new(required_str(params, "path")?);
        if !path.is_absolute() {
            return Err(Pcsx2BridgeError::BadParams(
                "savestate path must be absolute".into(),
            ));
        }
        let raw = path.to_str().ok_or_else(|| {
            Pcsx2BridgeError::BadParams("savestate path must be valid UTF-8".into())
        })?;
        if raw.is_empty() || raw.len() > 4096 {
            return Err(Pcsx2BridgeError::BadParams(
                "savestate path length must be in 1..=4096 bytes".into(),
            ));
        }
        let mut body = Vec::with_capacity(4 + raw.len());
        body.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        body.extend_from_slice(raw.as_bytes());
        self.require_frozen(if opcode == MSG_EMUCAP_SAVE_STATE {
            "save_state"
        } else {
            "load_state"
        })?;
        self.command(opcode, &body)?;
        Ok(())
    }

    fn require_frozen(&mut self, method: &str) -> BridgeResult<()> {
        let state = self.emulator_state()?;
        if state != "frozen" {
            return Err(Pcsx2BridgeError::BadState(format!(
                "{method} requires frozen state, got {state}"
            )));
        }
        Ok(())
    }
}

fn capability_notes() -> Value {
    json!({
        "backend": "pcsx2-pine-fork",
        "rust_bridge": true,
        "implemented_methods": METHODS,
        "planned_methods": UNSUPPORTED_METHODS,
        "cpu": ["ee"],
        "step_units": ["frames"],
        "frame_step": true,
        "state_restore": true,
        "disassemble": true,
        "breakpoints": true,
        "input": true,
        "screenshot": true,
        "call_stack": true,
    })
}

fn pcsx2_input_buttons_json() -> Value {
    json!({
        "system": "ps2",
        "buttons": input::PCSX2_INPUT_BUTTONS,
        "implemented": true,
        "notes": "Controller port 0. set_input replaces the digital-button override and an empty list restores native input. press_buttons applies all requested buttons in one emulator-frame window and releases them before its terminal response.",
    })
}

fn error_kind(error: &Pcsx2BridgeError) -> &'static str {
    match error {
        Pcsx2BridgeError::BadParams(_) => "bad_params",
        Pcsx2BridgeError::BadState(_) => "bad_state",
        Pcsx2BridgeError::UnknownMethod(_) => "unknown_method",
        Pcsx2BridgeError::Unsupported(_) => "unsupported",
        Pcsx2BridgeError::Emulator(_) => "emulator_error",
        Pcsx2BridgeError::Protocol(_) | Pcsx2BridgeError::Io(_) | Pcsx2BridgeError::Json(_) => {
            "bridge_error"
        }
    }
}

fn required_str<'a>(params: &'a Value, name: &str) -> BridgeResult<&'a str> {
    params
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Pcsx2BridgeError::BadParams(format!("{name} must be a string")))
}

fn optional_num(params: &Value, name: &str) -> BridgeResult<Option<u64>> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| {
            Pcsx2BridgeError::BadParams(format!("{name} must be a non-negative integer"))
        }),
        Some(Value::String(raw)) => crate::numparse::parse_num_str(raw)
            .map(Some)
            .map_err(|error| Pcsx2BridgeError::BadParams(format!("{name}: {error}"))),
        Some(_) => Err(Pcsx2BridgeError::BadParams(format!(
            "{name} must be an integer or hexadecimal string"
        ))),
    }
}

fn required_num(params: &Value, name: &str) -> BridgeResult<u64> {
    optional_num(params, name)?
        .ok_or_else(|| Pcsx2BridgeError::BadParams(format!("{name} is required")))
}

fn required_num_alias(params: &Value, names: &[&str]) -> BridgeResult<u64> {
    for name in names {
        if let Some(value) = optional_num(params, name)? {
            return Ok(value);
        }
    }
    Err(Pcsx2BridgeError::BadParams(format!(
        "{} is required",
        names.join(" or ")
    )))
}

fn routed_ee_address(params: &Value, length: u64) -> BridgeResult<u32> {
    match params.get("memory_type").and_then(Value::as_str) {
        Some("ee") => {}
        Some(other) => {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "unsupported memory_type `{other}`; valid: ee"
            )))
        }
        None => {
            return Err(Pcsx2BridgeError::BadParams(
                "memory_type is required".into(),
            ))
        }
    }
    let address = required_num_alias(params, &["address", "start"])?;
    let end = address
        .checked_add(length)
        .ok_or_else(|| Pcsx2BridgeError::BadParams("EE memory range overflow".into()))?;
    if end > PCSX2_EE_RAM_SIZE {
        return Err(Pcsx2BridgeError::BadParams(format!(
            "EE memory range [{address:#x}, {end:#x}) exceeds [0, {PCSX2_EE_RAM_SIZE:#x})"
        )));
    }
    u32::try_from(address).map_err(|_| Pcsx2BridgeError::BadParams("EE address exceeds u32".into()))
}

fn read_u32_exact(payload: &[u8]) -> BridgeResult<u32> {
    if payload.len() != 4 {
        return Err(Pcsx2BridgeError::Protocol(format!(
            "expected a four-byte PINE reply, got {} bytes",
            payload.len()
        )));
    }
    Ok(u32::from_le_bytes(
        payload.try_into().expect("four-byte slice"),
    ))
}

struct SliceCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SliceCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn bytes(&mut self, length: usize) -> BridgeResult<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| Pcsx2BridgeError::Protocol("reply cursor overflow".into()))?;
        let value = self.bytes.get(self.offset..end).ok_or_else(|| {
            Pcsx2BridgeError::Protocol("reply ended before the announced field length".into())
        })?;
        self.offset = end;
        Ok(value)
    }

    fn u32(&mut self) -> BridgeResult<u32> {
        Ok(u32::from_le_bytes(
            self.bytes(4)?.try_into().expect("four bytes"),
        ))
    }

    fn i32(&mut self) -> BridgeResult<i32> {
        Ok(i32::from_le_bytes(
            self.bytes(4)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self) -> BridgeResult<u64> {
        Ok(u64::from_le_bytes(
            self.bytes(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
#[path = "pcsx2_bridge_tests.rs"]
mod tests;
