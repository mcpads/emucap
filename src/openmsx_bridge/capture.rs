use super::{tcl_utf8_value, BridgeResult, OpenMsxBridge, OpenMsxBridgeError, OpenMsxControl};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;

const RASTER_BOUNDARY: &str = r#"format "%u %u %d %s %d %u" [machine_info VDP_frame_count] [machine_info VDP_cycle_in_frame] [machine_info VDP_emucap_raster_frame] [binary encode hex [encoding convertto utf-8 [machine]]] [expr {$emucap_raster_capture && !$deinterlace && !$deflicker && $renderer eq "SDLGL-PP" && $videosource eq "MSX"}] [reg PC]"#;

#[derive(Debug, PartialEq, Eq)]
struct RasterBoundary {
    frame: u64,
    cycle: u64,
    raster: u64,
    machine: String,
    pc: u64,
}

fn parse_boundary(value: &str) -> BridgeResult<RasterBoundary> {
    let fields: Vec<_> = value.split_whitespace().collect();
    if fields.len() != 6 {
        return Err(OpenMsxBridgeError::Protocol(
            "invalid native raster boundary".into(),
        ));
    }
    if fields[4] != "1" {
        return Err(OpenMsxBridgeError::BadState(
            "native raster capture policy is unavailable or changed".into(),
        ));
    }
    let number = |field: &str| {
        field
            .parse::<u64>()
            .map_err(|_| OpenMsxBridgeError::Protocol("invalid native raster counter".into()))
    };
    let frame = number(fields[0])?;
    let cycle = number(fields[1])?;
    let pc = number(fields[5])?;
    if pc > u64::from(u16::MAX) {
        return Err(OpenMsxBridgeError::Protocol("invalid Z80 stop PC".into()));
    }
    if fields[2] == "-1" {
        return Err(OpenMsxBridgeError::BadState(
            "no complete raster since renderer initialization, reset or restore".into(),
        ));
    }
    let raster = number(fields[2])?;
    if raster.checked_add(1) != Some(frame) {
        return Err(OpenMsxBridgeError::BadState(format!(
            "retained raster {raster} is not the latest completed VDP frame before {frame}"
        )));
    }
    let machine = hex::decode(fields[3])
        .ok()
        .and_then(|v| String::from_utf8(v).ok())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| OpenMsxBridgeError::Protocol("invalid raster machine identity".into()))?;
    Ok(RasterBoundary {
        frame,
        cycle,
        raster,
        machine,
        pc,
    })
}

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    fn raster_boundary(&mut self) -> BridgeResult<RasterBoundary> {
        parse_boundary(&self.control.command(RASTER_BOUNDARY)?)
    }
    pub(super) fn screenshot(&mut self) -> BridgeResult<Value> {
        self.require_frozen("screenshot")?;
        self.require_stop_conjunction("screenshot")?;
        let before = self.current_frame()?;
        let boundary = self.raster_boundary()?;
        self.screenshot_sequence += 1;
        let directory = self.runtime_home.join("screenshots");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!(
            "capture-{}-{}.png",
            std::process::id(),
            self.screenshot_sequence
        ));
        let result = (|| {
            let path_var = tcl_utf8_value(&path)?;
            self.control.command(&format!(
                "set emucap_path {path_var}; screenshot -raw -size 320 $emucap_path"
            ))?;
            let png = crate::path_safety::read_bounded_regular_file_no_follow(
                &path,
                crate::live::protocol::MAX_INLINE_SCREENSHOT_BYTES,
            )?;
            if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
                return Err(OpenMsxBridgeError::Protocol(
                    "openMSX screenshot is not a complete PNG".into(),
                ));
            }
            let after = self.current_frame()?;
            let after_boundary = self.raster_boundary()?;
            self.require_stop_conjunction("screenshot")?;
            if after != before || after_boundary != boundary {
                return Err(OpenMsxBridgeError::Emulator(format!(
                    "openMSX screenshot advanced guest time: {before} -> {after}"
                )));
            }
            let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
            let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
            let sha256 = hex::encode(Sha256::digest(&png));
            Ok(json!({
                "png_base64": base64::engine::general_purpose::STANDARD.encode(&png),
                "sha256": sha256,
                "byte_len": png.len(),
                "width": width,
                "height": height,
                "frame": before,
                "frame_domain": "emucap.vdp_frame_boundary_sequence",
                "frame_before": before,
                "frame_after": after,
                "frame_stable": true,
                "state": "frozen",
                "freshness": "latest_completed_frame",
                "capture_boundary": {
                    "cpu": "z80",
                    "pc": boundary.pc,
                    "frame": boundary.frame,
                    "frame_domain": "openmsx.vdp.frame_count",
                    "cycle": boundary.cycle,
                    "cycle_domain": "openmsx.vdp.cycles_since_frame_start",
                },
                "raster_boundary": {
                    "id": format!("{}:{}:{}", self.capture_epoch, boundary.machine, boundary.raster),
                    "epoch": self.capture_epoch,
                    "launch_id": self.launch_id,
                    "machine_id": boundary.machine,
                    "source": "MSX",
                    "frame": boundary.raster,
                    "frame_domain": "openmsx.vdp.frame_count",
                    "completed_at": {"frame": boundary.raster + 1, "cycle": 0},
                    "cycle_domain": "openmsx.vdp.cycles_since_frame_start",
                    "representation": "single_completed_field_scaled_320x240",
                    "image_sha256": sha256,
                },
            }))
        })();
        let _ = fs::remove_file(path);
        result
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
