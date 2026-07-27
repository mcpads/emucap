//! Stock openMSX XML-control bridge.
//!
//! openMSX already exposes the execution, debugger, input, screenshot, and
//! savestate primitives needed by the first MSX cartridge profile.  This
//! module keeps that emulator unmodified and translates its XML stdio control
//! channel into emucap's NDJSON adapter protocol.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

#[path = "openmsx_bridge/input.rs"]
mod input;
#[path = "openmsx_bridge/xml.rs"]
mod xml;

#[cfg(test)]
use input::button_position;
use input::{normalize_buttons, require_port_zero, row_masks, INPUT_BUTTONS};
pub use xml::XmlControl;

const MAX_MEMORY_TRANSFER: u64 = 16 * 1024;
const MAX_INPUT_FRAMES: u64 = 120;
const BASE_METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "get_state",
    "read_memory",
    "write_memory",
    "set_input",
    "press_buttons",
    "pause",
    "resume",
    "step",
    "step_instructions",
    "save_state",
    "load_state",
    "reset",
];
const BASE_EXCEPTIONS: &[&str] = &[
    "openmsx.state-read.frozen-only",
    "openmsx.memory-read.frozen-only",
    "openmsx.memory-read.bounded",
    "openmsx.memory-write.frozen-only",
    "openmsx.input-hold.port-zero-only",
    "openmsx.input-pulse.constraints",
    "openmsx.execution-step.z80-only",
];

#[derive(Debug, thiserror::Error)]
pub enum OpenMsxBridgeError {
    #[error("{0}")]
    BadParams(String),
    #[error("{0}")]
    BadState(String),
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Emulator(String),
    #[error("{0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type BridgeResult<T> = Result<T, OpenMsxBridgeError>;

pub trait OpenMsxControl {
    fn command(&mut self, command: &str) -> BridgeResult<String>;
    fn advance_frames(&mut self, count: u64) -> BridgeResult<()>;
    fn is_terminal(&self) -> bool;
    fn child_pid(&self) -> u32;
}

pub struct OpenMsxBridge<C> {
    control: C,
    content: PathBuf,
    content_sha1: String,
    content_size: u64,
    runtime_home: PathBuf,
    display: bool,
    frozen: bool,
    region_sizes: BTreeMap<&'static str, u64>,
    held_buttons: BTreeSet<String>,
    screenshot_sequence: u64,
    name: Option<String>,
    session_token: Option<String>,
    launch_id: Option<String>,
}

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub fn new(
        mut control: C,
        content: &Path,
        runtime_home: &Path,
        display: bool,
    ) -> BridgeResult<Self> {
        if control.command("openmsx_info version")? != "openMSX 21.0" {
            return Err(OpenMsxBridgeError::Unsupported(
                "the MSX adapter requires openMSX 21.0".into(),
            ));
        }
        if control.command("machine_info config_name")? != "C-BIOS_MSX2+"
            || control.command("machine_info type")? != "MSX2+"
        {
            return Err(OpenMsxBridgeError::Unsupported(
                "the first MSX profile requires the C-BIOS_MSX2+ machine".into(),
            ));
        }
        control.command("openmsx_update enable setting")?;
        control.command("set throttle off")?;
        control.command("set power on")?;
        control.command(if display {
            "set renderer SDLGL-PP"
        } else {
            "set renderer none"
        })?;
        control.command("set mute on")?;
        control.command("set pause on")?;

        let mut region_sizes = BTreeMap::new();
        region_sizes.insert(
            "memory",
            parse_decimal(&control.command("debug size memory")?, "memory size")?,
        );
        region_sizes.insert(
            "vram",
            parse_decimal(&control.command("debug size VRAM")?, "VRAM size")?,
        );
        region_sizes.insert(
            "ram",
            parse_decimal(&control.command("debug size {Main RAM}")?, "Main RAM size")?,
        );
        if region_sizes["memory"] != 65_536
            || region_sizes["vram"] != 131_072
            || region_sizes["ram"] != 524_288
        {
            return Err(OpenMsxBridgeError::Unsupported(format!(
                "C-BIOS_MSX2+ memory layout changed: {region_sizes:?}"
            )));
        }

        let bytes = fs::read(content)?;
        let content_sha1 = format!("{:x}", Sha1::digest(&bytes));
        Ok(Self {
            control,
            content: content.to_path_buf(),
            content_sha1,
            content_size: bytes.len() as u64,
            runtime_home: runtime_home.to_path_buf(),
            display,
            frozen: true,
            region_sizes,
            held_buttons: BTreeSet::new(),
            screenshot_sequence: 0,
            name: std::env::var("EMUCAP_NAME").ok(),
            session_token: std::env::var("EMUCAP_SESSION_TOKEN").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
        })
    }

    pub fn child_pid(&self) -> u32 {
        self.control.child_pid()
    }

    pub fn backend_terminal(&self) -> bool {
        self.control.is_terminal()
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id;
        let result = match request.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "get_rom_info" => self.get_rom_info(),
            "get_state" => self.get_state(&request.params),
            "read_memory" => self.read_memory(&request.params),
            "write_memory" => self.write_memory(&request.params),
            "screenshot" if self.display => self.screenshot(),
            "screenshot" => Err(OpenMsxBridgeError::Unsupported(
                "headless openMSX does not expose screenshots".into(),
            )),
            "set_input" => self.set_input(&request.params),
            "press_buttons" => self.press_buttons(&request.params),
            "pause" => self.pause(&request.params),
            "resume" => self.resume(&request.params),
            "step" => self.step(&request.params),
            "step_instructions" => self.step_instructions(&request.params),
            "save_state" => self.save_state(&request.params),
            "load_state" => self.load_state(&request.params),
            "reset" => self.reset(),
            other => Err(OpenMsxBridgeError::UnknownMethod(other.into())),
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

    fn methods(&self) -> Vec<&'static str> {
        let mut methods = BASE_METHODS.to_vec();
        if self.display {
            methods.push("screenshot");
        }
        methods
    }

    fn active_exceptions(&self) -> Vec<&'static str> {
        let mut exceptions = BASE_EXCEPTIONS.to_vec();
        if self.display {
            exceptions.push("openmsx.screenshot.frozen-only");
        }
        exceptions
    }

    fn hello(&self) -> BridgeResult<Value> {
        let mut value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "msx",
            "adapter": "openmsx-rust-xml",
            "backend": "openMSX 21.0",
            "debugger": true,
            "methods": self.methods(),
            "memory_types": ["memory", "ram", "vram"],
            "region_sizes": self.region_sizes,
            "breakpoint_kinds": [],
            "contracts": crate::contracts::advertisement_value(&self.active_exceptions()),
            "capability_notes": {
                "machine": "C-BIOS_MSX2+",
                "media": "cartridge",
                "cpu": ["z80"],
                "step_units": ["frames", "instructions"],
                "display": self.display,
                "headless_screenshot": false,
            },
            "content": self.content.display().to_string(),
            "content_sha1": self.content_sha1,
            "build": std::env::var("EMUCAP_BUILD_HASH").unwrap_or_else(|_| "openmsx-21.0".into()),
        });
        let object = value.as_object_mut().expect("openMSX hello is an object");
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
        self.refresh_execution_state()?;
        let input_matrix = self.input_matrix()?;
        Ok(json!({
            "connected": !self.control.is_terminal(),
            "system": "msx",
            "adapter": "openmsx-rust-xml",
            "backend": "openMSX 21.0",
            "state": if self.frozen { "frozen" } else { "running" },
            "frame": self.current_frame()?,
            "methods": self.methods(),
            "memory_types": ["memory", "ram", "vram"],
            "region_sizes": self.region_sizes,
            "breakpoint_kinds": [],
            "input_override": !self.held_buttons.is_empty(),
            "input_matrix": input_matrix,
            "input_buttons": {
                "system": "msx",
                "buttons": INPUT_BUTTONS,
                "aliases": {
                    "start": "enter",
                    "return": "enter",
                    "fire1": "space",
                    "fire2": "ctrl"
                },
                "notes": "The first MSX profile injects the standard MSX keyboard matrix. A/B are keyboard letters, not joystick buttons."
            },
            "backend_pid": self.control.child_pid(),
            "launch_id": self.launch_id,
        }))
    }

    fn get_rom_info(&self) -> BridgeResult<Value> {
        Ok(json!({
            "system": "msx",
            "machine": "C-BIOS_MSX2+",
            "media": "cartridge",
            "path": self.content.display().to_string(),
            "sha1": self.content_sha1,
            "size": self.content_size,
        }))
    }

    fn get_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("get_state")?;
        validate_groups(params, &["cpu"])?;
        let mut state = serde_json::Map::new();
        for register in [
            "AF", "BC", "DE", "HL", "AF2", "BC2", "DE2", "HL2", "IX", "IY", "PC", "SP", "I", "R",
            "IM", "IFF",
        ] {
            let value =
                parse_decimal(&self.control.command(&format!("reg {register}"))?, register)?;
            state.insert(register.into(), json!(value));
        }
        Ok(json!({
            "cpu": "z80",
            "state": state,
            "frame": self.current_frame()?,
        }))
    }

    fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("read_memory")?;
        let memory_type = memory_type(params)?;
        let address = required_num(params, "address")?;
        let length = required_num(params, "length")?;
        if length > MAX_MEMORY_TRANSFER {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "read_memory length {length} exceeds {MAX_MEMORY_TRANSFER}"
            )));
        }
        self.validate_range(memory_type, address, length)?;
        let debuggable = debuggable_name(memory_type);
        let command =
            format!("binary encode hex [debug read_block {debuggable} {address} {length}]");
        let encoded = self.control.command(&command)?;
        let bytes = hex::decode(encoded.trim()).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!("openMSX returned invalid memory hex: {error}"))
        })?;
        if bytes.len() as u64 != length {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "openMSX returned {} bytes for a {length}-byte read",
                bytes.len()
            )));
        }
        Ok(json!({
            "memory_type": memory_type,
            "address": address,
            "length": length,
            "hex": hex::encode(bytes),
        }))
    }

    fn write_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("write_memory")?;
        let memory_type = memory_type(params)?;
        let address = required_num(params, "address")?;
        let encoded = params
            .get("hex")
            .and_then(Value::as_str)
            .ok_or_else(|| OpenMsxBridgeError::BadParams("hex must be a string".into()))?;
        let bytes = hex::decode(encoded)
            .map_err(|error| OpenMsxBridgeError::BadParams(format!("invalid hex: {error}")))?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_MEMORY_TRANSFER {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "write_memory hex must contain 1..={MAX_MEMORY_TRANSFER} bytes"
            )));
        }
        self.validate_range(memory_type, address, bytes.len() as u64)?;
        let debuggable = debuggable_name(memory_type);
        self.control.command(&format!(
            "debug write_block {debuggable} {address} [binary decode hex {}]",
            hex::encode(&bytes)
        ))?;
        Ok(json!({
            "memory_type": memory_type,
            "address": address,
            "length": bytes.len(),
            "written": bytes.len(),
        }))
    }

    fn pause(&mut self, params: &Value) -> BridgeResult<Value> {
        require_z80(params)?;
        self.control.command("set pause on")?;
        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "state": "frozen",
            "frame": self.current_frame()?,
        }))
    }

    fn resume(&mut self, params: &Value) -> BridgeResult<Value> {
        require_z80(params)?;
        self.release_debug_break()?;
        self.control.command("set pause off")?;
        self.frozen = false;
        Ok(json!({"status": "completed", "state": "running"}))
    }

    fn step(&mut self, params: &Value) -> BridgeResult<Value> {
        require_z80(params)?;
        let count = optional_num(params, "count")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(1);
        validate_step_count(count)?;
        match params
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("frames")
        {
            "frames" => self.frame_step(count),
            "instructions" => self.instruction_step(count),
            unit => Err(OpenMsxBridgeError::BadParams(format!(
                "unsupported MSX step unit: {unit}"
            ))),
        }
    }

    fn step_instructions(&mut self, params: &Value) -> BridgeResult<Value> {
        require_z80(params)?;
        let count = optional_num(params, "count")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(1);
        validate_step_count(count)?;
        self.instruction_step(count)
    }

    fn frame_step(&mut self, count: u64) -> BridgeResult<Value> {
        self.require_frozen("frame step")?;
        self.release_debug_break()?;
        let before = self.current_frame()?;
        self.control.advance_frames(count)?;
        self.frozen = true;
        let after = self.current_frame()?;
        if after != before + count {
            return Err(OpenMsxBridgeError::Emulator(format!(
                "openMSX frame step mismatch: expected {}, observed {after}",
                before + count
            )));
        }
        Ok(json!({
            "status": "completed",
            "unit": "frames",
            "count": count,
            "frame_before": before,
            "frame": after,
            "state": "frozen",
        }))
    }

    fn instruction_step(&mut self, count: u64) -> BridgeResult<Value> {
        self.require_frozen("instruction step")?;
        self.control.command("debug break")?;
        if self.control.command("debug breaked")? != "1" {
            return Err(OpenMsxBridgeError::Emulator(
                "openMSX did not enter CPU debug break".into(),
            ));
        }
        let pc_before = parse_decimal(&self.control.command("reg PC")?, "PC")?;
        for _ in 0..count {
            self.control.command("debug step")?;
        }
        let pc = parse_decimal(&self.control.command("reg PC")?, "PC")?;
        // `debug break` owns the CPU stop, but it may release the global pause
        // setting. Reassert the public frozen invariant before replying.
        self.control.command("set pause on")?;
        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "cpu": "z80",
            "count": count,
            "pc_before": pc_before,
            "pc": pc,
            "frame": self.current_frame()?,
            "state": "frozen",
        }))
    }

    fn set_input(&mut self, params: &Value) -> BridgeResult<Value> {
        require_port_zero(params)?;
        let buttons = normalize_buttons(params.get("buttons"))?;
        self.apply_buttons(&buttons)?;
        Ok(json!({
            "buttons": buttons,
            "mode": if self.held_buttons.is_empty() { "native" } else { "persistent" },
            "input_override": !self.held_buttons.is_empty(),
        }))
    }

    fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        require_port_zero(params)?;
        let buttons = normalize_buttons(params.get("buttons"))?;
        if buttons.is_empty() {
            return Err(OpenMsxBridgeError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1);
        if !(1..=MAX_INPUT_FRAMES).contains(&frames) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "press_buttons frames must be in 1..={MAX_INPUT_FRAMES}"
            )));
        }
        self.refresh_execution_state()?;
        let was_running = !self.frozen;
        if was_running {
            self.control.command("set pause on")?;
            self.frozen = true;
        }
        let persistent = self.held_buttons.clone();
        let mut combined = persistent.clone();
        combined.extend(buttons.iter().cloned());
        if let Err(primary) = self.apply_buttons(&combined) {
            if was_running {
                return match self.control.command("set pause off") {
                    Ok(_) => {
                        self.frozen = false;
                        Err(primary)
                    }
                    Err(restore) => Err(OpenMsxBridgeError::Emulator(format!(
                        "{primary}; execution-state restore also failed: {restore}"
                    ))),
                };
            }
            return Err(primary);
        }
        let pulse = self.frame_step(frames);
        let release = self.apply_buttons(&persistent);
        let resume = if was_running {
            self.control.command("set pause off").map(|_| {
                self.frozen = false;
            })
        } else {
            Ok(())
        };
        finish_input_pulse(pulse, release, resume)?;
        Ok(json!({
            "status": "completed",
            "buttons": buttons,
            "frames": frames,
            "state": if was_running { "running" } else { "frozen" },
            "input_override": !self.held_buttons.is_empty(),
        }))
    }

    fn save_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("save_state")?;
        let path = state_path(params)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let path_var = tcl_utf8_value(&path)?;
        self.control.command(&format!(
            "set emucap_path {path_var}; store_machine [machine] $emucap_path"
        ))?;
        if !path.is_file() {
            return Err(OpenMsxBridgeError::Emulator(format!(
                "openMSX did not create savestate {}",
                path.display()
            )));
        }
        Ok(json!({
            "status": "completed",
            "saved": path.display().to_string(),
            "state": "frozen",
        }))
    }

    fn load_state(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("load_state")?;
        let path = state_path(params)?;
        if !path.is_file() {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "savestate does not exist: {}",
                path.display()
            )));
        }
        let path_var = tcl_utf8_value(&path)?;
        self.control.command(&format!(
            "set emucap_path {path_var}; set newID [restore_machine $emucap_path]; \
             set oldID [machine]; if {{$oldID ne \"\"}} {{delete_machine $oldID}}; \
             activate_machine $newID; set pause on"
        ))?;
        self.frozen = true;
        let held = self.held_buttons.clone();
        self.release_supported_keys()?;
        self.press_key_set(&held)?;
        Ok(json!({
            "status": "completed",
            "loaded": path.display().to_string(),
            "state": "frozen",
            "frame": self.current_frame()?,
        }))
    }

    fn reset(&mut self) -> BridgeResult<Value> {
        let held = self.held_buttons.clone();
        self.release_supported_keys()?;
        self.control.command("reset; set pause on")?;
        self.frozen = true;
        self.press_key_set(&held)?;
        Ok(json!({
            "status": "completed",
            "state": "frozen",
            "frame": self.current_frame()?,
        }))
    }

    fn screenshot(&mut self) -> BridgeResult<Value> {
        self.require_frozen("screenshot")?;
        let before = self.current_frame()?;
        self.screenshot_sequence += 1;
        let directory = self.runtime_home.join("screenshots");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!(
            "capture-{}-{}.png",
            std::process::id(),
            self.screenshot_sequence
        ));
        let result = (|| {
            let path_var = tcl_utf8_value(&path)?;
            self.control.command(&format!(
                "set emucap_path {path_var}; screenshot -raw -size 320 $emucap_path"
            ))?;
            let png = fs::read(&path)?;
            if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
                return Err(OpenMsxBridgeError::Protocol(
                    "openMSX screenshot is not a complete PNG".into(),
                ));
            }
            let after = self.current_frame()?;
            if after != before {
                return Err(OpenMsxBridgeError::Emulator(format!(
                    "openMSX screenshot advanced guest time: {before} -> {after}"
                )));
            }
            let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
            let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
            let sha256 = format!("{:x}", Sha256::digest(&png));
            Ok(json!({
                "png_base64": base64::engine::general_purpose::STANDARD.encode(&png),
                "sha256": sha256,
                "byte_len": png.len(),
                "width": width,
                "height": height,
                "frame": before,
                "frame_before": before,
                "frame_after": after,
                "frame_stable": true,
                "state": "frozen",
                "freshness": "current_screen",
            }))
        })();
        let _ = fs::remove_file(path);
        result
    }

    fn refresh_execution_state(&mut self) -> BridgeResult<()> {
        self.frozen = matches!(self.control.command("set pause")?.as_str(), "true" | "1");
        Ok(())
    }

    fn require_frozen(&mut self, operation: &str) -> BridgeResult<()> {
        self.refresh_execution_state()?;
        if self.frozen {
            Ok(())
        } else {
            Err(OpenMsxBridgeError::BadState(format!(
                "{operation} requires frozen state"
            )))
        }
    }

    fn current_frame(&mut self) -> BridgeResult<u64> {
        parse_decimal(
            &self.control.command("machine_info VDP_frame_count")?,
            "VDP frame count",
        )
    }

    fn release_debug_break(&mut self) -> BridgeResult<()> {
        match self.control.command("debug breaked")?.trim() {
            "0" => Ok(()),
            "1" => self.control.command("debug cont").map(|_| ()),
            value => Err(OpenMsxBridgeError::Protocol(format!(
                "openMSX returned an invalid debug break state: {value:?}"
            ))),
        }
    }

    fn input_matrix(&mut self) -> BridgeResult<Value> {
        let mut rows = serde_json::Map::new();
        for row in [2_u8, 6, 7, 8] {
            let value = parse_decimal(
                &self
                    .control
                    .command(&format!("debug read keymatrix {row}"))?,
                "keyboard matrix row",
            )?;
            if value > u8::MAX as u64 {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "openMSX returned an invalid keyboard matrix byte for row {row}: {value}"
                )));
            }
            rows.insert(row.to_string(), json!(value));
        }
        Ok(Value::Object(rows))
    }

    fn validate_range(&self, memory_type: &str, address: u64, length: u64) -> BridgeResult<()> {
        let size = self.region_sizes[memory_type];
        if !matches!(address.checked_add(length), Some(end) if end <= size) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "{memory_type} access out of range: {address:#x}+{length:#x} exceeds {size:#x}"
            )));
        }
        Ok(())
    }

    fn apply_buttons(&mut self, buttons: &BTreeSet<String>) -> BridgeResult<()> {
        let previous = self.held_buttons.clone();
        let operation = (|| {
            self.release_supported_keys()?;
            self.press_key_set(buttons)?;
            Ok(())
        })();
        match operation {
            Ok(()) => {
                self.held_buttons = buttons.clone();
                Ok(())
            }
            Err(primary) => {
                let cleanup = self
                    .release_supported_keys()
                    .and_then(|_| self.press_key_set(&previous));
                self.held_buttons = previous;
                match cleanup {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(OpenMsxBridgeError::Emulator(format!(
                        "{primary}; input rollback also failed: {cleanup}"
                    ))),
                }
            }
        }
    }

    fn release_supported_keys(&mut self) -> BridgeResult<()> {
        for (row, mask) in row_masks(INPUT_BUTTONS.iter().copied()) {
            self.control.command(&format!("keymatrixup {row} {mask}"))?;
        }
        Ok(())
    }

    fn press_key_set(&mut self, buttons: &BTreeSet<String>) -> BridgeResult<()> {
        for (row, mask) in row_masks(buttons.iter().map(String::as_str)) {
            self.control
                .command(&format!("keymatrixdown {row} {mask}"))?;
        }
        Ok(())
    }
}

fn finish_input_pulse(
    pulse: BridgeResult<Value>,
    release: BridgeResult<()>,
    resume: BridgeResult<()>,
) -> BridgeResult<Value> {
    let mut errors = Vec::new();
    let value = match pulse {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(error.to_string());
            None
        }
    };
    if let Err(error) = release {
        errors.push(format!("input release failed: {error}"));
    }
    if let Err(error) = resume {
        errors.push(format!("execution-state restore failed: {error}"));
    }
    if errors.is_empty() {
        Ok(value.expect("successful pulse has a value"))
    } else {
        Err(OpenMsxBridgeError::Emulator(errors.join("; ")))
    }
}

fn validate_step_count(count: u64) -> BridgeResult<()> {
    if (1..=10_000).contains(&count) {
        Ok(())
    } else {
        Err(OpenMsxBridgeError::BadParams(
            "step count must be in 1..=10000".into(),
        ))
    }
}

fn validate_groups(params: &Value, allowed: &[&str]) -> BridgeResult<()> {
    let Some(groups) = params.get("groups") else {
        return Ok(());
    };
    let groups = groups
        .as_array()
        .ok_or_else(|| OpenMsxBridgeError::BadParams("groups must be a list".into()))?;
    for group in groups {
        let group = group
            .as_str()
            .ok_or_else(|| OpenMsxBridgeError::BadParams("groups must contain strings".into()))?;
        if !allowed.contains(&group) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "unsupported state group: {group}"
            )));
        }
    }
    Ok(())
}

fn state_path(params: &Value) -> BridgeResult<PathBuf> {
    let path = PathBuf::from(
        params
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| OpenMsxBridgeError::BadParams("path must be a string".into()))?,
    );
    if !path.is_absolute() {
        return Err(OpenMsxBridgeError::BadParams(
            "savestate path must be absolute".into(),
        ));
    }
    if path.as_os_str().len() > 4096 {
        return Err(OpenMsxBridgeError::BadParams(
            "savestate path exceeds 4096 bytes".into(),
        ));
    }
    Ok(path)
}

fn tcl_utf8_value(path: &Path) -> BridgeResult<String> {
    let raw = path.to_str().ok_or_else(|| {
        OpenMsxBridgeError::BadParams("path must be valid UTF-8 for openMSX".into())
    })?;
    Ok(format!(
        "[encoding convertfrom utf-8 [binary decode hex {}]]",
        hex::encode(raw.as_bytes())
    ))
}

fn memory_type(params: &Value) -> BridgeResult<&str> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("memory");
    if matches!(memory_type, "memory" | "ram" | "vram") {
        Ok(memory_type)
    } else {
        Err(OpenMsxBridgeError::BadParams(format!(
            "unsupported MSX memory_type: {memory_type}"
        )))
    }
}

fn debuggable_name(memory_type: &str) -> &'static str {
    match memory_type {
        "memory" => "memory",
        "ram" => "{Main RAM}",
        "vram" => "VRAM",
        _ => unreachable!("validated memory type"),
    }
}

fn require_z80(params: &Value) -> BridgeResult<()> {
    match params.get("cpu").and_then(Value::as_str) {
        None | Some("z80" | "main") => Ok(()),
        Some(cpu) => Err(OpenMsxBridgeError::BadParams(format!(
            "the first MSX profile supports the Z80 CPU only, got {cpu}"
        ))),
    }
}

fn required_num(params: &Value, name: &str) -> BridgeResult<u64> {
    optional_num(params, name)?
        .ok_or_else(|| OpenMsxBridgeError::BadParams(format!("{name} is required")))
}

fn optional_num(params: &Value, name: &str) -> BridgeResult<Option<u64>> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| {
            OpenMsxBridgeError::BadParams(format!("{name} must be a non-negative integer"))
        }),
        Some(Value::String(raw)) => crate::numparse::parse_num_str(raw)
            .map(Some)
            .map_err(|error| OpenMsxBridgeError::BadParams(format!("{name}: {error}"))),
        Some(_) => Err(OpenMsxBridgeError::BadParams(format!(
            "{name} must be an integer or hexadecimal string"
        ))),
    }
}

fn parse_decimal(raw: &str, label: &str) -> BridgeResult<u64> {
    raw.trim().parse::<u64>().map_err(|error| {
        OpenMsxBridgeError::Protocol(format!(
            "openMSX {label} was not an unsigned integer: {raw:?}: {error}"
        ))
    })
}

fn error_kind(error: &OpenMsxBridgeError) -> &'static str {
    match error {
        OpenMsxBridgeError::BadParams(_) => "bad_params",
        OpenMsxBridgeError::BadState(_) => "bad_state",
        OpenMsxBridgeError::UnknownMethod(_) => "unknown_method",
        OpenMsxBridgeError::Unsupported(_) => "unsupported",
        OpenMsxBridgeError::Emulator(_) => "emulator_error",
        OpenMsxBridgeError::Protocol(_) | OpenMsxBridgeError::Io(_) => "bridge_error",
    }
}

fn tag_text(line: &str, tag: &str) -> Option<String> {
    let start = line.find('>')? + 1;
    let end = line.rfind(&format!("</{tag}>"))?;
    Some(xml_unescape(line[start..end].trim()))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape(text: &str) -> String {
    text.replace("&#x0a;", "\n")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
#[path = "openmsx_bridge_tests.rs"]
mod tests;
