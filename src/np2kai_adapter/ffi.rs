use super::*;

pub(super) const RETRO_API_VERSION: u32 = 1;
pub(super) const DEBUG_API_VERSION: u32 = 1;
pub(super) const BP_EXEC: u32 = 0;
pub(super) const BP_READ: u32 = 1;
pub(super) const BP_WRITE: u32 = 2;
pub(super) const BP_ACCESS: u32 = 3;
pub(super) const BP_REGISTER: u32 = 4;
pub(super) const BP_RESET: u32 = 5;
pub(super) const RETRO_DEVICE_MOUSE: u32 = 2;
pub(super) const RETRO_DEVICE_KEYBOARD: u32 = 3;
pub(super) const RETRO_ENVIRONMENT_GET_CAN_DUPE: u32 = 3;
pub(super) const RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL: u32 = 8;
pub(super) const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: u32 = 9;
pub(super) const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: u32 = 10;
pub(super) const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: u32 = 11;
pub(super) const RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE: u32 = 13;
pub(super) const RETRO_ENVIRONMENT_GET_VARIABLE: u32 = 15;
pub(super) const RETRO_ENVIRONMENT_SET_VARIABLES: u32 = 16;
pub(super) const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: u32 = 17;
pub(super) const RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME: u32 = 18;
pub(super) const RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY: u32 = 31;
pub(super) const RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO: u32 = 32;
pub(super) const RETRO_ENVIRONMENT_SET_CONTROLLER_INFO: u32 = 35;
pub(super) const RETRO_ENVIRONMENT_SET_GEOMETRY: u32 = 37;
pub(super) const RETRO_ENVIRONMENT_GET_LANGUAGE: u32 = 39;
pub(super) const RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS: u32 = 44;
pub(super) const RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION: u32 = 52;
pub(super) const RETRO_ENVIRONMENT_GET_INPUT_MAX_USERS: u32 = 61;

pub(super) const PIXEL_0RGB1555: u32 = 0;
pub(super) const PIXEL_XRGB8888: u32 = 1;
pub(super) const PIXEL_RGB565: u32 = 2;

#[repr(C)]
pub(super) struct RetroVariable {
    pub(super) key: *const c_char,
    pub(super) value: *const c_char,
}

#[repr(C)]
pub(super) struct RetroGameInfo {
    pub(super) path: *const c_char,
    pub(super) data: *const c_void,
    pub(super) size: usize,
    pub(super) meta: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct RetroGameGeometry {
    pub(super) base_width: u32,
    pub(super) base_height: u32,
    pub(super) max_width: u32,
    pub(super) max_height: u32,
    pub(super) aspect_ratio: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct RetroSystemTiming {
    pub(super) fps: f64,
    pub(super) sample_rate: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct RetroSystemAvInfo {
    pub(super) geometry: RetroGameGeometry,
    pub(super) timing: RetroSystemTiming,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct NativeRegisters {
    pub(super) eax: u32,
    pub(super) ecx: u32,
    pub(super) edx: u32,
    pub(super) ebx: u32,
    pub(super) esp: u32,
    pub(super) ebp: u32,
    pub(super) esi: u32,
    pub(super) edi: u32,
    pub(super) eip: u32,
    pub(super) eflags: u32,
    pub(super) cs: u16,
    pub(super) ss: u16,
    pub(super) ds: u16,
    pub(super) es: u16,
    pub(super) fs: u16,
    pub(super) gs: u16,
    pub(super) cs_base: u32,
    pub(super) cr0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct NativeBreakpoint {
    pub(super) id: u64,
    pub(super) kind: u32,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) pause_on_hit: u32,
    pub(super) has_pc_min: u32,
    pub(super) pc_min: u32,
    pub(super) has_pc_max: u32,
    pub(super) pc_max: u32,
    pub(super) has_value: u32,
    pub(super) value: u32,
    pub(super) value_mask: u32,
    pub(super) value_len: u32,
    pub(super) register_index: u32,
    pub(super) register_min: u32,
    pub(super) register_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct NativeEvent {
    pub(super) sequence: u64,
    pub(super) breakpoint_id: u64,
    pub(super) kind: u32,
    pub(super) address: u32,
    pub(super) size: u32,
    pub(super) value: u32,
    pub(super) paused: u32,
    pub(super) registers: NativeRegisters,
}

impl Default for NativeEvent {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct NativeTrace {
    pub(super) sequence: u64,
    pub(super) registers: NativeRegisters,
    pub(super) text: [c_char; 256],
}

impl Default for NativeTrace {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

pub(super) type EnvironmentCallback = extern "C" fn(u32, *mut c_void) -> bool;
pub(super) type VideoCallback = extern "C" fn(*const c_void, u32, u32, usize);
pub(super) type AudioSampleCallback = extern "C" fn(i16, i16);
pub(super) type AudioBatchCallback = extern "C" fn(*const i16, usize) -> usize;
pub(super) type InputPollCallback = extern "C" fn();
pub(super) type InputStateCallback = extern "C" fn(u32, u32, u32, u32) -> i16;

type RetroSetEnvironment = unsafe extern "C" fn(EnvironmentCallback);
type RetroSetVideoRefresh = unsafe extern "C" fn(VideoCallback);
type RetroSetAudioSample = unsafe extern "C" fn(AudioSampleCallback);
type RetroSetAudioSampleBatch = unsafe extern "C" fn(AudioBatchCallback);
type RetroSetInputPoll = unsafe extern "C" fn(InputPollCallback);
type RetroSetInputState = unsafe extern "C" fn(InputStateCallback);
type RetroInit = unsafe extern "C" fn();
type RetroDeinit = unsafe extern "C" fn();
type RetroApiVersion = unsafe extern "C" fn() -> u32;
type RetroLoadGame = unsafe extern "C" fn(*const RetroGameInfo) -> bool;
type RetroUnloadGame = unsafe extern "C" fn();
type RetroRun = unsafe extern "C" fn();
type RetroReset = unsafe extern "C" fn();
type RetroGetSystemAvInfo = unsafe extern "C" fn(*mut RetroSystemAvInfo);
type RetroSerializeSize = unsafe extern "C" fn() -> usize;
type RetroSerialize = unsafe extern "C" fn(*mut c_void, usize) -> bool;
type RetroUnserialize = unsafe extern "C" fn(*const c_void, usize) -> bool;
type DebugApiVersion = unsafe extern "C" fn() -> u32;
type DebugReadMemory = unsafe extern "C" fn(u32, *mut u8, usize) -> i32;
type DebugWriteMemory = unsafe extern "C" fn(u32, *const u8, usize) -> i32;
type DebugGetRegisters = unsafe extern "C" fn(*mut NativeRegisters) -> i32;
type DebugStepInstruction = unsafe extern "C" fn() -> i32;
type DebugDisassemble =
    unsafe extern "C" fn(u32, *mut c_char, usize, *mut u32, *mut u8, *mut usize) -> i32;
type DebugSetBreakpoint = unsafe extern "C" fn(*const NativeBreakpoint) -> i32;
type DebugClearBreakpoint = unsafe extern "C" fn(u64) -> i32;
type DebugClearAllBreakpoints = unsafe extern "C" fn();
type DebugPollEvent = unsafe extern "C" fn(*mut NativeEvent) -> i32;
type DebugTakeDropped = unsafe extern "C" fn() -> u64;
type DebugSetTrace = unsafe extern "C" fn(i32);
type DebugPollTrace = unsafe extern "C" fn(*mut NativeTrace) -> i32;
type DebugStopRequested = unsafe extern "C" fn() -> i32;
type DebugClearStop = unsafe extern "C" fn();
type DebugChangeHdd = unsafe extern "C" fn(*const c_char) -> i32;
type DebugCurrentHdd = unsafe extern "C" fn() -> *const c_char;

pub(super) struct CoreApi {
    pub(super) set_environment: RetroSetEnvironment,
    pub(super) set_video_refresh: RetroSetVideoRefresh,
    pub(super) set_audio_sample: RetroSetAudioSample,
    pub(super) set_audio_sample_batch: RetroSetAudioSampleBatch,
    pub(super) set_input_poll: RetroSetInputPoll,
    pub(super) set_input_state: RetroSetInputState,
    pub(super) init: RetroInit,
    pub(super) deinit: RetroDeinit,
    pub(super) api_version: RetroApiVersion,
    pub(super) load_game: RetroLoadGame,
    pub(super) unload_game: RetroUnloadGame,
    pub(super) run: RetroRun,
    pub(super) reset: RetroReset,
    pub(super) get_system_av_info: RetroGetSystemAvInfo,
    pub(super) serialize_size: RetroSerializeSize,
    pub(super) serialize: RetroSerialize,
    pub(super) unserialize: RetroUnserialize,
    pub(super) debug_read_memory: DebugReadMemory,
    pub(super) debug_write_memory: DebugWriteMemory,
    pub(super) debug_get_registers: DebugGetRegisters,
    pub(super) debug_step_instruction: DebugStepInstruction,
    pub(super) debug_disassemble: DebugDisassemble,
    pub(super) debug_set_breakpoint: DebugSetBreakpoint,
    pub(super) debug_clear_breakpoint: DebugClearBreakpoint,
    pub(super) debug_clear_all_breakpoints: DebugClearAllBreakpoints,
    pub(super) debug_poll_event: DebugPollEvent,
    pub(super) debug_take_dropped_events: DebugTakeDropped,
    pub(super) debug_set_trace: DebugSetTrace,
    pub(super) debug_poll_trace: DebugPollTrace,
    pub(super) debug_take_dropped_trace: DebugTakeDropped,
    pub(super) debug_stop_requested: DebugStopRequested,
    pub(super) debug_clear_stop: DebugClearStop,
    pub(super) debug_change_hdd: DebugChangeHdd,
    pub(super) debug_current_hdd: DebugCurrentHdd,
    pub(super) _library: Library,
}

#[derive(Default)]
pub(super) struct CapturedFrame {
    pub(super) bytes: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pitch: usize,
    pub(super) pixel_format: u32,
    pub(super) callback_count: u64,
}

pub(super) struct CallbackState {
    pub(super) system_dir: CString,
    pub(super) save_dir: CString,
    pub(super) options: HashMap<String, CString>,
    pub(super) keys: BTreeSet<u32>,
    pub(super) mouse_buttons: u8,
    pub(super) mouse_dx: i16,
    pub(super) mouse_dy: i16,
    pub(super) frame: CapturedFrame,
    pub(super) av_info: RetroSystemAvInfo,
}

static CALLBACK_STATE: OnceLock<Mutex<Option<CallbackState>>> = OnceLock::new();

pub(super) fn callback_slot() -> &'static Mutex<Option<CallbackState>> {
    CALLBACK_STATE.get_or_init(|| Mutex::new(None))
}

impl CoreApi {
    pub(super) unsafe fn load(path: &Path) -> Np2kaiResult<Self> {
        let library =
            Library::new(path).map_err(|error| Np2kaiError::Dynamic(error.to_string()))?;
        macro_rules! symbol {
            ($name:literal, $kind:ty) => {{
                *library
                    .get::<$kind>(concat!($name, "\0").as_bytes())
                    .map_err(|error| Np2kaiError::Dynamic(error.to_string()))?
            }};
        }
        let debug_api_version = symbol!("emucap_np2_debug_api_version", DebugApiVersion);
        if debug_api_version() != DEBUG_API_VERSION {
            return Err(Np2kaiError::Core(format!(
                "unsupported NP2kai debug API version: expected {DEBUG_API_VERSION}"
            )));
        }
        Ok(Self {
            set_environment: symbol!("retro_set_environment", RetroSetEnvironment),
            set_video_refresh: symbol!("retro_set_video_refresh", RetroSetVideoRefresh),
            set_audio_sample: symbol!("retro_set_audio_sample", RetroSetAudioSample),
            set_audio_sample_batch: symbol!(
                "retro_set_audio_sample_batch",
                RetroSetAudioSampleBatch
            ),
            set_input_poll: symbol!("retro_set_input_poll", RetroSetInputPoll),
            set_input_state: symbol!("retro_set_input_state", RetroSetInputState),
            init: symbol!("retro_init", RetroInit),
            deinit: symbol!("retro_deinit", RetroDeinit),
            api_version: symbol!("retro_api_version", RetroApiVersion),
            load_game: symbol!("retro_load_game", RetroLoadGame),
            unload_game: symbol!("retro_unload_game", RetroUnloadGame),
            run: symbol!("retro_run", RetroRun),
            reset: symbol!("retro_reset", RetroReset),
            get_system_av_info: symbol!("retro_get_system_av_info", RetroGetSystemAvInfo),
            serialize_size: symbol!("retro_serialize_size", RetroSerializeSize),
            serialize: symbol!("retro_serialize", RetroSerialize),
            unserialize: symbol!("retro_unserialize", RetroUnserialize),
            debug_read_memory: symbol!("emucap_np2_read_memory", DebugReadMemory),
            debug_write_memory: symbol!("emucap_np2_write_memory", DebugWriteMemory),
            debug_get_registers: symbol!("emucap_np2_get_regs", DebugGetRegisters),
            debug_step_instruction: symbol!("emucap_np2_step_instruction", DebugStepInstruction),
            debug_disassemble: symbol!("emucap_np2_disassemble", DebugDisassemble),
            debug_set_breakpoint: symbol!("emucap_np2_set_breakpoint", DebugSetBreakpoint),
            debug_clear_breakpoint: symbol!("emucap_np2_clear_breakpoint", DebugClearBreakpoint),
            debug_clear_all_breakpoints: symbol!(
                "emucap_np2_clear_all_breakpoints",
                DebugClearAllBreakpoints
            ),
            debug_poll_event: symbol!("emucap_np2_poll_event", DebugPollEvent),
            debug_take_dropped_events: symbol!("emucap_np2_take_dropped_events", DebugTakeDropped),
            debug_set_trace: symbol!("emucap_np2_set_trace", DebugSetTrace),
            debug_poll_trace: symbol!("emucap_np2_poll_trace", DebugPollTrace),
            debug_take_dropped_trace: symbol!("emucap_np2_take_dropped_trace", DebugTakeDropped),
            debug_stop_requested: symbol!("emucap_np2_stop_requested", DebugStopRequested),
            debug_clear_stop: symbol!("emucap_np2_clear_stop", DebugClearStop),
            debug_change_hdd: symbol!("emucap_np2_change_hdd", DebugChangeHdd),
            debug_current_hdd: symbol!("emucap_np2_current_hdd", DebugCurrentHdd),
            _library: library,
        })
    }
}

pub(super) extern "C" fn environment_callback(command: u32, data: *mut c_void) -> bool {
    if data.is_null() {
        return false;
    }
    let mut slot = callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner());
    let Some(state) = slot.as_mut() else {
        return false;
    };
    unsafe {
        match command {
            RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY => {
                *data.cast::<*const c_char>() = state.system_dir.as_ptr();
                true
            }
            RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => {
                *data.cast::<*const c_char>() = state.save_dir.as_ptr();
                true
            }
            RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => false,
            RETRO_ENVIRONMENT_SET_VARIABLES => {
                let mut variable = data.cast::<RetroVariable>();
                while !(*variable).key.is_null() {
                    let key = CStr::from_ptr((*variable).key)
                        .to_string_lossy()
                        .into_owned();
                    if !state.options.contains_key(&key) && !(*variable).value.is_null() {
                        let definition = CStr::from_ptr((*variable).value).to_string_lossy();
                        if let Some((_, values)) = definition.split_once("; ") {
                            if let Some(default) = values.split('|').next() {
                                if let Ok(default) = CString::new(default) {
                                    state.options.insert(key, default);
                                }
                            }
                        }
                    }
                    variable = variable.add(1);
                }
                true
            }
            RETRO_ENVIRONMENT_GET_VARIABLE => {
                let variable = &mut *data.cast::<RetroVariable>();
                if variable.key.is_null() {
                    return false;
                }
                let key = CStr::from_ptr(variable.key).to_string_lossy();
                if let Some(value) = state.options.get(key.as_ref()) {
                    variable.value = value.as_ptr();
                    true
                } else {
                    variable.value = ptr::null();
                    false
                }
            }
            RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
                *data.cast::<bool>() = false;
                true
            }
            RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
                let format = *data.cast::<u32>();
                if matches!(format, PIXEL_0RGB1555 | PIXEL_XRGB8888 | PIXEL_RGB565) {
                    state.frame.pixel_format = format;
                    true
                } else {
                    false
                }
            }
            RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO => {
                state.av_info = *data.cast::<RetroSystemAvInfo>();
                true
            }
            RETRO_ENVIRONMENT_SET_GEOMETRY => {
                state.av_info.geometry = *data.cast::<RetroGameGeometry>();
                true
            }
            RETRO_ENVIRONMENT_GET_CAN_DUPE => {
                *data.cast::<bool>() = true;
                true
            }
            RETRO_ENVIRONMENT_GET_LANGUAGE => {
                *data.cast::<u32>() = 0;
                true
            }
            RETRO_ENVIRONMENT_GET_INPUT_MAX_USERS => {
                *data.cast::<u32>() = 1;
                true
            }
            RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE => true,
            RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS
            | RETRO_ENVIRONMENT_SET_CONTROLLER_INFO
            | RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME
            | RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL
            | RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS => true,
            _ => false,
        }
    }
}

pub(super) extern "C" fn video_callback(
    data: *const c_void,
    width: u32,
    height: u32,
    pitch: usize,
) {
    if data.is_null() || width == 0 || height == 0 || pitch == 0 {
        return;
    }
    let Some(byte_len) = pitch.checked_mul(height as usize) else {
        return;
    };
    if byte_len > 64 * 1024 * 1024 {
        return;
    }
    let mut slot = callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner());
    let Some(state) = slot.as_mut() else { return };
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), byte_len) };
    state.frame.bytes.clear();
    state.frame.bytes.extend_from_slice(bytes);
    state.frame.width = width;
    state.frame.height = height;
    state.frame.pitch = pitch;
    state.frame.callback_count = state.frame.callback_count.saturating_add(1);
}

pub(super) extern "C" fn audio_sample_callback(_left: i16, _right: i16) {}
pub(super) extern "C" fn audio_batch_callback(_data: *const i16, frames: usize) -> usize {
    frames
}
pub(super) extern "C" fn input_poll_callback() {}

pub(super) extern "C" fn input_state_callback(port: u32, device: u32, _index: u32, id: u32) -> i16 {
    if port != 0 {
        return 0;
    }
    let slot = callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner());
    let Some(state) = slot.as_ref() else { return 0 };
    match device {
        RETRO_DEVICE_KEYBOARD => i16::from(state.keys.contains(&id)),
        RETRO_DEVICE_MOUSE => match id {
            0 => state.mouse_dx,
            1 => state.mouse_dy,
            2 => i16::from(state.mouse_buttons & 1 != 0),
            3 => i16::from(state.mouse_buttons & 2 != 0),
            4 => i16::from(state.mouse_buttons & 4 != 0),
            _ => 0,
        },
        _ => 0,
    }
}

pub(super) fn clear_callback_state() {
    *callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner()) = None;
}

pub(super) fn callback_count() -> Np2kaiResult<u64> {
    callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner())
        .as_ref()
        .map(|state| state.frame.callback_count)
        .ok_or_else(|| Np2kaiError::BadState("NP2kai callback state is unavailable".into()))
}

pub(super) fn set_controls(buttons: &BTreeSet<String>) {
    if let Some(state) = callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner())
        .as_mut()
    {
        state.keys = key_ids(buttons);
        state.mouse_buttons = mouse_button_mask(buttons);
    }
}

pub(super) fn set_pointer_delta(dx: i16, dy: i16) {
    if let Some(state) = callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner())
        .as_mut()
    {
        state.mouse_dx = dx;
        state.mouse_dy = dy;
    }
}

pub(super) fn captured_png() -> Np2kaiResult<(Vec<u8>, u32, u32)> {
    let slot = callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner());
    let state = slot
        .as_ref()
        .ok_or_else(|| Np2kaiError::BadState("NP2kai callback state is unavailable".into()))?;
    let frame = &state.frame;
    if frame.bytes.is_empty() || frame.width == 0 || frame.height == 0 {
        return Err(Np2kaiError::BadState(
            "NP2kai has not produced a capturable video frame".into(),
        ));
    }
    let pixel_size = if frame.pixel_format == PIXEL_XRGB8888 {
        4
    } else {
        2
    };
    let row_bytes = frame.width as usize * pixel_size;
    if row_bytes > frame.pitch
        || frame.pitch.checked_mul(frame.height as usize) != Some(frame.bytes.len())
    {
        return Err(Np2kaiError::Core(
            "captured video geometry does not match its buffer".into(),
        ));
    }
    let mut rgb = Vec::with_capacity(frame.width as usize * frame.height as usize * 3);
    for y in 0..frame.height as usize {
        let row = &frame.bytes[y * frame.pitch..y * frame.pitch + row_bytes];
        for x in 0..frame.width as usize {
            let (r, g, b) = match frame.pixel_format {
                PIXEL_RGB565 => {
                    let value = u16::from_ne_bytes([row[x * 2], row[x * 2 + 1]]);
                    (
                        scale5((value >> 11) & 31),
                        scale6((value >> 5) & 63),
                        scale5(value & 31),
                    )
                }
                PIXEL_XRGB8888 => {
                    let value = u32::from_ne_bytes([
                        row[x * 4],
                        row[x * 4 + 1],
                        row[x * 4 + 2],
                        row[x * 4 + 3],
                    ]);
                    (
                        ((value >> 16) & 255) as u8,
                        ((value >> 8) & 255) as u8,
                        (value & 255) as u8,
                    )
                }
                PIXEL_0RGB1555 => {
                    let value = u16::from_ne_bytes([row[x * 2], row[x * 2 + 1]]);
                    (
                        scale5((value >> 10) & 31),
                        scale5((value >> 5) & 31),
                        scale5(value & 31),
                    )
                }
                other => {
                    return Err(Np2kaiError::Core(format!(
                        "unsupported captured pixel format: {other}"
                    )))
                }
            };
            rgb.extend([r, g, b]);
        }
    }
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| Np2kaiError::Core(format!("write PNG header: {error}")))?;
        writer
            .write_image_data(&rgb)
            .map_err(|error| Np2kaiError::Core(format!("write PNG data: {error}")))?;
    }
    Ok((output, frame.width, frame.height))
}

fn scale5(value: u16) -> u8 {
    ((value as u32 * 255) / 31) as u8
}
fn scale6(value: u16) -> u8 {
    ((value as u32 * 255) / 63) as u8
}
