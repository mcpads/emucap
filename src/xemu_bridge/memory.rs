use super::*;
use std::fs;

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn gdb_command(&mut self, command: &str) -> XemuResult<String> {
        self.drain_gdb_stops(true)?;
        let mut response = self.gdb.send(command)?;
        while is_stop_packet(&response) {
            self.note_stop(response, true)?;
            response = self.gdb.recv_reply()?;
        }
        Ok(response)
    }

    pub(super) fn drain_gdb_stops(&mut self, report: bool) -> XemuResult<bool> {
        let before = self.events.len();
        while let Some(packet) = self.gdb.recv_nonblocking()? {
            if !is_stop_packet(&packet) {
                return Err(XemuBridgeError::Emulator(format!(
                    "unexpected asynchronous GDB packet: {packet}"
                )));
            }
            self.note_stop(packet, report)?;
        }
        Ok(self.events.len() > before)
    }

    pub(super) fn note_stop(&mut self, packet: String, report: bool) -> XemuResult<()> {
        if !report || stop_signal(&packet) == "02" {
            return Ok(());
        }
        if self.debug_stop_observed {
            return Ok(());
        }
        self.debug_stop_observed = true;
        let watch = stop_watch_address(&packet);
        self.events.push(json!({
            "type":"stop", "signal":stop_signal(&packet), "raw":packet,
            "watch":watch.as_ref().map(|(kind, _)| kind),
            "watch_address":watch.as_ref().map(|(_, address)| *address),
        }));
        Ok(())
    }

    pub(super) fn capture_debug_stop(&mut self, machine: &Value) -> XemuResult<()> {
        if machine.get("status").and_then(Value::as_str) != Some("debug")
            || self.debug_stop_observed
        {
            return Ok(());
        }
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            self.drain_gdb_stops(true)?;
            if self.debug_stop_observed {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Err(XemuBridgeError::Emulator(
            "xemu entered debug state but GDB did not deliver its stop packet within 500 ms".into(),
        ))
    }

    pub(super) fn read_abs_bytes(&mut self, address: u64, length: usize) -> XemuResult<Vec<u8>> {
        let mut bytes = Vec::with_capacity(length);
        let mut offset = 0usize;
        while offset < length {
            let chunk = (length - offset).min(MAX_MEMORY_CHUNK);
            let response =
                self.gdb_command(&format!("m{:x},{chunk:x}", address + offset as u64))?;
            if response.starts_with('E') {
                return Err(XemuBridgeError::Emulator(format!(
                    "GDB memory read failed: {response}"
                )));
            }
            let part = hex::decode(&response)
                .map_err(|_| XemuBridgeError::Emulator("GDB returned invalid memory hex".into()))?;
            if part.len() != chunk {
                return Err(XemuBridgeError::Emulator(format!(
                    "short GDB memory read at {:#x}: expected {chunk}, got {}",
                    address + offset as u64,
                    part.len()
                )));
            }
            bytes.extend_from_slice(&part);
            offset += chunk;
        }
        Ok(bytes)
    }

    pub(super) fn write_abs_bytes(&mut self, address: u64, bytes: &[u8]) -> XemuResult<()> {
        let mut offset = 0usize;
        while offset < bytes.len() {
            let chunk = (bytes.len() - offset).min(MAX_MEMORY_CHUNK);
            let response = self.gdb_command(&format!(
                "M{:x},{chunk:x}:{}",
                address + offset as u64,
                hex::encode(&bytes[offset..offset + chunk])
            ))?;
            if response != "OK" {
                return Err(XemuBridgeError::Emulator(format!(
                    "GDB memory write failed: {response}"
                )));
            }
            offset += chunk;
        }
        Ok(())
    }

    /// Run one bounded access against Xbox physical RAM, restoring the GDB stub's default
    /// virtual-address mode before returning. The bridge owns this GDB connection exclusively, so
    /// every public operation begins and ends in virtual mode. Keeping the mode switch inside the
    /// terminal request prevents a failed RAM read from changing the meaning of later CPU reads.
    pub(super) fn with_physical_memory<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> XemuResult<T>,
    ) -> XemuResult<T> {
        let response = self.gdb_command("Qqemu.PhyMemMode:1")?;
        if response != "OK" {
            return Err(XemuBridgeError::Emulator(format!(
                "xemu GDB physical-memory mode is unavailable: {response}"
            )));
        }
        let outcome = operation(self);
        let cleanup = (|| {
            let response = self.gdb_command("Qqemu.PhyMemMode:0")?;
            if response != "OK" {
                return Err(XemuBridgeError::Emulator(format!(
                    "xemu GDB virtual-memory mode restore failed: {response}"
                )));
            }
            Ok(())
        })();
        finish_operation(outcome, cleanup, "physical memory access")
    }

    pub(super) fn read_routed_bytes(
        &mut self,
        memory_type: &str,
        absolute: u64,
        length: usize,
    ) -> XemuResult<Vec<u8>> {
        if memory_type == "main" {
            let physical = absolute.checked_sub(XBOX_RAM_CPU_ALIAS).ok_or_else(|| {
                XemuBridgeError::BadParams("invalid Xbox main-memory route".into())
            })?;
            self.with_physical_memory(|bridge| bridge.read_abs_bytes(physical, length))
        } else {
            self.read_abs_bytes(absolute, length)
        }
    }

    fn write_routed_bytes(
        &mut self,
        memory_type: &str,
        absolute: u64,
        bytes: &[u8],
    ) -> XemuResult<()> {
        if memory_type == "main" {
            let physical = absolute.checked_sub(XBOX_RAM_CPU_ALIAS).ok_or_else(|| {
                XemuBridgeError::BadParams("invalid Xbox main-memory route".into())
            })?;
            self.with_physical_memory(|bridge| bridge.write_abs_bytes(physical, bytes))
        } else {
            self.write_abs_bytes(absolute, bytes)
        }
    }

    pub(super) fn read_memory(&mut self, params: &Value) -> XemuResult<Value> {
        let length = required_num(params, "length")? as usize;
        if length > MAX_MEMORY_TRANSFER {
            return Err(XemuBridgeError::BadParams(format!(
                "read length {length:#x} exceeds {MAX_MEMORY_TRANSFER:#x}"
            )));
        }
        let (memory_type, absolute, offset) = region_address(params, length as u64)?;
        let marker = self.freeze_for_operation()?;
        let outcome = self.read_routed_bytes(&memory_type, absolute, length);
        let cleanup = self.restore_after_operation(marker);
        let bytes = finish_operation(outcome, cleanup, "read_memory")?;
        Ok(json!({
            "memory_type":memory_type, "address":offset, "length":length,
            "hex":hex::encode(bytes)
        }))
    }

    pub(super) fn write_memory(&mut self, params: &Value) -> XemuResult<Value> {
        let raw = params
            .get("hex")
            .or_else(|| params.get("data"))
            .and_then(Value::as_str)
            .ok_or_else(|| XemuBridgeError::BadParams("missing required param: hex".into()))?;
        if raw.is_empty() || raw.len() % 2 != 0 {
            return Err(XemuBridgeError::BadParams(
                "hex must contain complete bytes".into(),
            ));
        }
        let bytes =
            hex::decode(raw).map_err(|_| XemuBridgeError::BadParams("hex decode failed".into()))?;
        if bytes.len() > MAX_MEMORY_TRANSFER {
            return Err(XemuBridgeError::BadParams(format!(
                "write length {:#x} exceeds {MAX_MEMORY_TRANSFER:#x}",
                bytes.len()
            )));
        }
        let (memory_type, absolute, offset) = region_address(params, bytes.len() as u64)?;
        let marker = self.freeze_for_operation()?;
        let outcome = self.write_routed_bytes(&memory_type, absolute, &bytes);
        let cleanup = self.restore_after_operation(marker);
        finish_operation(outcome, cleanup, "write_memory")?;
        Ok(json!({"memory_type":memory_type, "address":offset, "written":bytes.len()}))
    }

    pub(super) fn find_pattern(&mut self, params: &Value) -> XemuResult<Value> {
        let pattern = hex::decode(required_str(params, "hex")?)
            .map_err(|_| XemuBridgeError::BadParams("hex decode failed".into()))?;
        if pattern.is_empty() {
            return Err(XemuBridgeError::BadParams(
                "hex must contain at least one byte".into(),
            ));
        }
        let start = optional_num(params, "start")?.unwrap_or(0);
        let requested = optional_num(params, "length")?.unwrap_or(MAX_FIND_LEN as u64);
        let scan_len = requested.min(MAX_FIND_LEN as u64) as usize;
        let max_matches = optional_num(params, "max_matches")?
            .unwrap_or(256)
            .clamp(1, 4096) as usize;
        let align = optional_num(params, "align")?.unwrap_or(1).max(1) as usize;
        let mut routed = params.clone();
        routed["address"] = json!(start);
        let (memory_type, absolute, _) = region_address(&routed, scan_len as u64)?;
        let marker = self.freeze_for_operation()?;
        let outcome = self.read_routed_bytes(&memory_type, absolute, scan_len);
        let cleanup = self.restore_after_operation(marker);
        let bytes = finish_operation(outcome, cleanup, "find_pattern")?;

        let mut matches = Vec::new();
        let mut cursor = 0usize;
        let mut truncated_matches = false;
        while cursor <= bytes.len().saturating_sub(pattern.len()) {
            let Some(index) = find_subslice(&bytes[cursor..], &pattern) else {
                break;
            };
            let relative = cursor + index;
            if relative.is_multiple_of(align) {
                if matches.len() == max_matches {
                    truncated_matches = true;
                    break;
                }
                matches.push(start + relative as u64);
            }
            cursor = relative + 1;
        }
        Ok(json!({
            "memory_type":memory_type, "start":start, "scanned":scan_len,
            "matches":matches, "count":matches.len(),
            "truncated_scan":requested > MAX_FIND_LEN as u64,
            "truncated_matches":truncated_matches,
            "truncated":requested > MAX_FIND_LEN as u64 || truncated_matches,
        }))
    }

    pub(super) fn dump_memory(&mut self, params: &Value) -> XemuResult<Value> {
        let root = std::path::PathBuf::from(required_str(params, "path")?);
        fs::create_dir_all(&root)?;
        let marker = self.freeze_for_operation()?;
        let partial = root.join(".main.bin.partial");
        let outcome = (|| {
            let final_path = root.join("main.bin");
            let mut output = std::fs::File::create(&partial)?;
            use std::io::Write;
            let mut offset = 0u64;
            self.with_physical_memory(|bridge| {
                while offset < XBOX_RAM_SIZE {
                    let chunk = (XBOX_RAM_SIZE - offset).min(MAX_MEMORY_TRANSFER as u64) as usize;
                    let bytes = bridge.read_abs_bytes(offset, chunk)?;
                    output.write_all(&bytes)?;
                    offset += chunk as u64;
                }
                Ok(())
            })?;
            output.sync_all()?;
            if output.metadata()?.len() != XBOX_RAM_SIZE {
                return Err(XemuBridgeError::Emulator(
                    "Xbox main RAM dump was short".into(),
                ));
            }
            drop(output);
            fs::rename(&partial, &final_path)?;
            fs::write(
                root.join("regions.json"),
                serde_json::to_vec_pretty(&json!([{
                    "name":"main", "memory_type":"main", "base_address":0,
                    "size":XBOX_RAM_SIZE
                }]))?,
            )?;
            Ok(json!({
                "path":root.display().to_string(), "regions":["main"],
                "bytes":XBOX_RAM_SIZE
            }))
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&partial);
        }
        let cleanup = self.restore_after_operation(marker);
        finish_operation(outcome, cleanup, "dump_memory")
    }
}

pub(super) fn finish_operation<T>(
    outcome: XemuResult<T>,
    cleanup: XemuResult<()>,
    operation: &str,
) -> XemuResult<T> {
    crate::live::temporal::finish_with_cleanup(outcome, cleanup, |primary, cleanup| {
        XemuBridgeError::Emulator(match primary {
            Some(primary) => {
                format!("{operation} failed: {primary}; cleanup also failed: {cleanup}")
            }
            None => format!("{operation} completed but cleanup failed: {cleanup}"),
        })
    })
}
