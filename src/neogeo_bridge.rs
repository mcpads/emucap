//! Neo Geo MVS, AES, and CD bridge for the repository-owned MAME Lua debugger plugin.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Digest as Sha2Digest, Sha256};

use crate::gdb_rsp::{GdbBridgeEnv, GdbError, GdbTransport};
use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

#[path = "neogeo_bridge/breakpoints.rs"]
mod breakpoints;
use breakpoints::{breakpoint_kinds, is_breakpoint_stop};

const MVS_METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "get_state",
    "save_state",
    "load_state",
    "read_memory",
    "write_memory",
    "screenshot",
    "set_input",
    "press_buttons",
    "pause",
    "resume",
    "step",
    "step_instructions",
    "run_frames",
    "reset",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "poll_events",
    "disassemble",
    "call_stack",
];
const CD_METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "get_state",
    "read_memory",
    "write_memory",
    "screenshot",
    "set_input",
    "press_buttons",
    "pause",
    "resume",
    "step",
    "step_instructions",
    "run_frames",
    "reset",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "poll_events",
    "disassemble",
    "call_stack",
];
const MVS_ACTIVE_EXCEPTIONS: &[&str] = &[
    "neogeo.state-read.frozen-only",
    "neogeo.state-save.frozen-only",
    "neogeo.state-load.frozen-only",
    "neogeo.memory-read.frozen-only",
    "neogeo.memory-read.bounded",
    "neogeo.memory-write.frozen-only",
    "neogeo.input-hold.port-zero-only",
    "neogeo.input-pulse.constraints",
    "neogeo.execution-step.main-cpu-only",
    "neogeo.execution-pause.machine-global",
    "neogeo.execution-resume.machine-global",
    "neogeo.breakpoint.pausing-subset",
    "neogeo.call-stack.frozen-best-effort",
];
const CD_ACTIVE_EXCEPTIONS: &[&str] = &[
    "neogeo.state-read.frozen-only",
    "neogeo.memory-read.frozen-only",
    "neogeo.memory-read.bounded",
    "neogeo.memory-write.frozen-only",
    "neogeo.input-hold.port-zero-only",
    "neogeo.input-pulse.constraints",
    "neogeo.execution-step.main-cpu-only",
    "neogeo.execution-pause.machine-global",
    "neogeo.execution-resume.machine-global",
    "neogeo.breakpoint.pausing-subset",
    "neogeo.call-stack.frozen-best-effort",
];
const MVS_RAM_BASE: u64 = 0x10_0000;
const MVS_RAM_SIZE: u64 = 0x1_0000;
const CD_RAM_BASE: u64 = 0;
const CD_RAM_SIZE: u64 = 0x20_0000;
const MAX_READ: u64 = 0x4000;
const MAX_INPUT_FRAMES: u64 = 120;
const FRAME_OPERATION_STARTUP_MS: u64 = 5_000;
const FRAME_OPERATION_BUDGET_MS: u64 = 500;
const STATE_OPERATION_TIMEOUT: Duration = Duration::from_secs(65);
const REG_NAMES: &[&str] = &[
    "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "a0", "a1", "a2", "a3", "a4", "a5", "a6", "sp",
    "sr", "pc",
];
const MVS_INPUT_BUTTONS: &[&str] = &[
    "a", "b", "c", "d", "start", "coin", "service", "up", "down", "left", "right",
];
const CD_INPUT_BUTTONS: &[&str] = &[
    "a", "b", "c", "d", "start", "select", "up", "down", "left", "right",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NeoGeoProfile {
    Mvs,
    Aes,
    Cd,
}

impl NeoGeoProfile {
    fn parse(system: &str) -> BridgeResult<Self> {
        match system {
            "neogeo_mvs" => Ok(Self::Mvs),
            "neogeo_aes" => Ok(Self::Aes),
            "neogeo_cd" => Ok(Self::Cd),
            _ => Err(BridgeError::BadParams(format!(
                "unsupported Neo Geo system: {system}"
            ))),
        }
    }

    fn methods(self) -> &'static [&'static str] {
        match self {
            Self::Mvs | Self::Aes => MVS_METHODS,
            Self::Cd => CD_METHODS,
        }
    }

    fn active_exceptions(self) -> &'static [&'static str] {
        match self {
            Self::Mvs | Self::Aes => MVS_ACTIVE_EXCEPTIONS,
            Self::Cd => CD_ACTIVE_EXCEPTIONS,
        }
    }

    fn ram(self) -> (u64, u64) {
        match self {
            Self::Mvs | Self::Aes => (MVS_RAM_BASE, MVS_RAM_SIZE),
            Self::Cd => (CD_RAM_BASE, CD_RAM_SIZE),
        }
    }

    fn input_buttons(self) -> &'static [&'static str] {
        match self {
            Self::Mvs => MVS_INPUT_BUTTONS,
            Self::Aes | Self::Cd => CD_INPUT_BUTTONS,
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::Mvs => "mvs",
            Self::Aes => "aes",
            Self::Cd => "cdz",
        }
    }

    fn supports_state_files(self) -> bool {
        matches!(self, Self::Mvs | Self::Aes)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("{0}")]
    BadParams(String),
    #[error("{0}")]
    BadState(String),
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("{0}")]
    Emulator(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Gdb(#[from] GdbError),
}

type BridgeResult<T> = Result<T, BridgeError>;
const MAX_DASM_OUTPUT_BYTES: u64 = 0x2_0000;

#[derive(Debug, Clone)]
struct NeoGeoSnapshot {
    memory_type: String,
    address: u64,
    length: u64,
}

#[derive(Debug, Clone)]
enum NeoGeoArmState {
    Armed,
    Failed(String),
}

#[derive(Debug, Clone)]
struct NeoGeoBreakpoint {
    kind: String,
    start: u64,
    end: u64,
    absolute_start: u64,
    backend_kind: String,
    backend_id: Option<u64>,
    snapshots: Vec<NeoGeoSnapshot>,
    arm_state: NeoGeoArmState,
}

pub struct NeoGeoBridge<G> {
    gdb: G,
    env: GdbBridgeEnv,
    system: String,
    profile: NeoGeoProfile,
    frozen: bool,
    breakpoints: BTreeMap<u64, NeoGeoBreakpoint>,
    next_breakpoint_id: u64,
    events: VecDeque<Value>,
    last_hit_seq: u64,
    next_reset_seq: u64,
    adapter_home: PathBuf,
}

impl<G: GdbTransport> NeoGeoBridge<G> {
    pub fn new(gdb: G, env: GdbBridgeEnv, system: &str) -> BridgeResult<Self> {
        let profile = NeoGeoProfile::parse(system)?;
        Ok(Self {
            gdb,
            env,
            system: system.into(),
            profile,
            frozen: false,
            breakpoints: BTreeMap::new(),
            next_breakpoint_id: 1,
            events: VecDeque::new(),
            last_hit_seq: 0,
            next_reset_seq: 1,
            adapter_home: std::env::var_os("EMUCAP_ADAPTER_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::temp_dir().join(format!("emucap-neogeo-{}", std::process::id()))
                }),
        })
    }

    pub fn handle_request(&mut self, req: Request) -> Response {
        let id = req.id;
        let result = match req.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "get_rom_info" => self.get_rom_info(),
            "get_state" => self.get_state(),
            "save_state" if self.profile.supports_state_files() => self.save_state(&req.params),
            "load_state" if self.profile.supports_state_files() => self.load_state(&req.params),
            "read_memory" => self.read_memory(&req.params),
            "write_memory" => self.write_memory(&req.params),
            "screenshot" => self.screenshot(),
            "set_input" => self.set_input(&req.params),
            "press_buttons" => self.press_buttons(&req.params),
            "pause" => self.pause(&req.params),
            "resume" => self.resume(&req.params),
            "step" => self.step(&req.params),
            "step_instructions" => self.step_instructions(&req.params),
            "run_frames" => self.run_frames(&req.params),
            "reset" => self.reset(),
            "set_breakpoint" => self.set_breakpoint(&req.params),
            "clear_breakpoint" => self.clear_breakpoint(&req.params),
            "list_breakpoints" => self.list_breakpoints(),
            "clear_all_breakpoints" => self.clear_all_breakpoints(),
            "poll_events" => self.poll_events(&req.params),
            "disassemble" => self.disassemble(&req.params),
            "call_stack" => self.call_stack(&req.params),
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

    pub fn backend_terminal(&self) -> bool {
        self.gdb.is_terminal()
    }

    fn hello(&self) -> BridgeResult<Value> {
        let methods = self.profile.methods();
        let (_, ram_size) = self.profile.ram();
        let input_buttons = self.profile.input_buttons();
        let state_restore = match self.profile {
            NeoGeoProfile::Mvs | NeoGeoProfile::Aes => json!({
                "supported": true,
                "format": "mame-native",
                "save_completion": "pre-save notifier plus completed non-empty file",
                "load_completion": "post-load notifier",
                "execution_state": "frozen",
                "screenshot_after_load": "step one frozen frame before judging the restored screen",
            }),
            NeoGeoProfile::Cd => json!({
                "supported": false,
                "reason": "MAME 0.288 does not mark the CDZ driver as supporting save states",
            }),
        };
        let mut value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": self.system,
            "adapter": "mame-neogeo-rust-gdb",
            "backend": "lua-gdbstub",
            "debugger": true,
            "methods": methods,
            "memory_types": ["ram"],
            "region_sizes": {"ram": ram_size},
            "breakpoint_kinds": breakpoint_kinds(),
            "contracts": crate::contracts::advertisement_value(self.profile.active_exceptions()),
            "input_buttons": {"system": self.system, "buttons": input_buttons},
            "capability_notes": {
                "implemented_methods": methods,
                "step_units": ["frames", "instructions"],
                "step_cpus": ["m68000"],
                "main_cpu": "m68000",
                "secondary_cpu": "z80",
                "initial_scope": self.profile.scope(),
                "state_restore": state_restore,
            },
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
                "frame": {
                    "max_count": max_sync_frame_count(),
                    "estimated_ms_per_frame": FRAME_OPERATION_BUDGET_MS,
                },
            },
        });
        let obj = value.as_object_mut().expect("hello object");
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
        Ok(value)
    }

    fn status(&mut self) -> BridgeResult<Value> {
        self.drain_breakpoint_packets()?;
        let methods = self.profile.methods();
        let (_, ram_size) = self.profile.ram();
        let input_buttons = self.profile.input_buttons();
        let frame = self.current_frame()?;
        let fields = self.input_fields()?;
        let input_override = self.input_override()?;
        Ok(json!({
            "connected": true,
            "system": self.system,
            "adapter": "mame-neogeo-rust-gdb",
            "backend": "lua-gdbstub",
            "debugger": true,
            "state": if self.frozen { "frozen" } else { "running" },
            "frame": frame,
            "methods": methods,
            "memory_types": ["ram"],
            "region_sizes": {"ram": ram_size},
            "breakpoint_kinds": breakpoint_kinds(),
            "input_buttons": {"system": self.system, "buttons": input_buttons, "available": fields},
            "input_override": input_override,
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
                "frame": {
                    "max_count": max_sync_frame_count(),
                    "estimated_ms_per_frame": FRAME_OPERATION_BUDGET_MS,
                },
            },
        }))
    }

    fn get_rom_info(&self) -> BridgeResult<Value> {
        let content = self.env.content.as_ref().ok_or_else(|| {
            BridgeError::BadParams("EMUCAP_CONTENT is not set for get_rom_info".into())
        })?;
        if !content.is_file() {
            return Err(BridgeError::BadParams(format!(
                "content not found: {}",
                content.display()
            )));
        }
        match self.profile {
            NeoGeoProfile::Mvs | NeoGeoProfile::Aes => {
                let mut file = File::open(content)?;
                let mut hasher = Sha1::new();
                let mut size = 0_u64;
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                    size = size
                        .checked_add(read as u64)
                        .ok_or_else(|| BridgeError::BadParams("content size overflow".into()))?;
                }
                Ok(json!({
                    "system": self.system,
                    "adapter": "mame-neogeo-rust-gdb",
                    "name": content.file_name().and_then(|v| v.to_str()).unwrap_or(""),
                    "path": content.canonicalize()?.display().to_string(),
                    "sha1": hex::encode(hasher.finalize()),
                    "size": size,
                    "media_type": content.extension().and_then(|v| v.to_str()).unwrap_or("").to_ascii_lowercase(),
                }))
            }
            NeoGeoProfile::Cd => {
                let identity = crate::cue::graph_identity(content)?;
                let files = identity
                    .files
                    .iter()
                    .map(|file| {
                        json!({
                            "declared_name": file.declared_name,
                            "path": file.path.display().to_string(),
                            "size": file.size,
                            "sha1": file.sha1,
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({
                    "system": self.system,
                    "adapter": "mame-neogeo-rust-gdb",
                    "name": content.file_name().and_then(|v| v.to_str()).unwrap_or(""),
                    "path": content.canonicalize()?.display().to_string(),
                    "sha1": identity.sha1,
                    "size": identity.size,
                    "media_type": "cue",
                    "identity": {
                        "kind": "cue_graph_v1",
                        "files": files,
                    },
                }))
            }
        }
    }

    fn get_state(&mut self) -> BridgeResult<Value> {
        self.require_frozen("get_state")?;
        let raw = self.gdb.send("g")?;
        let bytes = hex::decode(raw.trim())
            .map_err(|_| BridgeError::Emulator("invalid MAME register packet".into()))?;
        if bytes.len() != REG_NAMES.len() * 4 {
            return Err(BridgeError::Emulator(format!(
                "unexpected M68000 register packet length: {}",
                bytes.len()
            )));
        }
        let mut regs = serde_json::Map::new();
        for (index, name) in REG_NAMES.iter().enumerate() {
            let offset = index * 4;
            let value = u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes"));
            regs.insert((*name).into(), json!(value));
        }
        Ok(json!({"M68K": regs, "frame": self.current_frame()?}))
    }

    fn call_stack(&mut self, params: &Value) -> BridgeResult<Value> {
        require_main_cpu(params)?;
        self.require_frozen("call_stack")?;
        let raw = self.lua_cmd("callstack", None)?;
        let mut fields = raw.split('|');
        if fields.next() != Some("STACK") {
            return Err(BridgeError::Emulator(
                "invalid MAME native call stack response".into(),
            ));
        }
        let parse_hex = |name: &str, value: Option<&str>| -> BridgeResult<u64> {
            u64::from_str_radix(value.unwrap_or(""), 16)
                .map_err(|_| BridgeError::Emulator(format!("invalid MAME call stack {name}")))
        };
        let pc = parse_hex("pc", fields.next())?;
        let sp = parse_hex("sp", fields.next())?;
        let complete = match fields.next() {
            Some("1") => true,
            Some("0") => false,
            _ => {
                return Err(BridgeError::Emulator(
                    "invalid MAME call stack completeness flag".into(),
                ))
            }
        };
        let parse_decimal = |name: &str, value: Option<&str>| -> BridgeResult<u64> {
            value
                .unwrap_or("")
                .parse()
                .map_err(|_| BridgeError::Emulator(format!("invalid MAME call stack {name}")))
        };
        let dropped = parse_decimal("dropped count", fields.next())?;
        let native_depth = parse_decimal("native depth", fields.next())?;
        const MAX_NATIVE_FRAMES: usize = 63;
        let mut native_frames = Vec::new();
        for (index, encoded) in fields.enumerate() {
            if native_frames.len() == MAX_NATIVE_FRAMES {
                return Err(BridgeError::Emulator(format!(
                    "MAME returned more than {MAX_NATIVE_FRAMES} native call frames"
                )));
            }
            let values = encoded.split(',').collect::<Vec<_>>();
            if values.len() != 4 {
                return Err(BridgeError::Emulator(format!(
                    "invalid MAME call stack frame {index}"
                )));
            }
            native_frames.push((
                parse_hex("source", Some(values[0]))?,
                parse_hex("target", Some(values[1]))?,
                parse_hex("return address", Some(values[2]))?,
                parse_hex("return stack pointer", Some(values[3]))?,
            ));
        }
        let expected = usize::try_from(native_depth.min(MAX_NATIVE_FRAMES as u64))
            .expect("bounded native stack depth");
        if native_frames.len() != expected {
            return Err(BridgeError::Emulator(format!(
                "MAME call stack declared {native_depth} native frames but returned {}",
                native_frames.len()
            )));
        }
        let mut frames = vec![json!({
            "pc": pc,
            "sp": sp,
            "kind": "pc",
        })];
        for (source, target, return_address, return_stack_pointer) in
            native_frames.into_iter().rev()
        {
            frames.push(json!({
                "pc": source,
                "kind": "call",
                "target": target,
                "return_address": return_address,
                "return_stack_pointer": return_stack_pointer,
            }));
        }
        let depth = frames.len();
        Ok(json!({
            "frames": frames,
            "depth": depth,
            "cpu": "m68000",
            "order": "innermost_to_outermost",
            "method": "mame-native-control-transfer-history",
            "authority": "best_effort",
            "complete_since_reset": complete && dropped == 0,
            "native_depth": native_depth,
            "dropped": dropped,
            "truncated": native_depth > MAX_NATIVE_FRAMES as u64 || dropped > 0,
            "max_depth": MAX_NATIVE_FRAMES + 1,
        }))
    }

    fn save_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("save_state")?;
        let requested = required_path(params, "path")?;
        let path = absolute_path(&requested)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let partial = state_partial_sibling(&path)?;
        let _ = fs::remove_file(&partial);
        let result = (|| {
            self.state_lua_cmd("savesync", &partial)?;
            let metadata = fs::metadata(&partial)?;
            if metadata.len() == 0 {
                return Err(BridgeError::Emulator(
                    "MAME save completed with an empty state file".into(),
                ));
            }
            // MAME writes into a unique sibling first. Publish that completed file through the
            // crate's rollback-aware replacement path so replacing an existing state also works
            // on Windows without deleting the prior state before the new file is ready.
            crate::launch::copy_file_replace(&partial, &path)?;
            let _ = fs::remove_file(&partial);
            let data = fs::read(&path)?;
            let mut hasher = Sha256::new();
            Sha2Digest::update(&mut hasher, &data);
            self.frozen = true;
            Ok(json!({
                "status": "completed",
                "path": path.display().to_string(),
                "format": "mame-native",
                "bytes": data.len(),
                "sha256": hex::encode(hasher.finalize()),
                "state": "frozen",
                "frame": self.current_frame()?,
            }))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result
    }

    fn load_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("load_state")?;
        let requested = required_path(params, "path")?;
        let path = absolute_path(&requested)?;
        if !path.is_file() || path.metadata()?.len() == 0 {
            return Err(BridgeError::BadParams(format!(
                "Neo Geo save state not found or empty: {}",
                path.display()
            )));
        }
        self.state_lua_cmd("loadsync", &path)?;
        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "path": path.display().to_string(),
            "format": "mame-native",
            "state": "frozen",
            "frame": self.current_frame()?,
        }))
    }

    fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("read_memory")?;
        let length = required_num(params, "length")?;
        if length > MAX_READ {
            return Err(BridgeError::BadParams(format!(
                "read length {length:#x} exceeds {MAX_READ:#x}"
            )));
        }
        let address = region_address(self.profile, params, length)?;
        let raw = self.gdb.send(&format!("m{address:x},{length:x}"))?;
        let data = hex::decode(raw.trim())
            .map_err(|_| BridgeError::Emulator("invalid MAME memory response".into()))?;
        if data.len() != length as usize {
            return Err(BridgeError::Emulator(format!(
                "short MAME memory response: expected {length}, got {}",
                data.len()
            )));
        }
        Ok(
            json!({"address": required_num(params, "address")?, "length": length, "hex": hex::encode(data)}),
        )
    }

    fn write_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("write_memory")?;
        let raw = params
            .get("hex")
            .or_else(|| params.get("data"))
            .and_then(Value::as_str)
            .ok_or_else(|| BridgeError::BadParams("missing required param: hex".into()))?;
        let data = hex::decode(raw)
            .map_err(|_| BridgeError::BadParams("hex must contain complete bytes".into()))?;
        let address = region_address(self.profile, params, data.len() as u64)?;
        let response = self.gdb.send(&format!(
            "M{address:x},{:x}:{}",
            data.len(),
            hex::encode(&data)
        ))?;
        if response != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME memory write failed: {response}"
            )));
        }
        Ok(json!({"written": data.len(), "address": required_num(params, "address")?}))
    }

    fn pause(&mut self, params: &Value) -> BridgeResult<Value> {
        require_main_cpu(params)?;
        self.drain_breakpoint_packets()?;
        if !self.frozen {
            let response = self.gdb.interrupt()?;
            if !is_stop(&response) {
                return Err(BridgeError::Emulator(format!(
                    "MAME pause did not return a stop packet: {response}"
                )));
            }
            self.frozen = true;
        }
        Ok(json!({"state": "frozen", "frame": self.current_frame()?}))
    }

    fn resume(&mut self, params: &Value) -> BridgeResult<Value> {
        require_main_cpu(params)?;
        self.drain_breakpoint_packets()?;
        if self.frozen {
            self.gdb.send_no_reply("c")?;
            self.frozen = false;
        }
        Ok(json!({"state": "running"}))
    }

    fn step(&mut self, params: &Value) -> BridgeResult<Value> {
        require_main_cpu(params)?;
        let count = optional_num(params, "count")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(1)
            .max(1);
        match params
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("frames")
        {
            "frames" => self.frame_step(count, true),
            "instructions" => self.instruction_step(count),
            unit => Err(BridgeError::BadParams(format!(
                "unsupported Neo Geo step unit: {unit}"
            ))),
        }
    }

    fn step_instructions(&mut self, params: &Value) -> BridgeResult<Value> {
        require_main_cpu(params)?;
        let count = optional_num(params, "count")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(1)
            .max(1);
        self.instruction_step(count)
    }

    fn instruction_step(&mut self, count: u64) -> BridgeResult<Value> {
        self.require_frozen("instruction step")?;
        for _ in 0..count {
            let response = self.gdb.send("s")?;
            if is_breakpoint_stop(&response) {
                let event = self.record_breakpoint_hit(response)?;
                return Ok(json!({
                    "status":"interrupted",
                    "reason":"breakpoint",
                    "breakpoint_id":event["id"],
                    "event":event,
                    "state":"frozen",
                }));
            }
            if !is_stop(&response) {
                return Err(BridgeError::Emulator(format!(
                    "MAME instruction step did not stop: {response}"
                )));
            }
        }
        Ok(
            json!({"status": "completed", "unit": "instructions", "count": count, "state": "frozen"}),
        )
    }

    fn run_frames(&mut self, params: &Value) -> BridgeResult<Value> {
        let count = optional_num(params, "n")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(1)
            .max(1);
        self.frame_step(count, false)
    }

    fn frame_step(&mut self, count: u64, stop_on_done: bool) -> BridgeResult<Value> {
        self.drain_breakpoint_packets()?;
        if stop_on_done {
            self.require_frozen("frame step")?;
        }
        let max_count = max_sync_frame_count();
        if count > max_count {
            return Err(BridgeError::BadParams(format!(
                "Neo Geo frame count {count} exceeds the synchronous cap {max_count}; split the advance and verify each terminal response"
            )));
        }
        let before = self.current_frame()?;
        let command = if stop_on_done {
            "framestep"
        } else {
            "runframes"
        };
        let previous_timeout = self.gdb.get_timeout()?;
        let estimated_ms = FRAME_OPERATION_STARTUP_MS
            .saturating_add(count.saturating_mul(FRAME_OPERATION_BUDGET_MS));
        let timeout = Duration::from_millis(estimated_ms);
        self.gdb.set_timeout(timeout)?;
        let outcome = self.lua_cmd(command, Some(&count.to_string()));
        let restore = self.gdb.set_timeout(previous_timeout);
        let response = match (outcome, restore) {
            (Ok(value), Ok(())) => value,
            (Err(primary), Ok(())) => return Err(primary),
            (Ok(_), Err(cleanup)) => {
                return Err(BridgeError::Emulator(format!(
                    "MAME {command} completed but failed to restore the GDB timeout: {cleanup}"
                )))
            }
            (Err(primary), Err(cleanup)) => {
                return Err(BridgeError::Emulator(format!(
                    "{primary}; additionally failed to restore the GDB timeout: {cleanup}"
                )))
            }
        };
        if is_breakpoint_stop(&response) {
            let event = self.record_breakpoint_hit(response)?;
            return Ok(json!({
                "status":"interrupted",
                "reason":"breakpoint",
                "breakpoint_id":event["id"],
                "event":event,
                "frame_before":before,
                "state":"frozen",
            }));
        }
        if response != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME {command} failed: {response}"
            )));
        }
        self.frozen = stop_on_done;
        let after = self.current_frame()?;
        let frame_counter_delta = after.checked_sub(before);
        if stop_on_done && frame_counter_delta != Some(count) {
            self.frozen = true;
            return Err(BridgeError::Emulator(format!(
                "Neo Geo exact frame step completed with screen-frame delta {frame_counter_delta:?}, expected {count}"
            )));
        }
        Ok(json!({
            "status": "completed",
            "unit": "frames",
            "count": count,
            "frames_observed_min": count,
            "frame_before": before,
            "frame": after,
            "frame_counter_delta": frame_counter_delta,
            "frame_counter_continuous": frame_counter_delta.is_some(),
            "state": if stop_on_done { "frozen" } else { "running" },
        }))
    }

    fn screenshot(&mut self) -> BridgeResult<Value> {
        let frame_before = self.current_frame()?;
        let path = std::env::temp_dir().join(format!(
            "emucap_neogeo_{}_{}.png",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|v| v.as_nanos())
                .unwrap_or_default()
        ));
        let result = (|| {
            self.lua_cmd("snapshot", Some(path.to_string_lossy().as_ref()))?;
            let data = fs::read(&path)?;
            if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(BridgeError::Emulator("MAME snapshot is not PNG".into()));
            }
            let frame_after = self.current_frame()?;
            let mut hasher = Sha256::new();
            Sha2Digest::update(&mut hasher, &data);
            Ok(json!({
                "png_base64": base64::engine::general_purpose::STANDARD.encode(&data),
                "sha256": hex::encode(hasher.finalize()),
                "byte_len": data.len(),
                "frame_before": frame_before,
                "frame_after": frame_after,
                "frame_stable": frame_before == frame_after,
                "state": if self.frozen { "frozen" } else { "running" },
                "freshness": "current_screen",
            }))
        })();
        let _ = fs::remove_file(path);
        result
    }

    fn set_input(&mut self, params: &Value) -> BridgeResult<Value> {
        require_port_zero(params)?;
        let buttons = normalize_buttons(self.profile, params.get("buttons"))?;
        self.lua_cmd("setinput", Some(&buttons.join(",")))?;
        Ok(
            json!({"buttons": buttons, "mode": if buttons.is_empty() { "native" } else { "persistent" }}),
        )
    }

    fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        require_port_zero(params)?;
        self.drain_breakpoint_packets()?;
        let buttons = normalize_buttons(self.profile, params.get("buttons"))?;
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        if frames > MAX_INPUT_FRAMES {
            return Err(BridgeError::BadParams(format!(
                "Neo Geo press_buttons supports at most {MAX_INPUT_FRAMES} frames"
            )));
        }
        let response = self.lua_cmd("press", Some(&format!("{frames}:{}", buttons.join(","))))?;
        if is_breakpoint_stop(&response) {
            let event = self.record_breakpoint_hit(response)?;
            return Ok(json!({
                "status":"interrupted",
                "reason":"breakpoint",
                "breakpoint_id":event["id"],
                "event":event,
                "buttons":buttons,
                "frames":frames,
                "state":"frozen",
            }));
        }
        if response != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME input pulse failed: {response}"
            )));
        }
        self.frozen = false;
        Ok(json!({"status": "completed", "buttons": buttons, "frames": frames, "state": "running"}))
    }

    fn reset(&mut self) -> BridgeResult<Value> {
        let reset_seq = self.next_reset_seq;
        self.next_reset_seq = self
            .next_reset_seq
            .checked_add(1)
            .ok_or_else(|| BridgeError::BadState("Neo Geo reset sequence is exhausted".into()))?;
        let response = match self.lua_cmd("resetsync", Some(&reset_seq.to_string())) {
            Ok(response) => response,
            Err(error) => {
                self.fail_all_breakpoints_after_reset(&error.to_string());
                return Err(error);
            }
        };
        let expected = format!("OK:{reset_seq}");
        if response != expected {
            let message = format!(
                "MAME synchronous reset expected sequence {reset_seq}, received {response}"
            );
            self.fail_all_breakpoints_after_reset(&message);
            return Err(BridgeError::Emulator(message));
        }
        self.frozen = false;
        Ok(json!({
            "status": "completed",
            "reset": "completed",
            "state": "running",
            "reset_seq": reset_seq,
        }))
    }

    fn fail_all_breakpoints_after_reset(&mut self, reason: &str) {
        for breakpoint in self.breakpoints.values_mut() {
            breakpoint.backend_id = None;
            breakpoint.arm_state =
                NeoGeoArmState::Failed(format!("reset could not verify native point: {reason}"));
        }
    }

    fn current_frame(&mut self) -> BridgeResult<u64> {
        self.lua_cmd("frame", None)?
            .parse()
            .map_err(|_| BridgeError::Emulator("invalid MAME frame counter".into()))
    }

    fn input_fields(&mut self) -> BridgeResult<Vec<String>> {
        let raw = self.lua_cmd("inputfields", None)?;
        Ok(raw
            .split(',')
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn input_override(&mut self) -> BridgeResult<Value> {
        let remaining: i64 = self
            .lua_cmd("inputstatus", None)?
            .parse()
            .map_err(|_| BridgeError::Emulator("invalid MAME input status".into()))?;
        Ok(match remaining {
            0 => json!({"observable": true, "engaged": false, "mode": "native"}),
            n if n < 0 => json!({"observable": true, "engaged": true, "mode": "persistent"}),
            n => {
                json!({"observable": true, "engaged": true, "mode": "timed", "remaining_frames": n})
            }
        })
    }

    fn lua_cmd(&mut self, name: &str, value: Option<&str>) -> BridgeResult<String> {
        let payload = match value {
            Some(value) => format!("qEmucap,{name},{}", hex::encode(value.as_bytes())),
            None => format!("qEmucap,{name}"),
        };
        let response = self.gdb.send(&payload)?;
        if response.starts_with('E') {
            Err(BridgeError::Emulator(format!(
                "MAME {name} failed: {response}"
            )))
        } else {
            Ok(response)
        }
    }

    fn state_lua_cmd(&mut self, name: &str, path: &Path) -> BridgeResult<()> {
        let path = path.to_str().ok_or_else(|| {
            BridgeError::BadParams(format!(
                "MAME state paths must be valid UTF-8: {}",
                path.display()
            ))
        })?;
        let previous_timeout = self.gdb.get_timeout()?;
        self.gdb.set_timeout(STATE_OPERATION_TIMEOUT)?;
        let outcome = self.lua_cmd(name, Some(path));
        let restore = self.gdb.set_timeout(previous_timeout);
        let response = match (outcome, restore) {
            (Ok(value), Ok(())) => value,
            (Err(primary), Ok(())) => return Err(primary),
            (Ok(_), Err(cleanup)) => {
                return Err(BridgeError::Emulator(format!(
                    "MAME {name} completed but failed to restore the GDB timeout: {cleanup}"
                )))
            }
            (Err(primary), Err(cleanup)) => {
                return Err(BridgeError::Emulator(format!(
                    "{primary}; additionally failed to restore the GDB timeout: {cleanup}"
                )))
            }
        };
        if response != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME {name} returned an unexpected response: {response}"
            )));
        }
        Ok(())
    }

    fn require_frozen(&self, operation: &str) -> BridgeResult<()> {
        if self.frozen {
            Ok(())
        } else {
            Err(BridgeError::BadState(format!(
                "{operation} requires a frozen Neo Geo machine; call pause first"
            )))
        }
    }
}

fn max_sync_frame_count() -> u64 {
    let deadline_ms = crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64;
    deadline_ms
        .saturating_sub(FRAME_OPERATION_STARTUP_MS)
        .checked_div(FRAME_OPERATION_BUDGET_MS)
        .unwrap_or(0)
        .min(crate::live::temporal::MAX_SYNC_ADVANCE_COUNT)
}

fn required_path(params: &Value, key: &str) -> BridgeResult<PathBuf> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| BridgeError::BadParams(format!("missing or invalid param: {key}")))
}

fn absolute_path(path: &Path) -> BridgeResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn state_partial_sibling(path: &Path) -> BridgeResult<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        BridgeError::BadParams(format!(
            "save path has no parent directory: {}",
            path.display()
        ))
    })?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    Ok(parent.join(format!(".{name}.partial.{}.{nanos}", std::process::id())))
}

fn error_kind(error: &BridgeError) -> &'static str {
    match error {
        BridgeError::BadParams(_) => "bad_params",
        BridgeError::BadState(_) => "bad_state",
        BridgeError::UnknownMethod(_) => "unknown_method",
        BridgeError::Emulator(_) | BridgeError::Gdb(GdbError::Emulator(_)) => "emulator_error",
        BridgeError::Io(_) | BridgeError::Gdb(_) => "bridge_error",
    }
}

fn is_stop(value: &str) -> bool {
    value.starts_with('S') || value.starts_with('T')
}

fn parse_num(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => {
            let value = value.trim();
            if let Some(value) = value.strip_prefix("0x").or_else(|| value.strip_prefix('$')) {
                u64::from_str_radix(value, 16).ok()
            } else {
                value.parse().ok()
            }
        }
        _ => None,
    }
}

fn required_num(params: &Value, key: &str) -> BridgeResult<u64> {
    params
        .get(key)
        .and_then(parse_num)
        .ok_or_else(|| BridgeError::BadParams(format!("missing or invalid param: {key}")))
}

fn optional_num(params: &Value, key: &str) -> BridgeResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| BridgeError::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

fn region_address(profile: NeoGeoProfile, params: &Value, length: u64) -> BridgeResult<u64> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("ram");
    if memory_type != "ram" {
        return Err(BridgeError::BadParams(format!(
            "unsupported Neo Geo memory_type: {memory_type}"
        )));
    }
    let offset = required_num(params, "address")?;
    let (ram_base, ram_size) = profile.ram();
    if !matches!(offset.checked_add(length), Some(end) if end <= ram_size) {
        return Err(BridgeError::BadParams(format!(
            "ram access out of range: offset {offset:#x}+{length:#x} exceeds {ram_size:#x}"
        )));
    }
    ram_base
        .checked_add(offset)
        .ok_or_else(|| BridgeError::BadParams("ram address overflow".into()))
}

fn require_port_zero(params: &Value) -> BridgeResult<()> {
    if optional_num(params, "port")?.unwrap_or(0) == 0 {
        Ok(())
    } else {
        Err(BridgeError::BadParams(
            "Neo Geo input currently supports port 0 only".into(),
        ))
    }
}

fn require_main_cpu(params: &Value) -> BridgeResult<()> {
    match params.get("cpu").and_then(Value::as_str) {
        None | Some("maincpu" | "m68000" | "68k") => Ok(()),
        Some(cpu) => Err(BridgeError::BadParams(format!(
            "Neo Geo execution control currently supports the m68000 main CPU only, got {cpu}"
        ))),
    }
}

fn normalize_buttons(profile: NeoGeoProfile, value: Option<&Value>) -> BridgeResult<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| BridgeError::BadParams("buttons must be a list".into()))?;
    values
        .iter()
        .map(|value| {
            let key = value
                .as_str()
                .ok_or_else(|| BridgeError::BadParams("button names must be strings".into()))?
                .trim()
                .to_ascii_lowercase();
            if profile.input_buttons().contains(&key.as_str()) {
                Ok(key)
            } else {
                Err(BridgeError::BadParams(format!(
                    "unsupported Neo Geo button: {key}"
                )))
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "neogeo_bridge_tests.rs"]
mod tests;
