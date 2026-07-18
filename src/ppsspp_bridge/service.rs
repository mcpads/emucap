use super::*;

impl<T: WsTransport> PpssppBridge<T> {
    pub(super) fn status(&mut self) -> BridgeResult<Value> {
        let version = self.ws.call("version", json!({}))?;
        let game_status = self.ws.call("game.status", json!({}))?;
        // `game.status`'s own `paused` field is `GetUIState() == UISTATE_PAUSEMENU` (the GUI pause
        // menu) — unrelated to the CPU debugger's halt state, and stays `false` even while the CPU
        // is stopped at a breakpoint (verified empirically). `cpu.status.stepping` is the real
        // debugger-halt indicator pause/resume/step_instructions act on, so `state` is derived from
        // that, not from `game.status`.
        let stepping = self.cpu_is_stepping()?;
        let ppsspp_version = version
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        Ok(json!({
            "connected": true,
            "system": "psp",
            "adapter": "ppsspp-rust-ws",
            "backend": "ppsspp-debugger-ws",
            "debugger": true,
            "state": if stepping { "frozen" } else { "running" },
            "methods": METHODS,
            "memory_types": MEMORY_TYPES,
            "contracts": crate::contracts::advertisement_value(&[
                "ppsspp.execution.frame-step-absent",
                "ppsspp.input-hold.port-zero-only",
                "ppsspp.input-pulse.constraints",
            ]),
            "capability_notes": capability_notes(),
            "ppsspp_version": ppsspp_version,
            "game": game_status.get("game").cloned().unwrap_or(Value::Null),
            "input_buttons": psp_input_buttons_json(),
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
            },
            "input_override": match &self.held_buttons {
                Some(buttons) => json!({
                    "observable": true,
                    "authority": "bridge_local",
                    "engaged": !buttons.is_empty(),
                    "mode": if buttons.is_empty() { "native" } else { "persistent" },
                    "buttons": buttons,
                }),
                None => json!({
                    "observable": false,
                    "authority": "unavailable_until_bridge_input",
                }),
            },
        }))
    }

    /// Identity-guard handshake — the connect-time surface a caller inspects before trusting the
    /// bridge. Unlike `status`, `methods` here is the same truthful `METHODS` list (not a live
    /// probe), since PPSSPP's WebSocket needs no per-request emulator round-trip to answer it.
    /// Echoes `name`/`session_token` (when the launcher set them via `EMUCAP_NAME`/
    /// `EMUCAP_SESSION_TOKEN`) so `emucap-mcp`'s TCP handshake can verify this bridge is the one it
    /// just spawned — without this echo the identity guard rejects every connection attempt with
    /// `IdentityMismatch` (mirrors the NDS/PC-98 bridges' `hello`).
    pub(super) fn hello(&self) -> BridgeResult<Value> {
        let mut result = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "psp",
            "adapter": "ppsspp-rust-ws",
            "backend": "ppsspp-debugger-ws",
            "debugger": true,
            "methods": METHODS,
            "memory_types": MEMORY_TYPES,
            "contracts": crate::contracts::advertisement_value(&[
                "ppsspp.execution.frame-step-absent",
                "ppsspp.input-hold.port-zero-only",
                "ppsspp.input-pulse.constraints",
            ]),
            "capability_notes": capability_notes(),
            "input_buttons": psp_input_buttons_json(),
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
            },
        });
        let obj = result.as_object_mut().expect("hello is an object");
        if let Some(name) = &self.name {
            obj.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.session_token {
            obj.insert("session_token".into(), json!(token));
        }
        if let Some(launch_id) = &self.launch_id {
            obj.insert("launch_id".into(), json!(launch_id));
        }
        Ok(result)
    }

    /// `memory.read {address, size}` → `{base64}`. `memory_type` resolves to an absolute PSP
    /// address (today only `main`, `PSP_MAIN_RAM_BASE + offset`); the base64 payload is decoded and
    /// re-emitted as hex to match the other adapters' `read_memory` wire shape.
    pub(super) fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let length = required_num(params, "length")? as usize;
        if length > MAX_READ_LEN {
            return Err(BridgeError::BadParams(format!(
                "read length {length:#x} exceeds the {MAX_READ_LEN:#x} cap — read a large region in chunks (advance the start address)"
            )));
        }
        let addr = route_main_address(params, length as u64)?;
        let result = self
            .ws
            .call("memory.read", json!({ "address": addr, "size": length }))?;
        let b64 = result
            .get("base64")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BridgeError::Emulator("memory.read: reply had no base64 field".into())
            })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|err| {
                BridgeError::Emulator(format!("memory.read: base64 decode failed: {err}"))
            })?;
        Ok(json!({ "hex": hex::encode(bytes) }))
    }

    /// hex → base64 → `memory.write {address, base64}`. Same `memory_type` → absolute-address
    /// routing as `read_memory`.
    pub(super) fn write_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let hexstr = required_str(params, "hex")?;
        if hexstr.len() % 2 != 0 {
            return Err(BridgeError::BadParams("hex must have even length".into()));
        }
        let bytes =
            hex::decode(hexstr).map_err(|_| BridgeError::BadParams("hex decode failed".into()))?;
        let addr = route_main_address(params, bytes.len() as u64)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        self.ws
            .call("memory.write", json!({ "address": addr, "base64": b64 }))?;
        Ok(json!({ "written": bytes.len() }))
    }

    /// Read exactly `len` bytes of `main` (user RAM) starting at `offset` into the region, via
    /// `memory.read` (base64 → raw bytes). `dump_memory` streams the whole region through this in
    /// `MAX_READ_LEN` chunks; the caller keeps `[offset, offset+len)` within `PSP_MAIN_RAM_SIZE`, so
    /// no out-of-region read reaches PPSSPP. A reply that decodes to fewer (or more) bytes than
    /// requested is a short read: it is rejected here so `dump_memory` fails cleanly rather than
    /// writing a `main.bin` smaller than the `regions.json` it advertises.
    pub(super) fn read_main_bytes(&mut self, offset: u64, len: usize) -> BridgeResult<Vec<u8>> {
        let addr = PSP_MAIN_RAM_BASE.checked_add(offset).ok_or_else(|| {
            BridgeError::BadParams(format!("main address overflow at offset {offset:#x}"))
        })?;
        let result = self
            .ws
            .call("memory.read", json!({ "address": addr, "size": len }))?;
        let b64 = result
            .get("base64")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BridgeError::Emulator("memory.read: reply had no base64 field".into())
            })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|err| {
                BridgeError::Emulator(format!("memory.read: base64 decode failed: {err}"))
            })?;
        if bytes.len() != len {
            return Err(BridgeError::Emulator(format!(
                "memory.read at {addr:#x}: requested {len} bytes but PPSSPP returned {} (short read)",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    /// Bulk-export every PSP memory region under `path` as `<name>.bin` region files +
    /// `regions.json` + a `state.json` register snapshot — so huge memory goes to files, not inline,
    /// and `emucap diff` / the cross-ROM diff recipe read PSP dumps with the same loader as every
    /// other adapter (`src/analysis/dump.rs`, the `regions.json` shape's single source of truth).
    /// Mirrors the PC-98 bridge's `dump_memory`: each region is streamed via `memory.read` in
    /// `MAX_READ_LEN` chunks bounded to `PSP_MAIN_RAM_SIZE` (today only `main`, user RAM). `state.json`
    /// is the unwrapped `get_state` map — the same file the MCP host writes uniformly for any adapter
    /// (`src/live/tools.rs`), so a direct bridge dump is already diff-ready.
    pub(super) fn dump_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        // The MCP host (src/live/tools.rs) hands us its own `dump-staging` sibling dir and does the
        // single atomic swap of the finished staging into the final dump location, so the bridge
        // writes region files directly into `path` with no dir-level swap of its own — mirroring the
        // NDS bridge. Each region streams to a `.partial` temp that is size-verified then atomically
        // renamed into `<name>.bin`, and `regions.json` is written last, so a short/failed
        // `memory.read` mid-stream leaves no truncated `.bin` and no manifest advertising one — the
        // host discards this whole staging dir on error.
        std::fs::create_dir_all(&path)?;
        let metas = self.build_dump(&path)?;
        // state.json is written by the MCP host uniformly for every adapter, so the bridge writes
        // only the region .bins + regions.json — matching the PC-98/NDS bridges (no redundant
        // get_state round-trip here that the host would immediately overwrite).
        Ok(json!({ "path": path.display().to_string(), "regions": metas.len() }))
    }

    /// Stream every `MEMORY_TYPES` region into `<name>.bin` under `dir` and write `regions.json`,
    /// verifying each `.bin` ends up exactly the region size. Each region streams to a `.partial`
    /// temp that is renamed into place only after its size is verified, and any read/write error
    /// discards that temp before propagating — so a partial dump leaves no truncated `.bin`. Returns
    /// the region metadata written.
    pub(super) fn build_dump(&mut self, dir: &Path) -> BridgeResult<Vec<Value>> {
        let mut metas = Vec::new();
        for &name in MEMORY_TYPES {
            let (base, size) = match name {
                "main" => (PSP_MAIN_RAM_BASE, PSP_MAIN_RAM_SIZE),
                other => {
                    return Err(BridgeError::Emulator(format!(
                        "dump_memory: no extent known for region {other}"
                    )))
                }
            };
            let tmp_path = dir.join(format!(".{name}.bin.partial"));
            if let Err(e) = self.stream_region_to(&tmp_path, size) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e);
            }
            let written = std::fs::metadata(&tmp_path)?.len();
            if written != size {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(BridgeError::Emulator(format!(
                    "dump_memory: {name}.bin is {written:#x} bytes, expected region size {size:#x}"
                )));
            }
            std::fs::rename(&tmp_path, dir.join(format!("{name}.bin")))?;
            metas.push(json!({
                "name": name,
                "memory_type": name,
                "base_address": base,
                "size": size,
            }));
        }
        std::fs::write(dir.join("regions.json"), serde_json::to_vec(&metas)?)?;
        Ok(metas)
    }

    /// Stream `size` bytes of `main` (user RAM) into the file at `path`, reading PPSSPP in
    /// `MAX_READ_LEN` chunks. Any short/failed `memory.read` propagates so `build_dump` can discard
    /// the partial file.
    pub(super) fn stream_region_to(&mut self, path: &Path, size: u64) -> BridgeResult<()> {
        let mut file = File::create(path)?;
        let mut offset = 0u64;
        while offset < size {
            let chunk = MAX_READ_LEN.min((size - offset) as usize);
            let bytes = self.read_main_bytes(offset, chunk)?;
            file.write_all(&bytes)?;
            // Advance by what was actually read (read_main_bytes already guarantees == chunk).
            offset += bytes.len() as u64;
        }
        file.flush()?;
        Ok(())
    }
}
