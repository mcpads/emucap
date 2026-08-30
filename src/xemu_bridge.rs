//! Original Xbox xemu QMP/GDB bridge.
//!
//! The pinned xemu fork exposes guest-frame, controller, screenshot, and Xbox-disc operations over
//! QMP. Standard QEMU GDB-RSP supplies CPU state, memory, instruction stepping, and breakpoints.
//! This bridge combines both transports into terminal emucap operations; neither endpoint is part
//! of the public API on its own.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::gdb_rsp::{GdbBridgeEnv, GdbError, GdbTransport};
use crate::live::protocol::{ProtocolError, Request, Response};
use crate::qmp::{QmpError, QmpTransport};

pub const REQUIRED_HOST_API: u64 = 1;
const HOST_FEATURES: &[&str] = &["controlled_start"];
const MAX_MEMORY_TRANSFER: usize = 0x2_0000;
const MAX_MEMORY_CHUNK: usize = 0x2000;
const MAX_FIND_LEN: usize = 0x2_0000;
const XBOX_RAM_CPU_ALIAS: u64 = 0x8000_0000;
const XBOX_RAM_SIZE: u64 = 0x0400_0000;
const SCREENSHOT_POLL_INTERVAL: Duration = Duration::from_millis(8);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_POLL_INTERVAL: Duration = Duration::from_millis(8);
const CONTRACT_EXCEPTIONS: &[&str] = &[
    "xemu.state-save.frozen-only",
    "xemu.state-load.frozen-only",
    "xemu.state-load.same-generation-only",
];

const METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "get_state",
    "read_memory",
    "write_memory",
    "find_pattern",
    "dump_memory",
    "pause",
    "resume",
    "step",
    "step_instructions",
    "set_input",
    "screenshot",
    "reset",
    "change_media",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "poll_events",
    "disassemble",
    "call_stack",
    "save_state",
    "load_state",
    "probe",
];

// Recognize historical wire names only to return a stable unsupported error. They are not a
// roadmap and must never appear in capability metadata; the advertised METHODS list is complete.
const KNOWN_UNAVAILABLE_WIRE_METHODS: &[&str] = &[
    "run_frames",
    "press_buttons",
    "watch_register",
    "set_trace",
    "get_trace",
    "break_on_reset",
];

#[derive(Debug, Clone)]
struct XemuBreakpoint {
    kind: String,
    memory_type: String,
    start: u64,
    end: u64,
    absolute: u64,
    length: u64,
    ztype: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct InputState {
    buttons: Vec<String>,
    mask: u16,
    axes: BTreeMap<String, i16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XemuMachineIdentity {
    pub mcpx_sha256: Option<String>,
    pub flash_sha256: Option<String>,
    pub hdd_template_sha256: Option<String>,
    pub eeprom_initial_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XemuHostBuildIdentity {
    pub upstream: String,
    pub tag: String,
    pub commit: String,
    pub host_api: u32,
    pub patchset_sha256: String,
    pub binary_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XemuStateEnvironment {
    pub hdd: PathBuf,
    pub eeprom: PathBuf,
    pub host_build: XemuHostBuildIdentity,
}

impl XemuStateEnvironment {
    pub fn from_process_env() -> Result<Self, String> {
        let required_path = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} is required for managed xemu state"))
        };
        let required = |name: &str| {
            std::env::var(name).map_err(|_| format!("{name} is required for managed xemu state"))
        };
        let hdd = required_path("EMUCAP_XEMU_HDD_PATH")?;
        let eeprom = required_path("EMUCAP_XEMU_EEPROM_PATH")?;
        if !hdd.is_absolute() || !eeprom.is_absolute() {
            return Err("managed xemu HDD and EEPROM paths must be absolute".into());
        }
        let host_api = required("EMUCAP_XEMU_HOST_API")?
            .parse::<u32>()
            .map_err(|_| "EMUCAP_XEMU_HOST_API must be an integer".to_string())?;
        let host_build = XemuHostBuildIdentity {
            upstream: required("EMUCAP_XEMU_HOST_UPSTREAM")?,
            tag: required("EMUCAP_XEMU_HOST_TAG")?,
            commit: required("EMUCAP_XEMU_HOST_COMMIT")?,
            host_api,
            patchset_sha256: required("EMUCAP_XEMU_HOST_PATCHSET_SHA256")?,
            binary_sha256: required("EMUCAP_XEMU_HOST_BINARY_SHA256")?,
        };
        host_build.validate()?;
        Ok(Self {
            hdd,
            eeprom,
            host_build,
        })
    }
}

impl XemuHostBuildIdentity {
    fn validate(&self) -> Result<(), String> {
        if self.upstream.is_empty() || self.tag.is_empty() {
            return Err("managed xemu host upstream and tag must not be empty".into());
        }
        if self.host_api as u64 != REQUIRED_HOST_API {
            return Err(format!(
                "managed xemu host API mismatch: need {REQUIRED_HOST_API}, got {}",
                self.host_api
            ));
        }
        if self.commit.len() != 40 || !self.commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("managed xemu host commit is not a 40-digit hexadecimal hash".into());
        }
        for (name, value) in [
            ("patchset", self.patchset_sha256.as_str()),
            ("binary", self.binary_sha256.as_str()),
        ] {
            if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!(
                    "managed xemu host {name} identity is not a SHA-256"
                ));
            }
        }
        Ok(())
    }
}

impl XemuMachineIdentity {
    pub fn from_process_env() -> Result<Self, String> {
        let identity = Self {
            mcpx_sha256: std::env::var("EMUCAP_XEMU_MCPX_SHA256").ok(),
            flash_sha256: std::env::var("EMUCAP_XEMU_FLASH_SHA256").ok(),
            hdd_template_sha256: std::env::var("EMUCAP_XEMU_HDD_TEMPLATE_SHA256").ok(),
            eeprom_initial_sha256: std::env::var("EMUCAP_XEMU_EEPROM_INITIAL_SHA256").ok(),
        };
        let values = [
            identity.mcpx_sha256.as_deref(),
            identity.flash_sha256.as_deref(),
            identity.hdd_template_sha256.as_deref(),
            identity.eeprom_initial_sha256.as_deref(),
        ];
        let present = values.iter().filter(|value| value.is_some()).count();
        if present != 0 && present != values.len() {
            return Err("managed xemu machine identity is incomplete".into());
        }
        if values
            .iter()
            .flatten()
            .any(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err("managed xemu machine identity contains an invalid SHA-256".into());
        }
        Ok(identity)
    }

    fn value(&self) -> Value {
        match (
            self.mcpx_sha256.as_deref(),
            self.flash_sha256.as_deref(),
            self.hdd_template_sha256.as_deref(),
            self.eeprom_initial_sha256.as_deref(),
        ) {
            (Some(mcpx), Some(flash), Some(hdd), Some(eeprom)) => json!({
                "mcpx": {"sha256": mcpx, "role": "immutable_generation_copy"},
                "flash": {"sha256": flash, "role": "immutable_generation_copy"},
                "hdd": {"template_sha256": hdd, "role": "mutable_generation_copy"},
                "eeprom": {"initial_sha256": eeprom, "role": "mutable_generation_copy"},
            }),
            _ => Value::Null,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum XemuBridgeError {
    #[error("{0}")]
    BadParams(String),
    #[error("{0}")]
    BadState(String),
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("unsupported on xbox: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Emulator(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Gdb(#[from] GdbError),
    #[error(transparent)]
    Qmp(#[from] QmpError),
}

type XemuResult<T> = Result<T, XemuBridgeError>;

pub struct XemuBridge<Q, G> {
    qmp: Q,
    gdb: G,
    env: GdbBridgeEnv,
    screen_root: PathBuf,
    current_disc: Option<PathBuf>,
    breakpoints: BTreeMap<u64, XemuBreakpoint>,
    next_breakpoint_id: u64,
    events: Vec<Value>,
    debug_stop_observed: bool,
    next_screenshot_id: u64,
    held_input: Option<InputState>,
    controlled_start: bool,
    machine_identity: XemuMachineIdentity,
    state_environment: XemuStateEnvironment,
    next_state_job_id: u64,
    state_integrity_error: Option<String>,
    pending_state_snapshot_cleanup: Vec<(String, String)>,
    state_media_identity_cache: Option<state::MediaIdentityCache>,
    hello_completed: bool,
}

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub fn new(
        qmp: Q,
        gdb: G,
        env: GdbBridgeEnv,
        screen_root: PathBuf,
        controlled_start: bool,
        machine_identity: XemuMachineIdentity,
        state_environment: XemuStateEnvironment,
    ) -> Self {
        let current_disc = env.content.clone();
        Self {
            qmp,
            gdb,
            env,
            screen_root,
            current_disc,
            breakpoints: BTreeMap::new(),
            next_breakpoint_id: 1,
            events: Vec::new(),
            debug_stop_observed: false,
            next_screenshot_id: 1,
            held_input: None,
            controlled_start,
            machine_identity,
            state_environment,
            next_state_job_id: 1,
            state_integrity_error: None,
            pending_state_snapshot_cleanup: Vec::new(),
            state_media_identity_cache: None,
            hello_completed: false,
        }
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id;
        let result = if let Some(reason) = self.state_integrity_error.as_deref() {
            if request.method == "status" {
                self.status()
            } else {
                Err(XemuBridgeError::BadState(format!(
                    "Xbox state integrity is unresolved after a failed restore; inspect status and stop this exact generation before further control: {reason}"
                )))
            }
        } else {
            match request.method.as_str() {
                "hello" => self.hello(),
                "status" => self.status(),
                "get_rom_info" => self.get_rom_info(),
                "get_state" => self.get_state(),
                "read_memory" => self.read_memory(&request.params),
                "write_memory" => self.write_memory(&request.params),
                "find_pattern" => self.find_pattern(&request.params),
                "dump_memory" => self.dump_memory(&request.params),
                "pause" => self.pause(),
                "resume" => self.resume(),
                "step" => self.step(&request.params),
                "step_instructions" => self.step_instructions(&request.params),
                "set_input" => self.set_input(&request.params),
                "screenshot" => self.screenshot(),
                "reset" => self.reset(),
                "change_media" => self.change_media(&request.params),
                "set_breakpoint" => self.set_breakpoint(&request.params),
                "clear_breakpoint" => self.clear_breakpoint(&request.params),
                "list_breakpoints" => self.list_breakpoints(),
                "clear_all_breakpoints" => self.clear_all_breakpoints(),
                "poll_events" => self.poll_events(&request.params),
                "disassemble" => self.disassemble(&request.params),
                "call_stack" => self.call_stack(),
                "save_state" => self.save_state(&request.params),
                "load_state" => self.load_state(&request.params),
                "probe" => self.probe(&request.params),
                other if KNOWN_UNAVAILABLE_WIRE_METHODS.contains(&other) => {
                    Err(XemuBridgeError::Unsupported(other.into()))
                }
                other => Err(XemuBridgeError::UnknownMethod(other.into())),
            }
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

    pub fn backend_terminal(&self) -> bool {
        self.qmp.is_terminal() || self.gdb.is_terminal()
    }
}

mod debug;
mod input;
mod media;
mod memory;
mod service;
mod state;
mod support;
mod video;

use support::*;

#[cfg(test)]
#[path = "xemu_bridge_tests.rs"]
mod tests;
