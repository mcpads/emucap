//! Neo Geo MVS/AES bridge for the repository-owned MAME Lua debugger plugin.

use std::fs::{self, File};
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Digest as Sha2Digest, Sha256};

use crate::gdb_rsp::{GdbBridgeEnv, GdbError, GdbTransport};
use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

const METHODS: &[&str] = &[
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
];
const ACTIVE_EXCEPTIONS: &[&str] = &[
    "neogeo.state-read.frozen-only",
    "neogeo.memory-read.frozen-only",
    "neogeo.memory-read.bounded",
    "neogeo.memory-write.frozen-only",
    "neogeo.input-hold.port-zero-only",
    "neogeo.input-pulse.constraints",
    "neogeo.execution-step.main-cpu-only",
    "neogeo.execution-pause.machine-global",
    "neogeo.execution-resume.machine-global",
];
const RAM_BASE: u64 = 0x10_0000;
const RAM_SIZE: u64 = 0x1_0000;
const MAX_READ: u64 = 0x4000;
const MAX_INPUT_FRAMES: u64 = 120;
const REG_NAMES: &[&str] = &[
    "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "a0", "a1", "a2", "a3", "a4", "a5", "a6", "sp",
    "sr", "pc",
];
const INPUT_BUTTONS: &[&str] = &[
    "a", "b", "c", "d", "start", "coin", "service", "up", "down", "left", "right",
];

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

pub struct NeoGeoBridge<G> {
    gdb: G,
    env: GdbBridgeEnv,
    system: String,
    frozen: bool,
}

impl<G: GdbTransport> NeoGeoBridge<G> {
    pub fn new(gdb: G, env: GdbBridgeEnv, system: &str) -> BridgeResult<Self> {
        if system != "neogeo_mvs" {
            return Err(BridgeError::BadParams(format!(
                "unsupported Neo Geo system: {system}"
            )));
        }
        Ok(Self {
            gdb,
            env,
            system: system.into(),
            frozen: false,
        })
    }

    pub fn handle_request(&mut self, req: Request) -> Response {
        let id = req.id;
        let result = match req.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "get_rom_info" => self.get_rom_info(),
            "get_state" => self.get_state(),
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
        let mut value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": self.system,
            "adapter": "mame-neogeo-rust-gdb",
            "backend": "lua-gdbstub",
            "debugger": true,
            "methods": METHODS,
            "memory_types": ["ram"],
            "region_sizes": {"ram": RAM_SIZE},
            "breakpoint_kinds": [],
            "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS),
            "input_buttons": {"system": self.system, "buttons": INPUT_BUTTONS},
            "capability_notes": {
                "implemented_methods": METHODS,
                "step_units": ["frames", "instructions"],
                "step_cpus": ["m68000"],
                "main_cpu": "m68000",
                "secondary_cpu": "z80",
                "initial_scope": "mvs",
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
            "methods": METHODS,
            "memory_types": ["ram"],
            "region_sizes": {"ram": RAM_SIZE},
            "breakpoint_kinds": [],
            "input_buttons": {"system": self.system, "buttons": INPUT_BUTTONS, "available": fields},
            "input_override": input_override,
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
            "sha1": format!("{:x}", hasher.finalize()),
            "size": size,
            "media_type": content.extension().and_then(|v| v.to_str()).unwrap_or("").to_ascii_lowercase(),
        }))
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

    fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("read_memory")?;
        let length = required_num(params, "length")?;
        if length > MAX_READ {
            return Err(BridgeError::BadParams(format!(
                "read length {length:#x} exceeds {MAX_READ:#x}"
            )));
        }
        let address = region_address(params, length)?;
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
        let address = region_address(params, data.len() as u64)?;
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
        if stop_on_done {
            self.require_frozen("frame step")?;
        }
        let before = self.current_frame()?;
        let command = if stop_on_done {
            "framestep"
        } else {
            "runframes"
        };
        let response = self.lua_cmd(command, Some(&count.to_string()))?;
        if response != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME {command} failed: {response}"
            )));
        }
        self.frozen = stop_on_done;
        let after = self.current_frame()?;
        if after.saturating_sub(before) != count {
            return Err(BridgeError::Emulator(format!(
                "MAME frame step mismatch: requested {count}, observed {}",
                after.saturating_sub(before)
            )));
        }
        Ok(json!({
            "status": "completed",
            "unit": "frames",
            "count": count,
            "frame_before": before,
            "frame": after,
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
                "sha256": format!("{:x}", hasher.finalize()),
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
        let buttons = normalize_buttons(params.get("buttons"))?;
        self.lua_cmd("setinput", Some(&buttons.join(",")))?;
        Ok(
            json!({"buttons": buttons, "mode": if buttons.is_empty() { "native" } else { "persistent" }}),
        )
    }

    fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        require_port_zero(params)?;
        let buttons = normalize_buttons(params.get("buttons"))?;
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        if frames > MAX_INPUT_FRAMES {
            return Err(BridgeError::BadParams(format!(
                "Neo Geo press_buttons supports at most {MAX_INPUT_FRAMES} frames"
            )));
        }
        let response = self.lua_cmd("press", Some(&format!("{frames}:{}", buttons.join(","))))?;
        if response != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME input pulse failed: {response}"
            )));
        }
        self.frozen = false;
        Ok(json!({"status": "completed", "buttons": buttons, "frames": frames, "state": "running"}))
    }

    fn reset(&mut self) -> BridgeResult<Value> {
        self.lua_cmd("reset", None)?;
        self.frozen = false;
        Ok(json!({"reset": "scheduled", "state": "running"}))
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

fn region_address(params: &Value, length: u64) -> BridgeResult<u64> {
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
    if !matches!(offset.checked_add(length), Some(end) if end <= RAM_SIZE) {
        return Err(BridgeError::BadParams(format!(
            "ram access out of range: offset {offset:#x}+{length:#x} exceeds {RAM_SIZE:#x}"
        )));
    }
    RAM_BASE
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

fn normalize_buttons(value: Option<&Value>) -> BridgeResult<Vec<String>> {
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
            if INPUT_BUTTONS.contains(&key.as_str()) {
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
