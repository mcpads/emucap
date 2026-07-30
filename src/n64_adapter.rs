//! Initial Nintendo 64 adapter backed by a debugger-enabled Mupen64Plus core.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

#[path = "n64_adapter_control.rs"]
mod control;
#[path = "n64_adapter_debug.rs"]
mod debug;
#[path = "n64_adapter_frame.rs"]
mod frame;
#[path = "n64_adapter_lifecycle.rs"]
mod lifecycle;
#[path = "n64_adapter_prepare.rs"]
mod prepare;

#[cfg(test)]
use frame::wait_frame_gate;
use frame::{
    arm_frame_gate, cancel_frame_gate, frame_gate_is_blocked, release_frame_gate, reset_frame_gate,
    wait_frame_gate_or_debug_update, FrameGateTrigger, FrameWaitOutcome,
};
use lifecycle::{
    current_readiness, debug_init_callback, debug_log_callback, debug_update_callback,
    debug_vi_callback, reset_observation_state, state_callback,
};

const CORE_API_VERSION: c_int = 0x020001;
const M64TYPE_INT: c_int = 1;
const M64TYPE_BOOL: c_int = 3;
const M64TYPE_STRING: c_int = 4;
const M64CMD_ROM_OPEN: c_int = 1;
const M64CMD_ROM_CLOSE: c_int = 2;
const M64CMD_EXECUTE: c_int = 5;
const M64CMD_STOP: c_int = 6;
const M64CMD_RESET: c_int = 19;
const M64CMD_STATE_LOAD: c_int = 10;
const M64CMD_STATE_SAVE: c_int = 11;
const M64CMD_SEND_SDL_KEYDOWN: c_int = 13;
const M64CMD_SEND_SDL_KEYUP: c_int = 14;
const M64CMD_SET_FRAME_CALLBACK: c_int = 15;
const M64CMD_TAKE_NEXT_SCREENSHOT: c_int = 16;
const M64CORE_STATE_LOADCOMPLETE: c_int = 10;
const M64CORE_STATE_SAVECOMPLETE: c_int = 11;
const M64CORE_SCREENSHOT_CAPTURED: c_int = 12;
const M64P_DBG_RUN_STATE: c_int = 1;
const M64P_DBG_RUNSTATE_PAUSED: c_int = 0;
const M64P_DBG_RUNSTATE_RUNNING: c_int = 2;
const M64P_CPU_PC: c_int = 1;
const M64P_CPU_REG_REG: c_int = 2;
const M64P_CPU_REG_HI: c_int = 3;
const M64P_CPU_REG_LO: c_int = 4;
const M64PLUGIN_RSP: c_int = 1;
const M64PLUGIN_GFX: c_int = 2;
const M64PLUGIN_INPUT: c_int = 4;

const RDRAM_BASE: u64 = 0x8000_0000;
const RDRAM_SIZE: u64 = 8 * 1024 * 1024;
const MAX_MEMORY_TRANSFER: u64 = 16 * 1024;
const OPERATION_DEADLINE: Duration = Duration::from_secs(3);
const RECOVERY_DEADLINE: Duration = Duration::from_secs(1);
const COMPLETION_DEADLINE: Duration = Duration::from_secs(1);
const MAX_STEP_COUNT: u64 = crate::live::temporal::MAX_SYNC_ADVANCE_COUNT;
const MAX_INPUT_PULSE_FRAMES: u64 = 120;
const INPUT_BUTTONS: &[&str] = &[
    "a",
    "b",
    "z",
    "start",
    "l",
    "r",
    "up",
    "down",
    "left",
    "right",
    "dpad_up",
    "dpad_down",
    "dpad_left",
    "dpad_right",
    "c_up",
    "c_down",
    "c_left",
    "c_right",
];
const BASE_METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "get_state",
    "read_memory",
    "write_memory",
    "pause",
    "resume",
    "step_instructions",
    "set_input",
    "reset",
    "set_breakpoint",
    "clear_breakpoint",
    "list_breakpoints",
    "clear_all_breakpoints",
    "poll_events",
    "disassemble",
];
const ACTIVE_EXCEPTIONS: &[&str] = &[
    "n64.state-read.frozen-only",
    "n64.memory-read.frozen-only",
    "n64.memory-read.bounded",
    "n64.memory-write.frozen-only",
    "n64.execution-step.r4300-only",
    "n64.execution-pause.r4300-only",
    "n64.execution-resume.r4300-only",
    "n64.input-set.port-zero-only",
    "n64.breakpoint.pausing-subset",
];

static DEBUG_READY: AtomicBool = AtomicBool::new(false);
static INITIAL_RELEASED: AtomicBool = AtomicBool::new(false);
static EXECUTION_TERMINAL: AtomicBool = AtomicBool::new(false);
static UPDATE_COUNT: AtomicU64 = AtomicU64::new(0);
static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static FRAME_SEEN: AtomicBool = AtomicBool::new(false);
static VI_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_PC: AtomicU32 = AtomicU32::new(0);
static STATE_SAVE_RESULT: AtomicI32 = AtomicI32::new(-1);
static STATE_LOAD_RESULT: AtomicI32 = AtomicI32::new(-1);
static SCREENSHOT_RESULT: AtomicI32 = AtomicI32::new(-1);

type CoreStartup = unsafe extern "C" fn(
    c_int,
    *const c_char,
    *const c_char,
    *mut c_void,
    extern "C" fn(*mut c_void, c_int, *const c_char),
    *mut c_void,
    extern "C" fn(*mut c_void, c_int, c_int),
) -> c_int;
type CoreShutdown = unsafe extern "C" fn() -> c_int;
type CoreAttachPlugin = unsafe extern "C" fn(c_int, *mut c_void) -> c_int;
type CoreDetachPlugin = unsafe extern "C" fn(c_int) -> c_int;
type CoreDoCommand = unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int;
type ConfigOpenSection = unsafe extern "C" fn(*const c_char, *mut *mut c_void) -> c_int;
type ConfigSetParameter =
    unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_void) -> c_int;
type DebugSetCallbacks =
    unsafe extern "C" fn(extern "C" fn(), extern "C" fn(u32), extern "C" fn()) -> c_int;
type DebugSetRunState = unsafe extern "C" fn(c_int) -> c_int;
type DebugGetState = unsafe extern "C" fn(c_int) -> c_int;
type DebugStep = unsafe extern "C" fn() -> c_int;
type DebugGetCpuDataPtr = unsafe extern "C" fn(c_int) -> *mut c_void;
type DebugMemRead8 = unsafe extern "C" fn(u32) -> u8;
type DebugMemWrite8 = unsafe extern "C" fn(u32, u8);
type DebugBreakpointCommand =
    unsafe extern "C" fn(c_int, u32, *mut debug::NativeBreakpoint) -> c_int;
type DebugBreakpointLookup = unsafe extern "C" fn(u32, u32, u32) -> c_int;
type DebugBreakpointConsume = unsafe extern "C" fn(*mut u32, *mut u32);
type DebugDecodeOp = unsafe extern "C" fn(u32, *mut c_char, *mut c_char, c_int);
type PluginStartup = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    extern "C" fn(*mut c_void, c_int, *const c_char),
) -> c_int;
type PluginShutdown = unsafe extern "C" fn() -> c_int;

#[derive(Clone, Copy)]
struct Api {
    core_shutdown: CoreShutdown,
    core_attach_plugin: CoreAttachPlugin,
    core_detach_plugin: CoreDetachPlugin,
    core_do_command: CoreDoCommand,
    config_open_section: ConfigOpenSection,
    config_set_parameter: ConfigSetParameter,
    debug_set_callbacks: DebugSetCallbacks,
    debug_set_run_state: DebugSetRunState,
    debug_get_state: DebugGetState,
    debug_step: DebugStep,
    debug_get_cpu_data_ptr: DebugGetCpuDataPtr,
    debug_mem_read8: DebugMemRead8,
    debug_mem_write8: DebugMemWrite8,
    debug_breakpoint_command: DebugBreakpointCommand,
    debug_breakpoint_lookup: DebugBreakpointLookup,
    debug_breakpoint_consume: DebugBreakpointConsume,
    debug_decode_op: DebugDecodeOp,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

#[derive(Debug, thiserror::Error)]
pub enum N64Error {
    #[error("{0}")]
    BadParams(String),
    #[error("{0}")]
    BadState(String),
    #[error("unsupported N64 method: {0}")]
    Unsupported(String),
    #[error("Mupen64Plus {operation} failed with error {code}")]
    Core {
        operation: &'static str,
        code: c_int,
    },
    #[error("Mupen64Plus {0} timed out")]
    Timeout(&'static str),
    #[error(
        "Mupen64Plus {operation} did not close safely; the launch generation was stopped: {reason}"
    )]
    GenerationStopped {
        operation: &'static str,
        reason: String,
    },
    #[error("dynamic library error: {0}")]
    Dynamic(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type N64Result<T> = Result<T, N64Error>;

pub struct Mupen64PlusHost {
    api: Api,
    core_handle: *mut c_void,
    plugins: Vec<Plugin>,
    rom_path: PathBuf,
    name: Option<String>,
    session_token: Option<String>,
    launch_id: Option<String>,
    build: String,
    runtime_home: PathBuf,
    display: bool,
    frozen: bool,
    frame_paused: bool,
    frame_clock_synchronized: bool,
    held_buttons: BTreeSet<String>,
    breakpoints: BTreeMap<u64, debug::PublicBreakpoint>,
    next_breakpoint_id: u64,
    debug_events: VecDeque<Value>,
    last_debug_update_seen: u64,
    next_hit_seq: u64,
    next_reset_seq: u64,
    started: bool,
}

unsafe impl Send for Mupen64PlusHost {}

struct Plugin {
    kind: c_int,
    handle: *mut c_void,
    shutdown: PluginShutdown,
}

pub struct MupenExecution(Api);

impl MupenExecution {
    pub fn execute_blocking(self) -> N64Result<()> {
        let result = unsafe { (self.0.core_do_command)(M64CMD_EXECUTE, 0, ptr::null_mut()) };
        EXECUTION_TERMINAL.store(true, Ordering::Release);
        check_core("EXECUTE", result)
    }
}

impl Mupen64PlusHost {
    pub fn begin_execution(&mut self) -> MupenExecution {
        self.started = true;
        MupenExecution(self.api)
    }

    pub fn release_initial_pause(&self) -> N64Result<()> {
        wait_until("debugger initialization", OPERATION_DEADLINE, || {
            DEBUG_READY.load(Ordering::Acquire)
        })?;
        check_core("DebugSetRunState(running)", unsafe {
            (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_RUNNING)
        })?;
        check_core("DebugStep(initial release)", unsafe {
            (self.api.debug_step)()
        })?;
        INITIAL_RELEASED.store(true, Ordering::Release);
        Ok(())
    }

    pub fn terminal_reason() -> Option<String> {
        EXECUTION_TERMINAL
            .load(Ordering::Acquire)
            .then(|| "Mupen64Plus execution terminated".to_string())
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id;
        let result = match request.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "get_rom_info" => self.get_rom_info(),
            "get_state" => self.get_state(),
            "read_memory" => self.read_memory(&request.params),
            "write_memory" => self.write_memory(&request.params),
            "pause" => self.pause(&request.params),
            "resume" => self.resume(&request.params),
            "step" if self.display => self.step(&request.params),
            "run_frames" if self.display => self.run_frames(&request.params),
            "step_instructions" => self.step_instructions(&request.params),
            "set_input" => self.set_input(&request.params),
            "press_buttons" if self.display => self.press_buttons(&request.params),
            "screenshot" if self.display => self.screenshot(),
            "save_state" if self.display => self.save_state(&request.params),
            "load_state" if self.display => self.load_state(&request.params),
            "reset" => self.reset(&request.params),
            "set_breakpoint" => self.set_breakpoint(&request.params),
            "clear_breakpoint" => self.clear_breakpoint(&request.params),
            "list_breakpoints" => self.list_breakpoints(),
            "clear_all_breakpoints" => self.clear_all_breakpoints(),
            "poll_events" => self.poll_events(&request.params),
            "disassemble" => self.disassemble(&request.params),
            other => Err(N64Error::Unsupported(other.into())),
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

    fn hello(&self) -> N64Result<Value> {
        let methods = self.methods();
        let active_exceptions = self.active_exceptions();
        let mut value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "n64",
            "adapter": "mupen64plus-native",
            "backend": "mupen64plus-core",
            "debugger": true,
            "methods": methods,
            "memory_types": ["rdram"],
            "region_sizes": {"rdram": RDRAM_SIZE},
            "breakpoint_kinds": debug::breakpoint_kinds(),
            "input_buttons": INPUT_BUTTONS,
            "execution_limits": {
                "max_sync_advance_count": MAX_STEP_COUNT,
                "max_sync_operation_ms":
                    crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
                "frame": {
                    "max_count": if self.display { MAX_STEP_COUNT } else { 0 }
                }
            },
            "contracts": crate::contracts::advertisement_value(&active_exceptions),
            "capability_notes": {
                "implemented_methods": methods,
                "step_units": if self.display {
                    json!(["frames", "instructions"])
                } else {
                    json!(["instructions"])
                },
                "step_cpus": ["r4300"],
                "execution_mode": "pure_interpreter",
                "rsp_observation": "not_exposed",
                "frame_source": if self.display {
                    "rendered_frame_callback"
                } else {
                    "vi_callback"
                },
                "display": self.display
            },
            "content": self.rom_path.display().to_string(),
            "build": self.build,
        });
        let object = value.as_object_mut().expect("N64 hello object");
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

    fn status(&mut self) -> N64Result<Value> {
        self.drain_debug_update()?;
        let readiness = current_readiness(self.display);
        let connected = readiness.is_ready();
        if connected {
            self.frozen = (self.frame_paused && frame_gate_is_blocked())
                || unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) }
                    == M64P_DBG_RUNSTATE_PAUSED;
        }
        let mut value = json!({
            "connected": connected,
            "readiness": readiness.label(),
            "system": "n64",
            "adapter": "mupen64plus-native",
            "backend": "mupen64plus-core",
            "debugger": true,
            "frame": self.public_frame(),
            "rendered_frame": FRAME_COUNT.load(Ordering::Acquire),
            "rendered_frame_observed": FRAME_SEEN.load(Ordering::Acquire),
            "rendered_frame_synchronized": self.frame_clock_synchronized,
            "vi_count": VI_COUNT.load(Ordering::Acquire),
            "methods": self.methods(),
            "memory_types": ["rdram"],
            "region_sizes": {"rdram": RDRAM_SIZE},
            "breakpoint_kinds": debug::breakpoint_kinds()
            ,"display": self.display,
            "input_buttons": INPUT_BUTTONS,
            "execution_limits": {
                "max_sync_advance_count": MAX_STEP_COUNT,
                "max_sync_operation_ms":
                    crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
                "frame": {
                    "max_count": if self.display { MAX_STEP_COUNT } else { 0 }
                }
            },
            "input_override": {
                "engaged": !self.held_buttons.is_empty(),
                "buttons": self.held_buttons.iter().collect::<Vec<_>>(),
                "ownership": if self.held_buttons.is_empty() {
                    "native"
                } else {
                    "shared_with_native"
                }
            }
        });
        if connected {
            value.as_object_mut().expect("N64 status object").insert(
                "state".into(),
                json!(if self.frozen { "frozen" } else { "running" }),
            );
        }
        Ok(value)
    }

    fn get_rom_info(&self) -> N64Result<Value> {
        let mut file = File::open(&self.rom_path)?;
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
                .ok_or_else(|| N64Error::BadParams("ROM size overflow".into()))?;
        }
        Ok(json!({
            "system": "n64",
            "adapter": "mupen64plus-native",
            "name": self.rom_path.file_name().and_then(|v| v.to_str()).unwrap_or(""),
            "path": self.rom_path.canonicalize()?.display().to_string(),
            "sha1": hex::encode(hasher.finalize()),
            "size": size,
            "media_type": self.rom_path.extension().and_then(|v| v.to_str()).unwrap_or("").to_ascii_lowercase()
        }))
    }

    fn pause(&mut self, params: &Value) -> N64Result<Value> {
        require_r4300(params)?;
        self.require_connected()?;
        if let Some(event) = self.drain_debug_update()? {
            return Ok(json!({
                "status":"completed",
                "state":"frozen",
                "reason":"breakpoint",
                "breakpoint_id":event["id"],
                "event":event,
            }));
        }
        if !self.frame_paused
            && unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) } != M64P_DBG_RUNSTATE_PAUSED
        {
            let before = UPDATE_COUNT.load(Ordering::Acquire);
            check_core("DebugSetRunState(paused)", unsafe {
                (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_PAUSED)
            })?;
            wait_until("pause boundary", OPERATION_DEADLINE, || {
                UPDATE_COUNT.load(Ordering::Acquire) > before
                    && unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) }
                        == M64P_DBG_RUNSTATE_PAUSED
            })?;
            self.frame_paused = false;
        }
        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "state": "frozen",
            "frame": self.public_frame(),
            "vi_count": VI_COUNT.load(Ordering::Acquire),
            "pc": LAST_PC.load(Ordering::Acquire)
        }))
    }

    fn resume(&mut self, params: &Value) -> N64Result<Value> {
        require_r4300(params)?;
        self.require_connected()?;
        self.drain_debug_update()?;
        let was_paused =
            unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) } == M64P_DBG_RUNSTATE_PAUSED;
        check_core("DebugSetRunState(running)", unsafe {
            (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_RUNNING)
        })?;
        if self.frame_paused {
            if let Err(error) = release_frame_gate() {
                let recovery = self.recover_frame_failure(&error);
                return recovery.and(Err(error));
            }
        } else if was_paused {
            if let Err(error) = check_core("DebugStep(resume)", unsafe { (self.api.debug_step)() })
            {
                let _ = unsafe { (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_PAUSED) };
                return Err(error);
            }
        }
        self.frozen = false;
        self.frame_paused = false;
        Ok(json!({"status":"completed", "state":"running"}))
    }

    fn step_instructions(&mut self, params: &Value) -> N64Result<Value> {
        require_r4300(params)?;
        self.require_frozen("instruction step")?;
        let count = optional_num(params, "count")?.unwrap_or(1);
        if !(1..=MAX_STEP_COUNT).contains(&count) {
            return Err(N64Error::BadParams(format!(
                "instruction step count must be in 1..={MAX_STEP_COUNT}, got {count}"
            )));
        }
        let deadline = crate::live::temporal::OperationDeadline::after(
            crate::live::temporal::MAX_SYNC_OPERATION_TIME,
        );
        let before = UPDATE_COUNT.load(Ordering::Acquire);
        let mut completed = 0;
        if self.frame_paused {
            check_core("DebugSetRunState(instruction boundary)", unsafe {
                (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_PAUSED)
            })?;
            if let Err(error) = release_frame_gate() {
                let recovery = self.recover_frame_failure(&error);
                return recovery.and(Err(error));
            }
            let timeout = remaining_step_timeout(deadline, "instruction step total budget")?;
            if let Err(error) =
                wait_until("instruction boundary after frame pause", timeout, || {
                    UPDATE_COUNT.load(Ordering::Acquire) > before
                        && unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) }
                            == M64P_DBG_RUNSTATE_PAUSED
                })
            {
                self.recover_instruction_failure(before.saturating_add(1), &error)?;
                return Err(error);
            }
            self.frame_paused = false;
            completed = 1;
        }
        for expected in (completed + 1)..=count {
            let timeout = remaining_step_timeout(deadline, "instruction step total budget")?;
            check_core("DebugStep", unsafe { (self.api.debug_step)() })?;
            if let Err(error) = wait_until("instruction step", timeout, || {
                UPDATE_COUNT.load(Ordering::Acquire) >= before.saturating_add(expected)
            }) {
                self.recover_instruction_failure(before.saturating_add(expected), &error)?;
                return Err(error);
            }
            if let Some(event) = self.drain_debug_update()? {
                return Ok(json!({
                    "status":"interrupted",
                    "reason":"breakpoint",
                    "breakpoint_id":event["id"],
                    "event":event,
                    "unit":"instructions",
                    "completed":expected,
                    "state":"frozen",
                }));
            }
        }
        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "count": count,
            "cpu": "r4300",
            "state": "frozen",
            "pc": LAST_PC.load(Ordering::Acquire),
            "frame": self.public_frame(),
            "vi_count": VI_COUNT.load(Ordering::Acquire)
        }))
    }

    fn step(&mut self, params: &Value) -> N64Result<Value> {
        self.step_with_frame_gate(params, FrameGateTrigger::NextFrame)
    }

    fn step_with_frame_gate(
        &mut self,
        params: &Value,
        first_trigger: FrameGateTrigger,
    ) -> N64Result<Value> {
        require_r4300(params)?;
        self.require_frozen("frame step")?;
        let count = optional_num(params, "count")?
            .or(optional_num(params, "frames")?)
            .unwrap_or(1);
        if !(1..=MAX_STEP_COUNT).contains(&count) {
            return Err(N64Error::BadParams(format!(
                "frame step count must be in 1..={MAX_STEP_COUNT}, got {count}"
            )));
        }
        let deadline = crate::live::temporal::OperationDeadline::after(
            crate::live::temporal::MAX_SYNC_OPERATION_TIME,
        );

        let mut release_debugger_pause = !self.frame_paused;
        let mut frame_before = FRAME_COUNT.load(Ordering::Acquire);
        let mut frame_before_verified = self.frame_clock_synchronized;
        let mut current_frame = frame_before;
        for index in 0..count {
            let continuing_from_frame_barrier = self.frame_paused;
            let observed_before = FRAME_COUNT.load(Ordering::Acquire);
            let observed_before_verified = self.frame_clock_synchronized;
            let debug_before = UPDATE_COUNT.load(Ordering::Acquire);
            let trigger = if index == 0 {
                first_trigger
            } else {
                FrameGateTrigger::NextFrame
            };
            arm_frame_gate(trigger)?;

            if self.frame_paused {
                if let Err(error) = release_frame_gate() {
                    let recovery = self.recover_frame_failure(&error);
                    return recovery.and(Err(error));
                }
                self.frame_paused = false;
            }

            if release_debugger_pause && !continuing_from_frame_barrier {
                check_core("DebugSetRunState(frame running)", unsafe {
                    (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_RUNNING)
                })?;
                if let Err(error) = check_core("DebugStep(frame release)", unsafe {
                    (self.api.debug_step)()
                }) {
                    cancel_frame_gate();
                    let _ = unsafe { (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_PAUSED) };
                    return Err(error);
                }
                release_debugger_pause = false;
            }

            let timeout = match remaining_step_timeout(deadline, "frame step total budget") {
                Ok(timeout) => timeout,
                Err(error) => {
                    self.recover_frame_failure(&error)?;
                    return Err(error);
                }
            };
            let observed = match wait_frame_gate_or_debug_update(timeout, debug_before) {
                Ok(FrameWaitOutcome::Frame(observed)) => observed,
                Ok(FrameWaitOutcome::DebugUpdate(update)) => {
                    cancel_frame_gate();
                    if let Some(event) = self.drain_debug_update()? {
                        self.frozen = true;
                        self.frame_paused = false;
                        return Ok(json!({
                            "status":"interrupted",
                            "reason":"breakpoint",
                            "breakpoint_id":event["id"],
                            "event":event,
                            "unit":"frames",
                            "completed":index,
                            "state":"frozen",
                        }));
                    }
                    let error = N64Error::BadState(
                        format!(
                            "N64 frame advance was interrupted by unclassified debugger update {update}"
                        ),
                    );
                    self.recover_frame_failure(&error)?;
                    return Err(error);
                }
                Err(error) => {
                    self.recover_frame_failure(&error)?;
                    return Err(error);
                }
            };
            if let Err(error) = validate_observed_frame(
                trigger,
                observed_before_verified,
                observed_before,
                observed,
            ) {
                self.recover_frame_failure(&error)?;
                return Err(error);
            }
            if !frame_before_verified {
                frame_before = observed.saturating_sub(1);
                frame_before_verified = true;
            }
            self.frame_clock_synchronized = true;
            current_frame = observed;

            // The callback barrier is the single owner of this exact boundary.
            // Do not combine it with M64CMD_ADVANCE_FRAME's separate core pause.
            self.frame_paused = true;
        }

        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "unit": "frames",
            "count": count,
            "cpu": "r4300",
            "state": "frozen",
            "pc": LAST_PC.load(Ordering::Acquire),
            "frame_before": frame_before,
            "frame": current_frame,
            "frame_before_verified": frame_before_verified,
            "vi_count": VI_COUNT.load(Ordering::Acquire)
        }))
    }

    fn get_state(&self) -> N64Result<Value> {
        self.require_frozen("get_state")?;
        let pc_ptr = unsafe { (self.api.debug_get_cpu_data_ptr)(M64P_CPU_PC) } as *const u32;
        let regs_ptr = unsafe { (self.api.debug_get_cpu_data_ptr)(M64P_CPU_REG_REG) } as *const u64;
        let hi_ptr = unsafe { (self.api.debug_get_cpu_data_ptr)(M64P_CPU_REG_HI) } as *const u64;
        let lo_ptr = unsafe { (self.api.debug_get_cpu_data_ptr)(M64P_CPU_REG_LO) } as *const u64;
        if pc_ptr.is_null() || regs_ptr.is_null() || hi_ptr.is_null() || lo_ptr.is_null() {
            return Err(N64Error::BadState(
                "Mupen64Plus returned a null R4300 state pointer".into(),
            ));
        }
        let mut registers = serde_json::Map::new();
        for index in 0..32 {
            registers.insert(format!("r{index}"), json!(unsafe { *regs_ptr.add(index) }));
        }
        registers.insert("pc".into(), json!(unsafe { *pc_ptr }));
        registers.insert("hi".into(), json!(unsafe { *hi_ptr }));
        registers.insert("lo".into(), json!(unsafe { *lo_ptr }));
        Ok(json!({
            "cpu": "r4300",
            "state": registers,
            "frame": self.public_frame(),
            "rendered_frame": FRAME_COUNT.load(Ordering::Acquire),
            "rendered_frame_observed": FRAME_SEEN.load(Ordering::Acquire),
            "rendered_frame_synchronized": self.frame_clock_synchronized,
            "vi_count": VI_COUNT.load(Ordering::Acquire)
        }))
    }

    fn read_memory(&self, params: &Value) -> N64Result<Value> {
        self.require_frozen("read_memory")?;
        let length = required_num(params, "length")?;
        if length > MAX_MEMORY_TRANSFER {
            return Err(N64Error::BadParams(format!(
                "read length {length:#x} exceeds {MAX_MEMORY_TRANSFER:#x}"
            )));
        }
        let offset = required_num(params, "address")?;
        let address = rdram_address(params, length)?;
        let mut data = Vec::with_capacity(length as usize);
        for index in 0..length {
            data.push(unsafe { (self.api.debug_mem_read8)((address + index) as u32) });
        }
        Ok(json!({"address":offset, "length":length, "hex":hex::encode(data)}))
    }

    fn write_memory(&self, params: &Value) -> N64Result<Value> {
        self.require_frozen("write_memory")?;
        let raw = params
            .get("hex")
            .or_else(|| params.get("data"))
            .and_then(Value::as_str)
            .ok_or_else(|| N64Error::BadParams("missing required param: hex".into()))?;
        let data = hex::decode(raw)
            .map_err(|_| N64Error::BadParams("hex must contain complete bytes".into()))?;
        if data.len() as u64 > MAX_MEMORY_TRANSFER {
            return Err(N64Error::BadParams(format!(
                "write length {:#x} exceeds {MAX_MEMORY_TRANSFER:#x}",
                data.len()
            )));
        }
        let offset = required_num(params, "address")?;
        let address = rdram_address(params, data.len() as u64)?;
        for (index, byte) in data.iter().copied().enumerate() {
            unsafe { (self.api.debug_mem_write8)(address as u32 + index as u32, byte) };
        }
        Ok(json!({"address":offset, "written":data.len()}))
    }

    fn require_connected(&self) -> N64Result<()> {
        let readiness = current_readiness(self.display);
        if readiness.is_ready() {
            Ok(())
        } else {
            Err(N64Error::BadState(format!(
                "Mupen64Plus adapter is not ready: {}",
                readiness.label()
            )))
        }
    }

    fn methods(&self) -> Vec<&'static str> {
        methods_for(self.display)
    }

    fn public_frame(&self) -> u64 {
        if self.display {
            FRAME_COUNT.load(Ordering::Acquire)
        } else {
            VI_COUNT.load(Ordering::Acquire)
        }
    }

    fn active_exceptions(&self) -> Vec<&'static str> {
        exceptions_for(self.display)
    }

    fn require_frozen(&self, operation: &str) -> N64Result<()> {
        self.require_connected()?;
        let debugger_paused =
            unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) } == M64P_DBG_RUNSTATE_PAUSED;
        let frame_barrier_paused = self.frame_paused && frame_gate_is_blocked();
        if self.frozen && (debugger_paused || frame_barrier_paused) {
            Ok(())
        } else {
            Err(N64Error::BadState(format!(
                "{operation} requires a frozen N64 machine; call pause first"
            )))
        }
    }

    fn recover_frame_failure(&mut self, original: &N64Error) -> N64Result<()> {
        cancel_frame_gate();
        let before = UPDATE_COUNT.load(Ordering::Acquire);
        self.recover_instruction_failure(before.saturating_add(1), original)
    }

    fn recover_instruction_failure(
        &mut self,
        minimum_update: u64,
        original: &N64Error,
    ) -> N64Result<()> {
        check_core("DebugSetRunState(operation recovery)", unsafe {
            (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_PAUSED)
        })?;
        wait_until("operation recovery", RECOVERY_DEADLINE, || {
            UPDATE_COUNT.load(Ordering::Acquire) >= minimum_update
                && unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) }
                    == M64P_DBG_RUNSTATE_PAUSED
        })
        .map_err(|recovery| {
            N64Error::BadState(format!(
                "{original}; debugger-boundary recovery also failed: {recovery}"
            ))
        })?;
        self.frozen = true;
        self.frame_paused = false;
        Ok(())
    }

    fn stop_generation_with_unresolved_effect(
        &mut self,
        operation: &'static str,
        cause: &N64Error,
    ) -> N64Error {
        cancel_frame_gate();
        self.frame_paused = false;
        self.frozen = false;
        let stop = unsafe { (self.api.core_do_command)(M64CMD_STOP, 0, ptr::null_mut()) };
        if stop != 0 {
            eprintln!(
                "[mupen64plus-native] {operation} cleanup failed ({cause}); \
                 M64CMD_STOP returned {stop}; aborting the dedicated frontend"
            );
            std::process::abort();
        }
        EXECUTION_TERMINAL.store(true, Ordering::Release);
        N64Error::GenerationStopped {
            operation,
            reason: cause.to_string(),
        }
    }
}

fn methods_for(display: bool) -> Vec<&'static str> {
    let mut methods = BASE_METHODS.to_vec();
    if display {
        methods.extend([
            "step",
            "run_frames",
            "press_buttons",
            "screenshot",
            "save_state",
            "load_state",
        ]);
    }
    methods
}

fn exceptions_for(display: bool) -> Vec<&'static str> {
    let mut exceptions = ACTIVE_EXCEPTIONS.to_vec();
    if display {
        exceptions.extend([
            "n64.input-pulse.bounded",
            "n64.screenshot.frozen-only",
            "n64.state-save.frozen-only",
            "n64.state-load.frozen-only",
        ]);
    } else {
        exceptions.push("n64.execution.frame-step-absent");
    }
    exceptions
}

fn display_requested() -> bool {
    std::env::var("EMUCAP_N64_DISPLAY")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

unsafe fn load_api(handle: *mut c_void) -> N64Result<Api> {
    Ok(Api {
        core_shutdown: symbol(handle, b"CoreShutdown\0")?,
        core_attach_plugin: symbol(handle, b"CoreAttachPlugin\0")?,
        core_detach_plugin: symbol(handle, b"CoreDetachPlugin\0")?,
        core_do_command: symbol(handle, b"CoreDoCommand\0")?,
        config_open_section: symbol(handle, b"ConfigOpenSection\0")?,
        config_set_parameter: symbol(handle, b"ConfigSetParameter\0")?,
        debug_set_callbacks: symbol(handle, b"DebugSetCallbacks\0")?,
        debug_set_run_state: symbol(handle, b"DebugSetRunState\0")?,
        debug_get_state: symbol(handle, b"DebugGetState\0")?,
        debug_step: symbol(handle, b"DebugStep\0")?,
        debug_get_cpu_data_ptr: symbol(handle, b"DebugGetCPUDataPtr\0")?,
        debug_mem_read8: symbol(handle, b"DebugMemRead8\0")?,
        debug_mem_write8: symbol(handle, b"DebugMemWrite8\0")?,
        debug_breakpoint_command: symbol(handle, b"DebugBreakpointCommand\0")?,
        debug_breakpoint_lookup: symbol(handle, b"DebugBreakpointLookup\0")?,
        debug_breakpoint_consume: symbol(handle, b"DebugBreakpointConsume\0")?,
        debug_decode_op: symbol(handle, b"DebugDecodeOp\0")?,
    })
}

unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &'static [u8]) -> N64Result<T> {
    libc::dlerror();
    let pointer = libc::dlsym(handle, cstr(name).as_ptr());
    if pointer.is_null() {
        return Err(N64Error::Dynamic(dl_error()));
    }
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    Ok(std::mem::transmute_copy(&pointer))
}

fn open_library(path: &Path) -> N64Result<*mut c_void> {
    let path = path_cstring(path)?;
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        Err(N64Error::Dynamic(dl_error()))
    } else {
        Ok(handle)
    }
}

fn dl_error() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "unknown dynamic loader error".into()
    } else {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

fn platform_library(root: &Path, stem: &str) -> N64Result<PathBuf> {
    for suffix in [".dylib", ".so", ".so.2"] {
        let path = root.join(format!("{stem}{suffix}"));
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(N64Error::BadParams(format!(
        "Mupen64Plus library not found under {}: {stem}",
        root.display()
    )))
}

fn path_cstring(path: &Path) -> N64Result<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| N64Error::BadParams(format!("path contains NUL: {}", path.display())))
}

fn cstr(bytes: &'static [u8]) -> &'static CStr {
    CStr::from_bytes_with_nul(bytes).expect("static C string")
}

fn set_config_int(
    api: &Api,
    section: *mut c_void,
    name: &'static [u8],
    kind: c_int,
    value: c_int,
) -> N64Result<()> {
    check_core("ConfigSetParameter", unsafe {
        (api.config_set_parameter)(
            section,
            cstr(name).as_ptr(),
            kind,
            &value as *const c_int as *const c_void,
        )
    })
}

fn set_config_string(
    api: &Api,
    section: *mut c_void,
    name: &'static [u8],
    value: &Path,
) -> N64Result<()> {
    let value = path_cstring(value)?;
    check_core("ConfigSetParameter", unsafe {
        (api.config_set_parameter)(
            section,
            cstr(name).as_ptr(),
            M64TYPE_STRING,
            value.as_ptr() as *const c_void,
        )
    })
}

fn check_core(operation: &'static str, code: c_int) -> N64Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(N64Error::Core { operation, code })
    }
}

fn wait_until(
    operation: &'static str,
    timeout: Duration,
    mut predicate: impl FnMut() -> bool,
) -> N64Result<()> {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        if Instant::now() >= deadline {
            return Err(N64Error::Timeout(operation));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn remaining_step_timeout(
    deadline: crate::live::temporal::OperationDeadline,
    operation: &'static str,
) -> N64Result<Duration> {
    deadline
        .remaining_timeout()
        .map(|remaining| remaining.min(OPERATION_DEADLINE))
        .ok_or(N64Error::Timeout(operation))
}

fn validate_observed_frame(
    trigger: FrameGateTrigger,
    observed_before_verified: bool,
    observed_before: u64,
    observed: u64,
) -> N64Result<()> {
    if trigger == FrameGateTrigger::NextFrame
        && observed_before_verified
        && observed != observed_before + 1
    {
        return Err(N64Error::BadState(format!(
            "N64 frame step mismatch: expected {}, observed {observed}",
            observed_before + 1
        )));
    }
    if observed_before_verified && observed <= observed_before {
        return Err(N64Error::BadState(format!(
            "N64 frame boundary did not advance: before {observed_before}, observed {observed}"
        )));
    }
    Ok(())
}

fn error_kind(error: &N64Error) -> &'static str {
    match error {
        N64Error::BadParams(_) => "bad_params",
        N64Error::BadState(_) => "bad_state",
        N64Error::Unsupported(_) => "unsupported",
        N64Error::Core { .. } | N64Error::Timeout(_) | N64Error::GenerationStopped { .. } => {
            "emulator_error"
        }
        N64Error::Dynamic(_) | N64Error::Io(_) => "adapter_error",
    }
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

fn required_num(params: &Value, key: &str) -> N64Result<u64> {
    params
        .get(key)
        .and_then(parse_num)
        .ok_or_else(|| N64Error::BadParams(format!("missing or invalid param: {key}")))
}

fn optional_num(params: &Value, key: &str) -> N64Result<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| N64Error::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

fn rdram_address(params: &Value, length: u64) -> N64Result<u64> {
    let memory_type = params
        .get("memory_type")
        .and_then(Value::as_str)
        .unwrap_or("rdram");
    if memory_type != "rdram" {
        return Err(N64Error::BadParams(format!(
            "unsupported N64 memory_type: {memory_type}"
        )));
    }
    let offset = required_num(params, "address")?;
    if !matches!(offset.checked_add(length), Some(end) if end <= RDRAM_SIZE) {
        return Err(N64Error::BadParams(format!(
            "rdram access out of range: offset {offset:#x}+{length:#x} exceeds {RDRAM_SIZE:#x}"
        )));
    }
    RDRAM_BASE
        .checked_add(offset)
        .ok_or_else(|| N64Error::BadParams("RDRAM address overflow".into()))
}

fn require_r4300(params: &Value) -> N64Result<()> {
    match params.get("cpu").and_then(Value::as_str) {
        None | Some("r4300" | "maincpu") => Ok(()),
        Some(cpu) => Err(N64Error::BadParams(format!(
            "N64 execution control currently supports the R4300 CPU only, got {cpu}"
        ))),
    }
}

#[cfg(test)]
#[path = "n64_adapter_tests.rs"]
mod tests;
