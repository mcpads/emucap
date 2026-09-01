//! Managed PC-98 compatibility backend backed by a pinned NP2kai libretro core.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use libloading::Library;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

mod debug;
mod ffi;
mod operations;
mod support;

use ffi::*;
use support::*;

const MAX_SYNC_FRAMES: u64 = crate::live::temporal::MAX_SYNC_ADVANCE_COUNT;
const MAX_INPUT_PULSE_FRAMES: u64 = 120;
const HOST_FEATURES: &[&str] = &["controlled_start"];
const INSTRUCTION_STEP_FAILED: i32 = 0;
const INSTRUCTION_STEP_EXECUTED: i32 = 1;
const INSTRUCTION_STEP_PREEMPTED: i32 = 2;
const METHODS: &[&str] = &[
    "hello",
    "status",
    "read_memory",
    "find_pattern",
    "dump_memory",
    "get_rom_info",
    "change_media",
    "write_memory",
    "get_state",
    "probe",
    "poll_events",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "pause",
    "resume",
    "step",
    "step_instructions",
    "run_frames",
    "disassemble",
    "watch_register",
    "set_trace",
    "get_trace",
    "call_stack",
    "break_on_reset",
    "set_input",
    "press_buttons",
    "move_pointer",
    "screenshot",
    "save_state",
    "load_state",
    "reset",
];

const MEMORY_REGIONS: &[MemoryRegion] = &[
    MemoryRegion::new("cpu", 0x00000, 0x100000),
    MemoryRegion::new("physical", 0x00000, 0x100000),
    MemoryRegion::new("ram", 0x00000, 0x100000),
    MemoryRegion::new("tvram", 0xA0000, 0x4000),
    MemoryRegion::new("gvram_b", 0xA8000, 0x8000),
    MemoryRegion::new("gvram_r", 0xB0000, 0x8000),
    MemoryRegion::new("gvram_g", 0xB8000, 0x8000),
    MemoryRegion::new("gvram_i", 0xE0000, 0x8000),
];
const DUMP_REGIONS: &[&str] = &["ram", "tvram", "gvram_b", "gvram_r", "gvram_g", "gvram_i"];

#[derive(Clone, Copy, Debug)]
struct MemoryRegion {
    name: &'static str,
    base: u32,
    size: u32,
}

impl MemoryRegion {
    const fn new(name: &'static str, base: u32, size: u32) -> Self {
        Self { name, base, size }
    }
}

#[derive(Clone, Debug)]
struct SnapshotSpec {
    memory_type: String,
    address: u32,
    length: u32,
}

#[derive(Clone, Debug)]
struct DebugBreakpoint {
    kind: String,
    start: Option<u32>,
    end: Option<u32>,
    memory_type: Option<String>,
    pause_on_hit: bool,
    snapshots: Vec<SnapshotSpec>,
    register: Option<String>,
    min: Option<u32>,
    max: Option<u32>,
}
const ACTIVE_EXCEPTIONS: &[&str] = &[
    "np2kai.breakpoint.snapshot-pausing-only",
    "np2kai.breakpoint.read-value-unavailable",
    "np2kai.call-stack.best-effort",
    "np2kai.disassembly.current-mode",
    "np2kai.input-hold.port-zero-only",
    "np2kai.input-pulse.constraints",
    "np2kai.media.hdd0-only",
    "np2kai.media.hdd0-required",
    "np2kai.pointer-relative.constraints",
    "np2kai.screenshot.frozen-only",
    "np2kai.state-save.frozen-only",
    "np2kai.state-load.frozen-only",
];

#[derive(Debug, thiserror::Error)]
pub enum Np2kaiError {
    #[error("{0}")]
    BadParams(String),
    #[error("{0}")]
    BadState(String),
    #[error("unsupported NP2kai method: {0}")]
    Unsupported(String),
    #[error("NP2kai core error: {0}")]
    Core(String),
    #[error("dynamic library error: {0}")]
    Dynamic(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

type Np2kaiResult<T> = Result<T, Np2kaiError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StateIdentity {
    format: String,
    system: String,
    target_os: String,
    target_arch: String,
    media_sha256: String,
    firmware_sha256: String,
    core_sha256: String,
    upstream_commit: String,
    patchset_sha256: String,
    build_profile: String,
    frontend_build: String,
    state_sha256: String,
}

pub struct Np2kaiHost {
    api: CoreApi,
    content_path: PathBuf,
    content_size: u64,
    content_sha1: String,
    content_sha256: String,
    firmware_sha256: String,
    core_sha256: String,
    upstream_commit: String,
    patchset_sha256: String,
    build_profile: String,
    frontend_build: String,
    name: Option<String>,
    session_token: Option<String>,
    launch_id: Option<String>,
    runtime_home: PathBuf,
    held_buttons: BTreeSet<String>,
    controlled_start: bool,
    frozen: bool,
    initialized: bool,
    video_fresh: bool,
    frame: u64,
    fps: f64,
    loaded: bool,
    core_initialized: bool,
    breakpoints: BTreeMap<u64, DebugBreakpoint>,
    pending_events: Vec<Value>,
    next_breakpoint_id: u64,
    break_on_reset_id: Option<u64>,
    tracing: bool,
    trace_rows: Vec<Value>,
    dropped_trace: u64,
}

impl Np2kaiHost {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        content_path: &Path,
        core_path: &Path,
        firmware_dir: &Path,
        runtime_home: &Path,
        upstream_commit: &str,
        patchset_sha256: &str,
        build_profile: &str,
    ) -> Np2kaiResult<Self> {
        validate_content(content_path)?;
        if !core_path.is_file() {
            return Err(Np2kaiError::BadParams(format!(
                "NP2kai core not found: {}",
                core_path.display()
            )));
        }
        fs::create_dir_all(runtime_home)?;
        let system_root = runtime_home.join("system");
        let system_dir = system_root.join("np2kai");
        let save_dir = runtime_home.join("save");
        fs::create_dir_all(&system_dir)?;
        fs::create_dir_all(&save_dir)?;
        let firmware_sha256 = stage_firmware(firmware_dir, &system_dir)?;

        let system_root_c = path_cstring(&system_root)?;
        let save_dir_c = path_cstring(&save_dir)?;
        let mut options = HashMap::new();
        for (key, value) in [
            ("np2kai_keyboard", "Jp"),
            ("np2kai_async_cpu", "OFF"),
            ("np2kai_uselasthddmount", "OFF"),
            ("np2kai_keyrepeat", "ON"),
            ("np2kai_joymode", "Default"),
        ] {
            options.insert(
                key.to_string(),
                CString::new(value).expect("static NP2kai option"),
            );
        }
        {
            let mut slot = callback_slot()
                .lock()
                .unwrap_or_else(|value| value.into_inner());
            if slot.is_some() {
                return Err(Np2kaiError::BadState(
                    "another NP2kai core is already active in this process".into(),
                ));
            }
            *slot = Some(CallbackState {
                system_dir: system_root_c,
                save_dir: save_dir_c,
                options,
                keys: BTreeSet::new(),
                mouse_buttons: 0,
                mouse_dx: 0,
                mouse_dy: 0,
                frame: CapturedFrame {
                    pixel_format: PIXEL_0RGB1555,
                    ..CapturedFrame::default()
                },
                av_info: RetroSystemAvInfo::default(),
            });
        }

        let api = match unsafe { CoreApi::load(core_path) } {
            Ok(api) => api,
            Err(error) => {
                clear_callback_state();
                return Err(error);
            }
        };
        if unsafe { (api.api_version)() } != RETRO_API_VERSION {
            clear_callback_state();
            return Err(Np2kaiError::Core(format!(
                "unsupported libretro API version: expected {RETRO_API_VERSION}"
            )));
        }
        unsafe {
            (api.set_environment)(environment_callback);
            (api.set_video_refresh)(video_callback);
            (api.set_audio_sample)(audio_sample_callback);
            (api.set_audio_sample_batch)(audio_batch_callback);
            (api.set_input_poll)(input_poll_callback);
            (api.set_input_state)(input_state_callback);
            (api.init)();
        }
        let content_c = path_cstring(content_path)?;
        let game_info = RetroGameInfo {
            path: content_c.as_ptr(),
            data: ptr::null(),
            size: 0,
            meta: ptr::null(),
        };
        if !unsafe { (api.load_game)(&game_info) } {
            unsafe { (api.deinit)() };
            clear_callback_state();
            return Err(Np2kaiError::Core("retro_load_game failed".into()));
        }
        let mut av_info = RetroSystemAvInfo::default();
        unsafe { (api.get_system_av_info)(&mut av_info) };
        if !av_info.timing.fps.is_finite() || av_info.timing.fps <= 0.0 {
            unsafe {
                (api.unload_game)();
                (api.deinit)();
            }
            clear_callback_state();
            return Err(Np2kaiError::Core(format!(
                "invalid NP2kai frame rate: {}",
                av_info.timing.fps
            )));
        }

        let content_path = content_path.canonicalize()?;
        let content_size = content_path.metadata()?.len();
        let content_sha1 = crate::rom::sha1_of_file(&content_path)?;
        let content_sha256 = sha256_file(&content_path)?;
        let controlled_start = std::env::var("EMUCAP_START_FROZEN").ok().as_deref() == Some("1");
        Ok(Self {
            api,
            content_path,
            content_size,
            content_sha1,
            content_sha256,
            firmware_sha256,
            core_sha256: sha256_file(core_path)?,
            upstream_commit: upstream_commit.to_string(),
            patchset_sha256: patchset_sha256.to_string(),
            build_profile: build_profile.to_string(),
            frontend_build: crate::build_identity::BUILD_HASH.to_string(),
            name: std::env::var("EMUCAP_NAME").ok(),
            session_token: std::env::var("EMUCAP_SESSION_TOKEN").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
            runtime_home: runtime_home.to_path_buf(),
            held_buttons: BTreeSet::new(),
            controlled_start,
            frozen: controlled_start,
            initialized: false,
            video_fresh: false,
            frame: 0,
            fps: av_info.timing.fps,
            loaded: true,
            core_initialized: true,
            breakpoints: BTreeMap::new(),
            pending_events: Vec::new(),
            next_breakpoint_id: 1,
            break_on_reset_id: None,
            tracing: false,
            trace_rows: Vec::new(),
            dropped_trace: 0,
        })
    }

    pub fn is_running(&self) -> bool {
        !self.frozen
    }
    pub fn frame_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps.max(1.0))
    }
    pub fn run_scheduled_frame(&mut self) -> Np2kaiResult<()> {
        if !self.frozen {
            let _ = self.run_one_frame()?;
        }
        Ok(())
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id;
        let result = match request.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "get_rom_info" => self.get_rom_info(),
            "read_memory" => self.read_memory(&request.params),
            "find_pattern" => self.find_pattern(&request.params),
            "dump_memory" => self.dump_memory(&request.params),
            "change_media" => self.change_media(&request.params),
            "write_memory" => self.write_memory(&request.params),
            "get_state" => self.get_state(),
            "probe" => self.probe(&request.params),
            "poll_events" => self.poll_events(&request.params),
            "set_breakpoint" => self.set_breakpoint(&request.params),
            "clear_breakpoint" => self.clear_breakpoint(&request.params),
            "list_breakpoints" => self.list_breakpoints(),
            "clear_all_breakpoints" => self.clear_all_breakpoints(),
            "pause" => self.pause(),
            "resume" => self.resume(),
            "step" => self.step(&request.params),
            "step_instructions" => self.step_instructions(&request.params),
            "run_frames" => self.run_frames(&request.params),
            "disassemble" => self.disassemble(&request.params),
            "watch_register" => self.watch_register(&request.params),
            "set_trace" => self.set_trace(&request.params),
            "get_trace" => self.get_trace(&request.params),
            "call_stack" => self.call_stack(),
            "break_on_reset" => self.break_on_reset(&request.params),
            "set_input" => self.set_input(&request.params),
            "press_buttons" => self.press_buttons(&request.params),
            "move_pointer" => self.move_pointer(&request.params),
            "screenshot" => self.screenshot(),
            "save_state" => self.save_state(&request.params),
            "load_state" => self.load_state(&request.params),
            "reset" => self.reset(),
            other => Err(Np2kaiError::Unsupported(other.into())),
        };
        match result {
            Ok(result) => Response {
                id,
                ok: true,
                result: Some(result),
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
}

impl Drop for Np2kaiHost {
    fn drop(&mut self) {
        set_controls(&BTreeSet::new());
        unsafe {
            if self.loaded {
                (self.api.unload_game)();
                self.loaded = false;
            }
            if self.core_initialized {
                (self.api.deinit)();
                self.core_initialized = false;
            }
        }
        clear_callback_state();
    }
}

#[cfg(test)]
#[path = "np2kai_adapter_tests.rs"]
mod tests;
