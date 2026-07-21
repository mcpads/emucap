//! Initial Nintendo 64 adapter backed by a debugger-enabled Mupen64Plus core.

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

const CORE_API_VERSION: c_int = 0x020001;
const M64TYPE_INT: c_int = 1;
const M64TYPE_BOOL: c_int = 3;
const M64CMD_ROM_OPEN: c_int = 1;
const M64CMD_ROM_CLOSE: c_int = 2;
const M64CMD_EXECUTE: c_int = 5;
const M64CMD_STOP: c_int = 6;
const M64CMD_SET_FRAME_CALLBACK: c_int = 15;
const M64P_DBG_RUN_STATE: c_int = 1;
const M64P_DBG_RUNSTATE_PAUSED: c_int = 0;
const M64P_DBG_RUNSTATE_RUNNING: c_int = 2;
const M64P_CPU_PC: c_int = 1;
const M64P_CPU_REG_REG: c_int = 2;
const M64P_CPU_REG_HI: c_int = 3;
const M64P_CPU_REG_LO: c_int = 4;
const M64PLUGIN_RSP: c_int = 1;
const M64PLUGIN_GFX: c_int = 2;

const RDRAM_BASE: u64 = 0x8000_0000;
const RDRAM_SIZE: u64 = 8 * 1024 * 1024;
const MAX_MEMORY_TRANSFER: u64 = 16 * 1024;
const OPERATION_DEADLINE: Duration = Duration::from_secs(3);
const METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "get_state",
    "read_memory",
    "write_memory",
    "pause",
    "resume",
    "step_instructions",
];
const ACTIVE_EXCEPTIONS: &[&str] = &[
    "n64.execution.frame-step-absent",
    "n64.state-read.frozen-only",
    "n64.memory-read.frozen-only",
    "n64.memory-read.bounded",
    "n64.memory-write.frozen-only",
    "n64.execution-step.r4300-only",
    "n64.execution-pause.r4300-only",
    "n64.execution-resume.r4300-only",
];

static DEBUG_READY: AtomicBool = AtomicBool::new(false);
static EXECUTION_TERMINAL: AtomicBool = AtomicBool::new(false);
static CORE_EMU_STATE: AtomicI32 = AtomicI32::new(0);
static UPDATE_COUNT: AtomicU64 = AtomicU64::new(0);
static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static VI_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_PC: AtomicU32 = AtomicU32::new(0);

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
    display: bool,
    frozen: bool,
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
    pub fn prepare(root: &Path, runtime_home: &Path, rom_path: &Path) -> N64Result<Self> {
        reset_observation_state();
        std::fs::create_dir_all(runtime_home.join("config"))?;
        std::fs::create_dir_all(runtime_home.join("data"))?;
        std::fs::create_dir_all(runtime_home.join("screens"))?;

        let core_path = platform_library(root, "libmupen64plus")?;
        let core_handle = open_library(&core_path)?;
        let api = unsafe { load_api(core_handle)? };

        let config = path_cstring(&runtime_home.join("config"))?;
        let data = path_cstring(root)?;
        check_core("CoreStartup", unsafe {
            symbol::<CoreStartup>(core_handle, b"CoreStartup\0")?(
                CORE_API_VERSION,
                config.as_ptr(),
                data.as_ptr(),
                ptr::null_mut(),
                debug_log_callback,
                ptr::null_mut(),
                state_callback,
            )
        })?;
        eprintln!("[mupen64plus-native] core started");

        let mut core_section = ptr::null_mut();
        check_core("ConfigOpenSection(Core)", unsafe {
            (api.config_open_section)(cstr(b"Core\0").as_ptr(), &mut core_section)
        })?;
        set_config_int(&api, core_section, b"R4300Emulator\0", M64TYPE_INT, 0)?;
        set_config_int(&api, core_section, b"EnableDebugger\0", M64TYPE_BOOL, 1)?;
        set_config_int(&api, core_section, b"OnScreenDisplay\0", M64TYPE_BOOL, 0)?;

        let rom = std::fs::read(rom_path)?;
        let rom_len = c_int::try_from(rom.len())
            .map_err(|_| N64Error::BadParams("N64 ROM is too large".into()))?;
        check_core("ROM_OPEN", unsafe {
            (api.core_do_command)(M64CMD_ROM_OPEN, rom_len, rom.as_ptr() as *mut c_void)
        })?;
        eprintln!("[mupen64plus-native] ROM opened");

        let display = display_requested();
        let mut requested_plugins = Vec::with_capacity(2);
        if display {
            requested_plugins.push((M64PLUGIN_GFX, "mupen64plus-video-rice"));
        }
        requested_plugins.push((M64PLUGIN_RSP, "mupen64plus-rsp-hle"));

        let mut plugins = Vec::new();
        for (kind, stem) in requested_plugins {
            let path = platform_library(root, stem)?;
            eprintln!("[mupen64plus-native] loading plugin {}", path.display());
            let handle = open_library(&path)?;
            eprintln!("[mupen64plus-native] loaded plugin {stem}");
            let startup = unsafe { symbol::<PluginStartup>(handle, b"PluginStartup\0")? };
            let shutdown = unsafe { symbol::<PluginShutdown>(handle, b"PluginShutdown\0")? };
            if let Err(error) = check_core("PluginStartup", unsafe {
                startup(core_handle, ptr::null_mut(), debug_log_callback)
            }) {
                unsafe { libc::dlclose(handle) };
                return Err(error);
            }
            if let Err(error) = check_core("CoreAttachPlugin", unsafe {
                (api.core_attach_plugin)(kind, handle)
            }) {
                unsafe {
                    let _ = shutdown();
                    libc::dlclose(handle);
                }
                return Err(error);
            }
            plugins.push(Plugin {
                kind,
                handle,
                shutdown,
            });
            eprintln!("[mupen64plus-native] attached plugin {stem}");
        }

        check_core("DebugSetCallbacks", unsafe {
            (api.debug_set_callbacks)(
                debug_init_callback,
                debug_update_callback,
                debug_vi_callback,
            )
        })?;
        check_core("SET_FRAME_CALLBACK", unsafe {
            (api.core_do_command)(
                M64CMD_SET_FRAME_CALLBACK,
                0,
                frame_callback as *const () as *mut c_void,
            )
        })?;
        eprintln!("[mupen64plus-native] debugger callbacks registered");

        Ok(Self {
            api,
            core_handle,
            plugins,
            rom_path: rom_path.to_path_buf(),
            name: std::env::var("EMUCAP_NAME").ok(),
            session_token: std::env::var("EMUCAP_SESSION_TOKEN").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
            build: std::env::var("EMUCAP_BUILD_HASH").unwrap_or_else(|_| "unknown".into()),
            display,
            frozen: false,
            started: false,
        })
    }

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
            "step_instructions" => self.step_instructions(&request.params),
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
        let mut value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "n64",
            "adapter": "mupen64plus-native",
            "backend": "mupen64plus-core",
            "debugger": true,
            "methods": METHODS,
            "memory_types": ["rdram"],
            "region_sizes": {"rdram": RDRAM_SIZE},
            "breakpoint_kinds": [],
            "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS),
            "capability_notes": {
                "implemented_methods": METHODS,
                "step_units": ["instructions"],
                "step_cpus": ["r4300"],
                "execution_mode": "pure_interpreter",
                "rsp_observation": "not_exposed",
                "frame_source": "vi_callback",
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
        let connected =
            DEBUG_READY.load(Ordering::Acquire) && !EXECUTION_TERMINAL.load(Ordering::Acquire);
        if connected {
            self.frozen = unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) }
                == M64P_DBG_RUNSTATE_PAUSED;
        }
        Ok(json!({
            "connected": connected,
            "system": "n64",
            "adapter": "mupen64plus-native",
            "backend": "mupen64plus-core",
            "debugger": true,
            "state": if self.frozen { "frozen" } else { "running" },
            "frame": VI_COUNT.load(Ordering::Acquire),
            "rendered_frame": FRAME_COUNT.load(Ordering::Acquire),
            "vi_count": VI_COUNT.load(Ordering::Acquire),
            "methods": METHODS,
            "memory_types": ["rdram"],
            "region_sizes": {"rdram": RDRAM_SIZE},
            "breakpoint_kinds": []
            ,"display": self.display
        }))
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
            "sha1": format!("{:x}", hasher.finalize()),
            "size": size,
            "media_type": self.rom_path.extension().and_then(|v| v.to_str()).unwrap_or("").to_ascii_lowercase()
        }))
    }

    fn pause(&mut self, params: &Value) -> N64Result<Value> {
        require_r4300(params)?;
        self.require_connected()?;
        if unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) } != M64P_DBG_RUNSTATE_PAUSED {
            let before = UPDATE_COUNT.load(Ordering::Acquire);
            check_core("DebugSetRunState(paused)", unsafe {
                (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_PAUSED)
            })?;
            wait_until("pause boundary", OPERATION_DEADLINE, || {
                UPDATE_COUNT.load(Ordering::Acquire) > before
                    && unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) }
                        == M64P_DBG_RUNSTATE_PAUSED
            })?;
        }
        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "state": "frozen",
            "frame": VI_COUNT.load(Ordering::Acquire),
            "pc": LAST_PC.load(Ordering::Acquire)
        }))
    }

    fn resume(&mut self, params: &Value) -> N64Result<Value> {
        require_r4300(params)?;
        self.require_connected()?;
        let was_paused =
            unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) } == M64P_DBG_RUNSTATE_PAUSED;
        check_core("DebugSetRunState(running)", unsafe {
            (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_RUNNING)
        })?;
        if was_paused {
            check_core("DebugStep(resume)", unsafe { (self.api.debug_step)() })?;
        }
        self.frozen = false;
        Ok(json!({"status":"completed", "state":"running"}))
    }

    fn step_instructions(&mut self, params: &Value) -> N64Result<Value> {
        require_r4300(params)?;
        self.require_frozen("instruction step")?;
        let count = optional_num(params, "count")?.unwrap_or(1);
        if !(1..=10_000).contains(&count) {
            return Err(N64Error::BadParams(format!(
                "instruction step count must be in 1..=10000, got {count}"
            )));
        }
        let before = UPDATE_COUNT.load(Ordering::Acquire);
        for expected in 1..=count {
            check_core("DebugStep", unsafe { (self.api.debug_step)() })?;
            wait_until("instruction step", OPERATION_DEADLINE, || {
                UPDATE_COUNT.load(Ordering::Acquire) >= before + expected
            })?;
        }
        self.frozen = true;
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "count": count,
            "cpu": "r4300",
            "state": "frozen",
            "pc": LAST_PC.load(Ordering::Acquire),
            "frame": VI_COUNT.load(Ordering::Acquire)
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
            "frame": VI_COUNT.load(Ordering::Acquire),
            "rendered_frame": FRAME_COUNT.load(Ordering::Acquire),
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
        if DEBUG_READY.load(Ordering::Acquire) && !EXECUTION_TERMINAL.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(N64Error::BadState(
                "Mupen64Plus debugger is not connected".into(),
            ))
        }
    }

    fn require_frozen(&self, operation: &str) -> N64Result<()> {
        self.require_connected()?;
        if self.frozen
            && unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) } == M64P_DBG_RUNSTATE_PAUSED
        {
            Ok(())
        } else {
            Err(N64Error::BadState(format!(
                "{operation} requires a frozen N64 machine; call pause first"
            )))
        }
    }
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

impl Drop for Mupen64PlusHost {
    fn drop(&mut self) {
        if self.started && !EXECUTION_TERMINAL.load(Ordering::Acquire) {
            unsafe {
                let _ = (self.api.core_do_command)(M64CMD_STOP, 0, ptr::null_mut());
            }
        }
        if EXECUTION_TERMINAL.load(Ordering::Acquire) {
            for plugin in self.plugins.drain(..).rev() {
                unsafe {
                    let _ = (self.api.core_detach_plugin)(plugin.kind);
                    let _ = (plugin.shutdown)();
                    libc::dlclose(plugin.handle);
                }
            }
            unsafe {
                let _ = (self.api.core_do_command)(M64CMD_ROM_CLOSE, 0, ptr::null_mut());
                let _ = (self.api.core_shutdown)();
                libc::dlclose(self.core_handle);
            }
        }
    }
}

fn reset_observation_state() {
    DEBUG_READY.store(false, Ordering::Release);
    EXECUTION_TERMINAL.store(false, Ordering::Release);
    CORE_EMU_STATE.store(0, Ordering::Release);
    UPDATE_COUNT.store(0, Ordering::Release);
    FRAME_COUNT.store(0, Ordering::Release);
    VI_COUNT.store(0, Ordering::Release);
    LAST_PC.store(0, Ordering::Release);
}

extern "C" fn debug_log_callback(_context: *mut c_void, level: c_int, message: *const c_char) {
    if message.is_null() {
        return;
    }
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    eprintln!("[mupen64plus:{level}] {message}");
}

extern "C" fn state_callback(_context: *mut c_void, parameter: c_int, value: c_int) {
    if parameter == 1 {
        CORE_EMU_STATE.store(value, Ordering::Release);
    }
}

extern "C" fn debug_init_callback() {
    DEBUG_READY.store(true, Ordering::Release);
}

extern "C" fn debug_update_callback(pc: u32) {
    LAST_PC.store(pc, Ordering::Release);
    UPDATE_COUNT.fetch_add(1, Ordering::AcqRel);
}

extern "C" fn debug_vi_callback() {
    VI_COUNT.fetch_add(1, Ordering::AcqRel);
}

extern "C" fn frame_callback(frame: u32) {
    FRAME_COUNT.store(frame as u64, Ordering::Release);
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

fn error_kind(error: &N64Error) -> &'static str {
    match error {
        N64Error::BadParams(_) => "bad_params",
        N64Error::BadState(_) => "bad_state",
        N64Error::Unsupported(_) => "unsupported",
        N64Error::Core { .. } | N64Error::Timeout(_) => "emulator_error",
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
