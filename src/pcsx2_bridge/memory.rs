use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::*;

const MAX_PATTERN_BYTES: usize = 0x1_0000;
const MAX_PATTERN_MATCHES: u64 = 4096;

impl<T: PineTransport> Pcsx2Bridge<T> {
    pub(super) fn find_pattern(&mut self, params: &Value) -> BridgeResult<Value> {
        require_ee_memory_type(params)?;
        let pattern = hex::decode(required_str(params, "hex")?)
            .map_err(|_| Pcsx2BridgeError::BadParams("hex decode failed".into()))?;
        if pattern.is_empty() || pattern.len() > MAX_PATTERN_BYTES {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "find_pattern hex must contain 1..={MAX_PATTERN_BYTES:#x} bytes"
            )));
        }

        let start = optional_num(params, "start")?.unwrap_or(0);
        if start > PCSX2_EE_RAM_SIZE {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "find_pattern start {start:#x} exceeds EE RAM size {PCSX2_EE_RAM_SIZE:#x}"
            )));
        }
        let available = PCSX2_EE_RAM_SIZE - start;
        let requested = optional_num(params, "length")?.unwrap_or(available);
        let length = requested.min(available);
        let truncated_scan = requested > available;
        let max_matches = optional_num(params, "max_matches")?
            .unwrap_or(256)
            .clamp(1, MAX_PATTERN_MATCHES) as usize;
        let align = optional_num(params, "align")?.unwrap_or(1).max(1) as usize;

        let was_running = self.emulator_state()? == "running";
        if was_running {
            self.command(MSG_EMUCAP_PAUSE, &[])?;
        }
        let scan = self.read_ee_range(start, length as usize);
        let cleanup = if was_running {
            self.command(MSG_EMUCAP_RESUME, &[])
        } else {
            Ok(Vec::new())
        };
        let bytes = finish_with_cleanup(scan, cleanup, "find_pattern resume")?;

        let mut matches = Vec::new();
        let mut offset = 0usize;
        let mut truncated_matches = false;
        while offset <= bytes.len().saturating_sub(pattern.len()) {
            let Some(relative) = find_subslice(&bytes[offset..], &pattern) else {
                break;
            };
            let relative = offset + relative;
            if relative.is_multiple_of(align) {
                if matches.len() >= max_matches {
                    truncated_matches = true;
                    break;
                }
                matches.push(start + relative as u64);
            }
            offset = relative + 1;
        }

        Ok(json!({
            "memory_type": "ee",
            "start": start,
            "scanned": bytes.len(),
            "matches": matches,
            "count": matches.len(),
            "truncated": truncated_scan || truncated_matches,
            "truncated_scan": truncated_scan,
            "truncated_matches": truncated_matches,
        }))
    }

    pub(super) fn dump_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        std::fs::create_dir_all(&path)?;
        let was_running = self.emulator_state()? == "running";
        if was_running {
            self.command(MSG_EMUCAP_PAUSE, &[])?;
        }
        let dump = self.write_ee_dump(&path);
        let cleanup = if was_running {
            self.command(MSG_EMUCAP_RESUME, &[])
        } else {
            Ok(Vec::new())
        };
        finish_with_cleanup(dump, cleanup, "dump_memory resume")?;
        Ok(json!({ "path": path.display().to_string(), "regions": 1 }))
    }

    fn read_ee_range(&mut self, start: u64, length: usize) -> BridgeResult<Vec<u8>> {
        let mut bytes = Vec::with_capacity(length);
        let mut offset = 0usize;
        while offset < length {
            let chunk = MAX_MEMORY_TRANSFER.min(length - offset);
            bytes.extend_from_slice(&self.read_ee_chunk((start as usize + offset) as u32, chunk)?);
            offset += chunk;
        }
        Ok(bytes)
    }

    fn read_ee_chunk(&mut self, address: u32, length: usize) -> BridgeResult<Vec<u8>> {
        let mut body = Vec::with_capacity(8);
        body.extend_from_slice(&address.to_le_bytes());
        body.extend_from_slice(&(length as u32).to_le_bytes());
        let payload = self
            .command(MSG_EMUCAP_READ_BYTES, &body)
            .map_err(|error| {
                Pcsx2BridgeError::Emulator(format!(
                    "EE RAM read failed at [{address:#x}, {:#x}): {error}",
                    address as u64 + length as u64
                ))
            })?;
        if payload.len() != length {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "PCSX2 returned {} bytes for a {length}-byte EE RAM chunk at {address:#x}",
                payload.len()
            )));
        }
        Ok(payload)
    }

    fn write_ee_dump(&mut self, dir: &Path) -> BridgeResult<()> {
        let partial = dir.join(".ee.bin.partial");
        let final_path = dir.join("ee.bin");
        let result = (|| {
            let mut file = File::create(&partial)?;
            let mut offset = 0u64;
            while offset < PCSX2_EE_RAM_SIZE {
                let length = MAX_MEMORY_TRANSFER.min((PCSX2_EE_RAM_SIZE - offset) as usize);
                file.write_all(&self.read_ee_chunk(offset as u32, length)?)?;
                offset += length as u64;
            }
            file.flush()?;
            let written = file.metadata()?.len();
            if written != PCSX2_EE_RAM_SIZE {
                return Err(Pcsx2BridgeError::Emulator(format!(
                    "dump_memory wrote {written:#x} of {PCSX2_EE_RAM_SIZE:#x} EE RAM bytes"
                )));
            }
            drop(file);
            std::fs::rename(&partial, &final_path)?;
            let regions = [json!({
                "name": "ee",
                "memory_type": "ee",
                "base_address": 0,
                "size": PCSX2_EE_RAM_SIZE,
            })];
            std::fs::write(dir.join("regions.json"), serde_json::to_vec(&regions)?)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(partial);
        }
        result
    }
}

fn require_ee_memory_type(params: &Value) -> BridgeResult<()> {
    match params.get("memory_type").and_then(Value::as_str) {
        None | Some("ee") => Ok(()),
        Some(other) => Err(Pcsx2BridgeError::BadParams(format!(
            "unsupported memory_type `{other}`; valid: ee"
        ))),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn finish_with_cleanup<T>(
    operation: BridgeResult<T>,
    cleanup: BridgeResult<Vec<u8>>,
    context: &str,
) -> BridgeResult<T> {
    match (operation, cleanup) {
        (Ok(value), Ok(_)) => Ok(value),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(cleanup)) => Err(Pcsx2BridgeError::Emulator(format!(
            "{context} failed after the operation completed: {cleanup}"
        ))),
        (Err(operation), Err(cleanup)) => Err(Pcsx2BridgeError::Emulator(format!(
            "{operation}; {context} also failed: {cleanup}"
        ))),
    }
}
