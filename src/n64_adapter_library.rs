//! Dynamic loading of the Mupen64Plus core and its plugins.
use std::ffi::{c_void, CStr, CString};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use libloading::os::unix::Library as NativeLibrary;
#[cfg(windows)]
use libloading::os::windows::Library as NativeLibrary;

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
        debug_mem_read_rdram: symbol(handle, b"DebugMemReadRdram\0")?,
        debug_mem_write8: symbol(handle, b"DebugMemWrite8\0")?,
        debug_breakpoint_command: symbol(handle, b"DebugBreakpointCommand\0")?,
        debug_breakpoint_lookup: symbol(handle, b"DebugBreakpointLookup\0")?,
        debug_breakpoint_consume: symbol(handle, b"DebugBreakpointConsume\0")?,
        debug_decode_op: symbol(handle, b"DebugDecodeOp\0")?,
        debug_frame_resume: symbol(handle, b"DebugFrameResume\0")?,
        core_emucap_pacing: symbol(handle, b"CoreEmucapPacing\0")?,
    })
}

pub(super) unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &'static [u8]) -> N64Result<T> {
    // PreparationGuard or the host owns this handle; symbol lookup borrows it.
    let library = ManuallyDrop::new(NativeLibrary::from_raw(handle as _));
    library
        .get::<T>(name)
        .map(|symbol| *symbol)
        .map_err(|error| N64Error::Dynamic(error.to_string()))
}

pub(super) fn open_library(path: &Path) -> N64Result<*mut c_void> {
    #[cfg(unix)]
    let library = unsafe { NativeLibrary::open(Some(path), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    #[cfg(windows)]
    let library = unsafe {
        use libloading::os::windows::{
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        };
        NativeLibrary::load_with_flags(
            // LoadLibrary requires an absolute path for DLL_LOAD_DIR. Keep the
            // Win32 spelling: canonicalize adds a verbatim prefix which can
            // bypass loaded system-module identity and fail with error 487.
            std::path::absolute(path)?,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    };
    let library =
        library.map_err(|error| N64Error::Dynamic(format!("{}: {error:?}", path.display())))?;
    #[cfg(unix)]
    let handle = library.into_raw();
    #[cfg(windows)]
    let handle = library.into_raw() as *mut c_void;
    Ok(handle)
}

pub(super) unsafe fn close_library(handle: *mut c_void) {
    drop(NativeLibrary::from_raw(handle as _));
}

pub(super) fn platform_library(root: &Path, stem: &str) -> N64Result<PathBuf> {
    crate::launch::mupen64plus::library_path(root, stem).ok_or_else(|| {
        N64Error::BadParams(format!(
            "Mupen64Plus library not found under {}: {stem}",
            root.display()
        ))
    })
}

pub(super) fn path_cstring(path: &Path) -> N64Result<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| N64Error::BadParams(format!("path contains NUL: {}", path.display())))
}

pub(super) fn cstr(bytes: &'static [u8]) -> &'static CStr {
    CStr::from_bytes_with_nul(bytes).expect("static C string")
}

#[cfg(test)]
#[path = "n64_adapter_library_tests.rs"]
mod tests;
