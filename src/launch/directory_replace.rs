//! Publication of a prepared private runtime directory.
use std::path::Path;

pub(super) fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)
    }
    #[cfg(windows)]
    {
        // An exited Windows process can leave its image directory briefly locked while
        // native teardown completes. Retry only publication, without relaunching or
        // altering the existing generation. A persistent lock remains an error.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match std::fs::rename(from, to) {
                Err(error)
                    if matches!(error.raw_os_error(), Some(5 | 32 | 33))
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }
}
