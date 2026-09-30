//! Dynamic loading of the Mupen64Plus core and its plugins.
use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};

use super::{Api, N64Error, N64Result};

pub(super) unsafe fn load_api(handle: *mut c_void) -> N64Result<Api> {
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

pub(super) unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &'static [u8]) -> N64Result<T> {
    libc::dlerror();
    let pointer = libc::dlsym(handle, cstr(name).as_ptr());
    if pointer.is_null() {
        return Err(N64Error::Dynamic(dl_error()));
    }
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    Ok(std::mem::transmute_copy(&pointer))
}

pub(super) fn open_library(path: &Path) -> N64Result<*mut c_void> {
    let path = path_cstring(path)?;
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        Err(N64Error::Dynamic(dl_error()))
    } else {
        Ok(handle)
    }
}

pub(super) fn dl_error() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "unknown dynamic loader error".into()
    } else {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

pub(super) fn platform_library(root: &Path, stem: &str) -> N64Result<PathBuf> {
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

pub(super) fn path_cstring(path: &Path) -> N64Result<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| N64Error::BadParams(format!("path contains NUL: {}", path.display())))
}

pub(super) fn cstr(bytes: &'static [u8]) -> &'static CStr {
    CStr::from_bytes_with_nul(bytes).expect("static C string")
}
