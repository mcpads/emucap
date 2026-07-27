//! Readiness, callbacks, and owned shutdown for the N64 frontend.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdapterReadiness {
    WaitingForDebugger,
    WaitingForInitialRelease,
    WaitingForFirstRenderedFrame,
    Ready,
    Terminated,
}

impl AdapterReadiness {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::WaitingForDebugger => "waiting_for_debugger",
            Self::WaitingForInitialRelease => "waiting_for_initial_release",
            Self::WaitingForFirstRenderedFrame => "waiting_for_first_rendered_frame",
            Self::Ready => "ready",
            Self::Terminated => "terminated",
        }
    }

    pub(super) fn is_ready(self) -> bool {
        self == Self::Ready
    }
}

pub(super) fn readiness_for(
    display: bool,
    debugger_ready: bool,
    initial_released: bool,
    execution_terminal: bool,
    rendered_frame_seen: bool,
) -> AdapterReadiness {
    if execution_terminal {
        AdapterReadiness::Terminated
    } else if !debugger_ready {
        AdapterReadiness::WaitingForDebugger
    } else if !initial_released {
        AdapterReadiness::WaitingForInitialRelease
    } else if display && !rendered_frame_seen {
        AdapterReadiness::WaitingForFirstRenderedFrame
    } else {
        AdapterReadiness::Ready
    }
}

pub(super) fn current_readiness(display: bool) -> AdapterReadiness {
    readiness_for(
        display,
        DEBUG_READY.load(Ordering::Acquire),
        INITIAL_RELEASED.load(Ordering::Acquire),
        EXECUTION_TERMINAL.load(Ordering::Acquire),
        FRAME_SEEN.load(Ordering::Acquire),
    )
}

impl Drop for Mupen64PlusHost {
    fn drop(&mut self) {
        frame::shutdown_frame_gate();
        if !self.started {
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
            return;
        }
        for button in self.held_buttons.iter() {
            if let Some(key) = control::input_key(button) {
                unsafe {
                    let _ = (self.api.core_do_command)(M64CMD_SEND_SDL_KEYUP, key, ptr::null_mut());
                }
            }
        }
        self.held_buttons.clear();
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

pub(super) fn reset_observation_state() {
    reset_frame_gate();
    DEBUG_READY.store(false, Ordering::Release);
    INITIAL_RELEASED.store(false, Ordering::Release);
    EXECUTION_TERMINAL.store(false, Ordering::Release);
    UPDATE_COUNT.store(0, Ordering::Release);
    FRAME_COUNT.store(0, Ordering::Release);
    FRAME_SEEN.store(false, Ordering::Release);
    VI_COUNT.store(0, Ordering::Release);
    LAST_PC.store(0, Ordering::Release);
    STATE_SAVE_RESULT.store(-1, Ordering::Release);
    STATE_LOAD_RESULT.store(-1, Ordering::Release);
    SCREENSHOT_RESULT.store(-1, Ordering::Release);
}

pub(super) extern "C" fn debug_log_callback(
    _context: *mut c_void,
    level: c_int,
    message: *const c_char,
) {
    if message.is_null() {
        return;
    }
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    eprintln!("[mupen64plus:{level}] {message}");
}

pub(super) extern "C" fn state_callback(_context: *mut c_void, parameter: c_int, value: c_int) {
    match parameter {
        M64CORE_STATE_LOADCOMPLETE => STATE_LOAD_RESULT.store(value, Ordering::Release),
        M64CORE_STATE_SAVECOMPLETE => STATE_SAVE_RESULT.store(value, Ordering::Release),
        M64CORE_SCREENSHOT_CAPTURED => SCREENSHOT_RESULT.store(value, Ordering::Release),
        _ => {}
    }
}

pub(super) extern "C" fn debug_init_callback() {
    DEBUG_READY.store(true, Ordering::Release);
}

pub(super) extern "C" fn debug_update_callback(pc: u32) {
    LAST_PC.store(pc, Ordering::Release);
    let update = UPDATE_COUNT.fetch_add(1, Ordering::AcqRel) + 1;
    frame::notify_debug_update(update);
}

pub(super) extern "C" fn debug_vi_callback() {
    VI_COUNT.fetch_add(1, Ordering::AcqRel);
}
