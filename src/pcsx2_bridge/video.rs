use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use super::*;

static CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl<T: PineTransport> Pcsx2Bridge<T> {
    pub(super) fn screenshot(&mut self) -> BridgeResult<Value> {
        let directory = std::env::var_os("EMUCAP_PCSX2_CAPTURE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&directory)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let sequence = CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "emucap-pcsx2-{}-{nonce}-{sequence}.png",
            std::process::id()
        ));
        let raw_path = path.to_str().ok_or_else(|| {
            Pcsx2BridgeError::BadParams("screenshot path must be valid UTF-8".into())
        })?;
        if !path.is_absolute() || raw_path.len() > 4096 {
            return Err(Pcsx2BridgeError::BadParams(
                "screenshot path must be absolute and at most 4096 bytes".into(),
            ));
        }

        let result = (|| {
            let mut body = Vec::with_capacity(4 + raw_path.len());
            body.extend_from_slice(&(raw_path.len() as u32).to_le_bytes());
            body.extend_from_slice(raw_path.as_bytes());
            let payload = self.command(MSG_EMUCAP_SCREENSHOT, &body)?;
            let mut cursor = SliceCursor::new(&payload);
            let width = cursor.u32()?;
            let height = cursor.u32()?;
            let frame_before = cursor.u64()?;
            let frame_after = cursor.u64()?;
            if !cursor.is_empty() || width == 0 || height == 0 || frame_before != frame_after {
                return Err(Pcsx2BridgeError::Protocol(
                    "invalid or unstable PCSX2 screenshot reply".into(),
                ));
            }
            let png = std::fs::read(&path)?;
            if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(Pcsx2BridgeError::Emulator(
                    "PCSX2 screenshot file was not a PNG".into(),
                ));
            }
            let sha256 = format!("{:x}", Sha256::digest(&png));
            Ok(json!({
                "png_base64": base64::engine::general_purpose::STANDARD.encode(&png),
                "format": "png",
                "width": width,
                "height": height,
                "sha256": sha256,
                "byte_len": png.len(),
                "frame_before": frame_before,
                "frame_after": frame_after,
                "frame_stable": true,
                "freshness": "current",
                "frame_binding": "current",
                "generation": self.launch_id.as_deref().unwrap_or("attached-session"),
                "status": "completed",
            }))
        })();
        let _ = std::fs::remove_file(path);
        result
    }
}
