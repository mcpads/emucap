//! Pinned openMSX XML-control bridge.
//!
//! openMSX already exposes the execution, debugger, input, screenshot, and
//! savestate primitives needed by the first MSX cartridge profile.  This
//! module translates its XML stdio channel into emucap's NDJSON adapter
//! protocol and uses the pinned host extension for readback-checked joystick
//! ownership.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::launch::openmsx::{
    MediaKind, OpenMsxProfile, PreparedSession, REQUIRED_HOST_API as OPENMSX_HOST_API,
};
use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

#[path = "openmsx_bridge/breakpoints.rs"]
mod breakpoints;
#[path = "openmsx_bridge/frame.rs"]
mod frame;
#[path = "openmsx_bridge/input.rs"]
mod input;
#[path = "openmsx_bridge/joystick.rs"]
mod joystick;
#[path = "openmsx_bridge/state.rs"]
mod state;
#[path = "openmsx_bridge/xml.rs"]
mod xml;

use breakpoints::{breakpoint_kinds, PublicBreakpoint, DEBUGGER_EXCEPTION};
#[cfg(test)]
use input::button_position;
use input::{
    input_port, joystick_buttons, joystick_mask, normalize_joystick_buttons,
    normalize_keyboard_buttons, row_masks, InputPort, JOYSTICK_BUTTONS, KEYBOARD_BUTTONS,
};
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
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "poll_events",
    "disassemble",
];
const BASE_EXCEPTIONS: &[&str] = &[
    "openmsx.state-read.frozen-only",
    "openmsx.memory-read.frozen-only",
    "openmsx.memory-read.bounded",
    "openmsx.memory-write.frozen-only",
    "openmsx.input-pulse.constraints",
    "openmsx.execution-step.z80-only",
    DEBUGGER_EXCEPTION,
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
    session: PreparedSession,
    runtime_home: PathBuf,
    display: bool,
    frozen: bool,
    region_sizes: BTreeMap<&'static str, u64>,
    held_buttons: BTreeSet<String>,
    joystick_owners: [Option<u8>; 2],
    breakpoints: BTreeMap<u64, PublicBreakpoint>,
    next_breakpoint_id: u64,
    debug_events: VecDeque<Value>,
    last_hit_seq: u64,
    debugger_fatal: Option<String>,
    frame_probe_native_id: Option<String>,
    screenshot_sequence: u64,
    name: Option<String>,
    session_token: Option<String>,
    launch_id: Option<String>,
}

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub fn new(
        mut control: C,
        session: &PreparedSession,
        runtime_home: &Path,
        display: bool,
    ) -> BridgeResult<Self> {
        let profile = session.verify()?;
        if profile == OpenMsxProfile::MsxTurboR {
            return Err(OpenMsxBridgeError::Unsupported(
                "MSX turboR requires a separately proven active Z80/R800 bridge".into(),
            ));
        }
        if control.command("openmsx_info version")? != "openMSX 21.0" {
            return Err(OpenMsxBridgeError::Unsupported(
                "the MSX adapter requires openMSX 21.0".into(),
            ));
        }
        if control.command("machine_info config_name")? != session.machine
            || control.command("machine_info type")? != session.machine_type
        {
            return Err(OpenMsxBridgeError::Unsupported(format!(
                "{} requires openMSX machine {} ({})",
                session.system, session.machine, session.machine_type
            )));
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
        let (memory, ram, vram) = profile.expected_region_sizes();
        if region_sizes["memory"] != memory
            || region_sizes["vram"] != vram
            || region_sizes["ram"] != ram
        {
            return Err(OpenMsxBridgeError::Unsupported(format!(
                "{} memory layout changed: {region_sizes:?}",
                session.machine
            )));
        }
        if parse_decimal(
            &control.command("debug size emucap_joystick_override")?,
            "emucap joystick override size",
        )? != 2
        {
            return Err(OpenMsxBridgeError::Unsupported(
                "the MSX adapter requires the pinned joystick-override host API".into(),
            ));
        }

        let mut bridge = Self {
            control,
            session: session.clone(),
            runtime_home: runtime_home.to_path_buf(),
            display,
            frozen: true,
            region_sizes,
            held_buttons: BTreeSet::new(),
            joystick_owners: [None; 2],
            breakpoints: BTreeMap::new(),
            next_breakpoint_id: 1,
            debug_events: VecDeque::new(),
            last_hit_seq: 0,
            debugger_fatal: None,
            frame_probe_native_id: None,
            screenshot_sequence: 0,
            name: std::env::var("EMUCAP_NAME").ok(),
            session_token: std::env::var("EMUCAP_SESSION_TOKEN").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
        };
        bridge.require_runtime_identity("initialization")?;
        bridge.initialize_debugger()?;
        Ok(bridge)
    }

    pub fn child_pid(&self) -> u32 {
        self.control.child_pid()
    }

    pub fn backend_terminal(&self) -> bool {
        self.control.is_terminal() || self.debugger_fatal.is_some()
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
            "set_breakpoint" => self.set_breakpoint(&request.params),
            "clear_breakpoint" => self.clear_breakpoint(&request.params),
            "list_breakpoints" => self.list_breakpoints(),
            "clear_all_breakpoints" => self.clear_all_breakpoints(),
            "poll_events" => self.poll_events(&request.params),
            "disassemble" => self.disassemble(&request.params),
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
            "system": self.session.system,
            "adapter": "openmsx-rust-xml",
            "backend": "openMSX 21.0",
            "host_api": OPENMSX_HOST_API,
            "debugger": true,
            "methods": self.methods(),
            "memory_types": ["memory", "ram", "vram"],
            "region_sizes": self.region_sizes,
            "breakpoint_kinds": breakpoint_kinds(),
            "contracts": crate::contracts::advertisement_value(&self.active_exceptions()),
            "capability_notes": {
                "machine": self.session.machine,
                "machine_type": self.session.machine_type,
                "media": self.session.media.kind.as_str(),
                "firmware_manifest_sha256": self.session.firmware_manifest_sha256,
                "cpu": ["z80"],
                "step_units": ["frames", "instructions"],
                "input_ports": {
                    "0": "keyboard",
                    "1": "joystick",
                    "2": "joystick"
                },
                "display": self.display,
                "headless_screenshot": false,
            },
            "content": self.session.media.source_path.display().to_string(),
            "content_sha1": self.session.media.source_sha1,
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
        self.drain_debug_events()?;
        self.refresh_execution_state()?;
        let input_matrix = self.input_matrix()?;
        let joystick_owners = self.checked_joystick_owners()?;
        let joystick_values = [self.guest_joystick_value(0)?, self.guest_joystick_value(1)?];
        let any_input_override =
            !self.held_buttons.is_empty() || joystick_owners.iter().any(Option::is_some);
        Ok(json!({
            "connected": !self.control.is_terminal(),
            "system": self.session.system,
            "adapter": "openmsx-rust-xml",
            "backend": "openMSX 21.0",
            "state": if self.frozen { "frozen" } else { "running" },
            "frame": self.current_frame()?,
            "methods": self.methods(),
            "memory_types": ["memory", "ram", "vram"],
            "region_sizes": self.region_sizes,
            "breakpoint_kinds": breakpoint_kinds(),
            "queued_events": self.debug_events.len(),
            "debugger_fatal": self.debugger_fatal.as_deref(),
            "input_override": any_input_override,
            "input_owner": {
                "keyboard": if self.held_buttons.is_empty() { "native" } else { "persistent" },
                "joystick1": if joystick_owners[0].is_some() { "persistent" } else { "native" },
                "joystick2": if joystick_owners[1].is_some() { "persistent" } else { "native" },
            },
            "input_matrix": input_matrix,
            "joystick_ports": [
                {
                    "port": 1,
                    "engaged": joystick_owners[0].is_some(),
                    "active_low_mask": joystick_owners[0],
                    "guest_value": joystick_values[0],
                    "buttons": joystick_owners[0].map(joystick_buttons).unwrap_or_default(),
                },
                {
                    "port": 2,
                    "engaged": joystick_owners[1].is_some(),
                    "active_low_mask": joystick_owners[1],
                    "guest_value": joystick_values[1],
                    "buttons": joystick_owners[1].map(joystick_buttons).unwrap_or_default(),
                }
            ],
            "input_buttons": {
                "system": "msx",
                "buttons": KEYBOARD_BUTTONS,
                "aliases": {
                    "start": "enter",
                    "return": "enter",
                    "fire1": "space",
                    "fire2": "ctrl"
                },
                "devices": [
                    {"port": 0, "device": "keyboard", "buttons": KEYBOARD_BUTTONS},
                    {
                        "port": 1,
                        "device": "joystick",
                        "buttons": JOYSTICK_BUTTONS,
                        "aliases": {"fire1": "a", "fire2": "b", "button1": "a", "button2": "b"}
                    },
                    {
                        "port": 2,
                        "device": "joystick",
                        "buttons": JOYSTICK_BUTTONS,
                        "aliases": {"fire1": "a", "fire2": "b", "button1": "a", "button2": "b"}
                    }
                ],
                "notes": "Port 0 is the standard MSX keyboard matrix. Ports 1 and 2 are independent active-low MSX joysticks."
            },
            "backend_pid": self.control.child_pid(),
            "launch_id": self.launch_id,
        }))
    }

    fn get_rom_info(&self) -> BridgeResult<Value> {
        Ok(json!({
            "system": self.session.system,
            "machine": self.session.machine,
            "machine_type": self.session.machine_type,
            "media": self.session.media.kind.as_str(),
            "path": self.session.media.source_path.display().to_string(),
            "sha1": self.session.media.source_sha1,
            "size": self.session.media.source_size,
            "mounted_path": self.session.media.mounted_path.display().to_string(),
            "firmware_manifest_sha256": self.session.firmware_manifest_sha256,
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
        self.refresh_execution_state()?;
        if self.frozen {
            return Ok(json!({
                "status": "completed",
                "state": "frozen",
                "frame": self.current_frame()?,
            }));
        }
        self.control.command("set pause on")?;
        if let Err(primary) = self.control.command("debug break") {
            let cleanup = self
                .release_debug_break()
                .and_then(|_| self.control.command("set pause off").map(|_| ()))
                .and_then(|_| self.refresh_execution_state());
            return match cleanup {
                Ok(()) if !self.frozen => Err(primary),
                Ok(()) => self.fail_debugger(format!(
                    "{primary}; pause rollback did not restore running state"
                )),
                Err(cleanup) => {
                    self.fail_debugger(format!("{primary}; pause rollback also failed: {cleanup}"))
                }
            };
        }
        if let Err(error) = self.require_stop_conjunction("pause") {
            return self.fail_debugger(error.to_string());
        }
        Ok(json!({
            "status": "completed",
            "state": "frozen",
            "frame": self.current_frame()?,
        }))
    }

    fn resume(&mut self, params: &Value) -> BridgeResult<Value> {
        require_z80(params)?;
        self.release_debug_break()?;
        self.refresh_execution_state()?;
        if self.frozen {
            let dropped = self.drain_debug_events()?;
            if !self.debug_events.is_empty() || dropped != 0 {
                return Ok(json!({
                    "status":"interrupted",
                    "reason":"breakpoint",
                    "state":"frozen",
                    "event_pending":!self.debug_events.is_empty(),
                }));
            }
            return Err(OpenMsxBridgeError::Emulator(
                "openMSX resumed into a stop without a breakpoint event".into(),
            ));
        }
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
        self.prepare_temporal_request("frame step")?;
        let before = self.current_frame()?;
        if let Err(primary) = self.control.advance_frames(count) {
            let diagnostic = self
                .control
                .command("::emucap::frame_debug")
                .unwrap_or_else(|error| format!("unavailable: {error}"));
            let cleanup = self
                .control
                .command("::emucap::cancel_frame")
                .and_then(|_| self.control.command("set pause on"))
                .and_then(|_| self.control.command("debug break"))
                .and_then(|_| self.require_stop_conjunction("failed frame step cleanup"));
            return match cleanup {
                Ok(()) => Err(OpenMsxBridgeError::Emulator(format!(
                    "{primary}; frame diagnostic: {diagnostic}"
                ))),
                Err(cleanup) => self.fail_debugger(format!(
                    "{primary}; frame diagnostic: {diagnostic}; \
                     frame target cleanup also failed: {cleanup}"
                )),
            };
        }
        let after = self.current_frame()?;
        let dropped = self.drain_debug_events()?;
        if !self.debug_events.is_empty() || dropped != 0 {
            if let Err(error) = self.control.command("::emucap::cancel_frame") {
                return self.fail_debugger(format!(
                    "breakpoint interrupted frame step but target cleanup failed: {error}"
                ));
            }
            self.require_stop_conjunction("breakpoint-interrupted frame step")?;
            return Ok(json!({
                "status":"interrupted",
                "reason":"breakpoint",
                "unit":"frames",
                "count":after.saturating_sub(before),
                "requested":count,
                "frame_before":before,
                "frame":after,
                "state":"frozen",
                "event_pending":!self.debug_events.is_empty(),
            }));
        }
        if self.control.command("debug breaked")?.trim() == "1" {
            return self.fail_debugger(
                "openMSX entered CPU debug break without a valid callback event".into(),
            );
        }
        self.control.command("debug break")?;
        self.control.command("set pause on")?;
        self.require_stop_conjunction("frame step")?;
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
        self.prepare_temporal_request("instruction step")?;
        let pc_before = parse_decimal(&self.control.command("reg PC")?, "PC")?;
        let mut completed = 0_u64;
        for _ in 0..count {
            self.control.command("debug step")?;
            completed += 1;
            self.control.command("set pause on")?;
            let dropped = self.drain_debug_events()?;
            if !self.debug_events.is_empty() || dropped != 0 {
                self.require_stop_conjunction("breakpoint-interrupted instruction step")?;
                return Ok(json!({
                    "status":"interrupted",
                    "reason":"breakpoint",
                    "unit":"instructions",
                    "cpu":"z80",
                    "count":completed,
                    "requested":count,
                    "pc_before":pc_before,
                    "pc":parse_decimal(&self.control.command("reg PC")?, "PC")?,
                    "frame":self.current_frame()?,
                    "state":"frozen",
                    "event_pending":!self.debug_events.is_empty(),
                }));
            }
        }
        let pc = parse_decimal(&self.control.command("reg PC")?, "PC")?;
        self.control.command("set pause on")?;
        self.require_stop_conjunction("instruction step")?;
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
        match input_port(params)? {
            InputPort::Keyboard => {
                let buttons = normalize_keyboard_buttons(params.get("buttons"))?;
                self.apply_buttons(&buttons)?;
                Ok(json!({
                    "port": 0,
                    "device": "keyboard",
                    "buttons": buttons,
                    "mode": if self.held_buttons.is_empty() { "native" } else { "persistent" },
                    "input_override": !self.held_buttons.is_empty(),
                }))
            }
            InputPort::Joystick(index) => {
                let buttons = normalize_joystick_buttons(params.get("buttons"))?;
                let desired = (!buttons.is_empty()).then(|| joystick_mask(&buttons));
                self.apply_joystick_owner(index, desired)?;
                Ok(json!({
                    "port": index + 1,
                    "device": "joystick",
                    "buttons": buttons,
                    "active_low_mask": desired,
                    "mode": if desired.is_some() { "persistent" } else { "native" },
                    "input_override": desired.is_some(),
                }))
            }
        }
    }

    fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        let frames = optional_num(params, "frames")?.unwrap_or(1);
        if !(1..=MAX_INPUT_FRAMES).contains(&frames) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "press_buttons frames must be in 1..={MAX_INPUT_FRAMES}"
            )));
        }
        match input_port(params)? {
            InputPort::Keyboard => {
                let buttons = normalize_keyboard_buttons(params.get("buttons"))?;
                self.press_keyboard_buttons(buttons, frames)
            }
            InputPort::Joystick(index) => {
                let buttons = normalize_joystick_buttons(params.get("buttons"))?;
                self.press_joystick_buttons(index, buttons, frames)
            }
        }
    }

    fn press_keyboard_buttons(
        &mut self,
        buttons: BTreeSet<String>,
        frames: u64,
    ) -> BridgeResult<Value> {
        if buttons.is_empty() {
            return Err(OpenMsxBridgeError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        self.refresh_execution_state()?;
        let was_running = !self.frozen;
        if was_running {
            self.pause(&json!({}))?;
        }
        let persistent = self.held_buttons.clone();
        let mut combined = persistent.clone();
        combined.extend(buttons.iter().cloned());
        if let Err(primary) = self.apply_buttons(&combined) {
            if was_running {
                return match self.resume(&json!({})) {
                    Ok(_) => Err(primary),
                    Err(restore) => Err(OpenMsxBridgeError::Emulator(format!(
                        "{primary}; execution-state restore also failed: {restore}"
                    ))),
                };
            }
            return Err(primary);
        }
        let pulse = self.frame_step(frames);
        let interrupted = pulse
            .as_ref()
            .is_ok_and(|value| value.get("status").and_then(Value::as_str) == Some("interrupted"));
        let release = self.apply_buttons(&persistent);
        let resume = if was_running && !interrupted {
            self.resume(&json!({})).map(|_| ())
        } else {
            Ok(())
        };
        let mut pulse = finish_input_pulse(pulse, release, resume)?;
        if interrupted {
            let object = pulse.as_object_mut().expect("step result is an object");
            object.insert("port".into(), json!(0));
            object.insert("device".into(), json!("keyboard"));
            object.insert("buttons".into(), json!(buttons));
            object.insert(
                "input_override".into(),
                json!(!self.held_buttons.is_empty()),
            );
            return Ok(pulse);
        }
        Ok(json!({
            "status": "completed",
            "port": 0,
            "device": "keyboard",
            "buttons": buttons,
            "frames": frames,
            "state": if was_running { "running" } else { "frozen" },
            "input_override": !self.held_buttons.is_empty(),
        }))
    }

    fn refresh_execution_state(&mut self) -> BridgeResult<()> {
        let (paused, breaked) = self.query_stop_state()?;
        if paused != breaked {
            self.frozen = false;
            return Err(OpenMsxBridgeError::Emulator(format!(
                "openMSX stop mechanisms diverged: pause={paused}, debug_break={breaked}"
            )));
        }
        self.frozen = paused;
        Ok(())
    }

    fn require_runtime_identity(&mut self, operation: &str) -> BridgeResult<()> {
        let machine = self.control.command("machine_info config_name")?;
        let machine_type = self.control.command("machine_info type")?;
        if machine != self.session.machine || machine_type != self.session.machine_type {
            return Err(OpenMsxBridgeError::Emulator(format!(
                "openMSX identity drift during {operation}: expected {} ({}), observed {machine} ({machine_type})",
                self.session.machine, self.session.machine_type
            )));
        }
        let media_slot = match self.session.media.kind {
            MediaKind::Cartridge => "carta",
            MediaKind::Disk => "diska",
            MediaKind::Cassette => "cassetteplayer",
        };
        let encoded = self.control.command(&format!(
            "binary encode hex [encoding convertto utf-8 \
             [dict get [machine_info media {media_slot}] target]]"
        ))?;
        let bytes = hex::decode(encoded.trim()).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!(
                "openMSX returned invalid mounted-media path hex: {error}"
            ))
        })?;
        let observed = PathBuf::from(String::from_utf8(bytes).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!(
                "openMSX returned a non-UTF-8 mounted-media path: {error}"
            ))
        })?);
        let expected = fs::canonicalize(&self.session.media.mounted_path)?;
        let observed = fs::canonicalize(&observed).map_err(|error| {
            OpenMsxBridgeError::Emulator(format!(
                "openMSX mounted-media path is not accessible during {operation}: {}: {error}",
                observed.display()
            ))
        })?;
        if observed != expected {
            return Err(OpenMsxBridgeError::Emulator(format!(
                "openMSX media identity drift during {operation}: expected {}, observed {}",
                expected.display(),
                observed.display()
            )));
        }
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
            &self.control.command("::emucap::frame_seq")?,
            "emucap frame sequence",
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
        for (row, mask) in row_masks(KEYBOARD_BUTTONS.iter().copied()) {
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
