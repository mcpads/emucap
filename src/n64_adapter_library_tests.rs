use super::*;

#[test]
fn failed_symbol_lookup_preserves_the_owned_library() {
    #[cfg(windows)]
    let system_library =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/kernel32.dll");
    #[cfg(windows)]
    let path = system_library.as_path();
    #[cfg(target_os = "macos")]
    let path = Path::new("/usr/lib/libSystem.B.dylib");
    #[cfg(all(unix, not(target_os = "macos")))]
    let path = Path::new("libc.so.6");
    let handle = open_library(path).unwrap();
    unsafe {
        assert!(symbol::<unsafe extern "C" fn()>(handle, b"emucap_missing_symbol\0").is_err());
        #[cfg(windows)]
        let pid = symbol::<unsafe extern "system" fn() -> u32>(handle, b"GetCurrentProcessId\0")
            .unwrap()();
        #[cfg(unix)]
        let pid = symbol::<unsafe extern "C" fn() -> i32>(handle, b"getpid\0").unwrap()() as u32;
        assert_eq!(pid, std::process::id());
        close_library(handle);
    }
}

#[test]
fn missing_library_reports_a_loader_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(open_library(&dir.path().join("absent-library")).is_err());
}
