use super::*;
use base64::Engine;
use std::fs::OpenOptions;
use std::io::Read;
use std::time::Instant;

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn screenshot(&mut self) -> XemuResult<Value> {
        let request_id = self.next_screenshot_id;
        self.next_screenshot_id = self.next_screenshot_id.checked_add(1).ok_or_else(|| {
            XemuBridgeError::Emulator("screenshot request id space exhausted".into())
        })?;
        self.qmp.execute(
            "xemu-emucap-request-screenshot",
            Some(json!({"request-id":request_id})),
        )?;
        let deadline = Instant::now() + SCREENSHOT_TIMEOUT;
        loop {
            let status = self.qmp.execute(
                "xemu-emucap-screenshot-status",
                Some(json!({"request-id":request_id})),
            )?;
            match status.get("state").and_then(Value::as_str) {
                Some("pending") => {
                    if Instant::now() >= deadline {
                        return Err(XemuBridgeError::Emulator(format!(
                            "Xbox screenshot request {request_id} did not complete within {} ms",
                            SCREENSHOT_TIMEOUT.as_millis()
                        )));
                    }
                    std::thread::sleep(SCREENSHOT_POLL_INTERVAL);
                }
                Some("failed") => {
                    return Err(XemuBridgeError::Emulator(format!(
                        "Xbox screenshot failed: {}",
                        status
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown failure")
                    )))
                }
                Some("completed") => {
                    let expected = format!("emucap-{request_id:020}.png");
                    let filename =
                        status
                            .get("filename")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                XemuBridgeError::Emulator(
                                    "completed screenshot omitted filename".into(),
                                )
                            })?;
                    if filename != expected {
                        return Err(XemuBridgeError::Emulator(format!(
                            "unexpected screenshot filename: expected {expected}, got {filename}"
                        )));
                    }
                    let path = self.screen_root.join(filename);
                    let bytes = read_screenshot_file(&path)?;
                    let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
                    let reader = decoder.read_info().map_err(|error| {
                        XemuBridgeError::Emulator(format!("invalid screenshot PNG: {error}"))
                    })?;
                    let info = reader.info();
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    let _ = std::fs::remove_file(&path);
                    return Ok(json!({
                        "png_base64":encoded, "format":"png", "width":info.width,
                        "height":info.height, "frame":self.extension_status()?["frame-boundary"],
                    }));
                }
                Some(other) => {
                    return Err(XemuBridgeError::Emulator(format!(
                        "unknown Xbox screenshot state: {other}"
                    )))
                }
                None => {
                    return Err(XemuBridgeError::Emulator(
                        "Xbox screenshot status omitted state".into(),
                    ))
                }
            }
        }
    }
}

fn read_screenshot_file(path: &std::path::Path) -> XemuResult<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(XemuBridgeError::Emulator(format!(
            "screenshot output is not a regular non-symlink file: {}",
            path.display()
        )));
    }
    if metadata.len() > crate::live::protocol::MAX_INLINE_SCREENSHOT_BYTES {
        return Err(XemuBridgeError::Emulator(format!(
            "screenshot exceeds inline response limit: {} bytes",
            metadata.len()
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(XemuBridgeError::Emulator(
            "screenshot changed while reading or was not a PNG".into(),
        ));
    }
    Ok(bytes)
}
