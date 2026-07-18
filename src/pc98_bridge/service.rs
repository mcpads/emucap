use super::*;

impl<G: GdbTransport> Bridge<G> {
    pub(super) fn hello(&self) -> BridgeResult<Value> {
        let mut result = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": "pc98",
            "adapter": "mame-pc98-rust-gdb",
            "backend": "lua-gdbstub",
            "debugger": true,
            "methods": METHODS,
            "memory_types": memory_type_names(),
            "contracts": crate::contracts::advertisement_value(&[
                "pc98.call-stack.best-effort",
                "pc98.input-hold.port-zero-only",
                "pc98.input-pulse.constraints",
            ]),
            "region_sizes": region_sizes_json(),
            "capability_notes": {
                "backend": "lua-gdbstub",
                "rust_bridge": true,
                "implemented_methods": METHODS,
                "screenshot": true,
                "input": true,
                "frame_step": true,
                "step_units": ["frames", "instructions"],
                "breakpoints": true,
                "watch_register": true,
                "trace": true,
                "state_restore": state_restore_info(),
            },
            "input_buttons": input_buttons_json(),
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
            },
        });
        let obj = result.as_object_mut().expect("hello is an object");
        if let Some(name) = &self.env.name {
            obj.insert("name".into(), json!(name));
        }
        if let Some(token) = &self.env.session_token {
            obj.insert("session_token".into(), json!(token));
        }
        if let Some(launch_id) = &self.env.launch_id {
            obj.insert("launch_id".into(), json!(launch_id));
        }
        if let Some(content) = &self.env.content {
            obj.insert("content".into(), json!(content.display().to_string()));
        }
        obj.insert(
            "build".into(),
            json!(self.env.build.as_deref().unwrap_or("unknown")),
        );
        Ok(result)
    }

    pub(super) fn status(&mut self) -> BridgeResult<Value> {
        self.drain_stop()?;
        let mut input_buttons = input_buttons_json();
        let available = self.refresh_input_fields();
        if let Some(obj) = input_buttons.as_object_mut() {
            obj.insert("available".into(), json!(available));
        }
        let input_override = self.input_override_info();
        Ok(json!({
            "connected": true,
            "system": "pc98",
            "adapter": "mame-pc98-rust-gdb",
            "backend": "lua-gdbstub",
            "debugger": true,
            "frame": self.current_frame(),
            "state": if self.frozen { "frozen" } else { "running" },
            "memory_types": memory_type_names(),
            "contracts": crate::contracts::advertisement_value(&[
                "pc98.call-stack.best-effort",
                "pc98.input-hold.port-zero-only",
                "pc98.input-pulse.constraints",
            ]),
            "capability_notes": {
                "backend": "lua-gdbstub",
                "rust_bridge": true,
                "implemented_methods": METHODS,
                "screenshot": true,
                "input": true,
                "frame_step": true,
                "step_units": ["frames", "instructions"],
                "breakpoints": true,
                "watch_register": true,
                "trace": true,
                "state_restore": state_restore_info(),
            },
            "input_buttons": input_buttons,
            "input_override": input_override,
            "execution_limits": {
                "max_sync_advance_count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT,
                "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
                "frame": {
                    "max_count": self.max_sync_frame_count(),
                    "estimated_ms_per_frame": self.frame_operation_budget_ms(),
                    "trace_enabled": self.tracing,
                },
            },
        }))
    }

    pub(super) fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let length = required_num(params, "length")?;
        let address = region_address(params, length)?;
        let length = length as usize;
        let hex = self.read_abs_hex(address, length)?;
        Ok(json!({ "hex": hex }))
    }

    pub(super) fn write_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let hexstr = required_str(params, "hex")?;
        if hexstr.len() % 2 != 0 {
            return Err(BridgeError::BadParams("hex must have even length".into()));
        }
        let data =
            hex::decode(hexstr).map_err(|_| BridgeError::BadParams("hex decode failed".into()))?;
        let size = data.len();
        let address = region_address(params, size as u64)?;
        let resp = self.send_cmd(&format!("M{address:x},{size:x}:{hexstr}"))?;
        if resp != "OK" {
            return Err(BridgeError::Emulator(format!(
                "GDB memory write failed: {resp}"
            )));
        }
        Ok(json!({ "written": size }))
    }

    pub(super) fn find_pattern(&mut self, params: &Value) -> BridgeResult<Value> {
        let memory_type = params
            .get("memory_type")
            .and_then(Value::as_str)
            .unwrap_or("physical");
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let pattern = hex::decode(required_str(params, "hex")?)
            .map_err(|_| BridgeError::BadParams("hex decode failed".into()))?;
        if pattern.is_empty() {
            return Err(BridgeError::BadParams(
                "hex must contain at least one byte".into(),
            ));
        }

        let start = optional_num(params, "start")?.unwrap_or(0) as usize;
        let mut length = optional_num(params, "length")?
            .map(|v| v as usize)
            .unwrap_or_else(|| region.size.saturating_sub(start as u32) as usize);
        if start >= region.size as usize {
            length = 0;
        } else {
            length = length.min(region.size as usize - start);
        }
        let truncated_scan = length > MAX_FIND_LEN;
        let scan_len = length.min(MAX_FIND_LEN);
        let max_matches = optional_num(params, "max_matches")?
            .unwrap_or(256)
            .clamp(1, 4096) as usize;
        let align = optional_num(params, "align")?.unwrap_or(1).max(1) as usize;

        let buf = self.read_region_bytes(memory_type, start, scan_len)?;
        let mut matches = Vec::new();
        let mut truncated_matches = false;
        let mut pos = 0usize;
        while pos <= buf.len().saturating_sub(pattern.len()) {
            let Some(idx) = find_subslice(&buf[pos..], &pattern) else {
                break;
            };
            let off = start + pos + idx;
            if (off - start).is_multiple_of(align) {
                if matches.len() >= max_matches {
                    truncated_matches = true;
                    break;
                }
                matches.push(off);
            }
            pos += idx + 1;
        }

        Ok(json!({
            "matches": matches,
            "count": matches.len(),
            "truncated": truncated_scan || truncated_matches,
            "truncated_scan": truncated_scan,
            "truncated_matches": truncated_matches,
            "scanned": scan_len,
            "start": start,
        }))
    }

    pub(super) fn dump_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        fs::create_dir_all(&path)?;
        let mut metas = Vec::new();
        for name in DUMP_REGION_NAMES {
            let region = memory_region(name).expect("dump region is declared");
            let out_path = path.join(format!("{name}.bin"));
            let mut file = File::create(out_path)?;
            let mut offset = 0usize;
            while offset < region.size as usize {
                let chunk = MAX_READ_CHUNK.min(region.size as usize - offset);
                file.write_all(&self.read_region_bytes(name, offset, chunk)?)?;
                offset += chunk;
            }
            metas.push(json!({
                "name": name,
                "memory_type": name,
                "base_address": region.base,
                "size": region.size,
            }));
        }
        let regions_path = path.join("regions.json");
        fs::write(&regions_path, serde_json::to_vec(&metas)?)?;
        Ok(json!({ "path": path.display().to_string(), "regions": metas.len() }))
    }

    pub(super) fn get_state(&mut self) -> BridgeResult<Value> {
        let regs = self.read_regs_hex()?;
        Ok(json!({ "state": state_from_regs_hex(&regs) }))
    }

    pub(super) fn save_state(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        let out_path = absolute_path(&path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.stop_for_state_restore()?;
        let regs_hex = self.read_regs_hex()?;
        let save_items_dir = unique_temp_dir("emucap_pc98_saveitems_")?;
        // Stage the zip to a sibling `.partial` and rename over out_path only once it is fully
        // written, so a mid-save failure (region read timeout, peer close, ENOSPC, kill) leaves any
        // pre-existing savestate byte-for-byte intact instead of truncating it. Mirrors the NDS
        // dump_memory and PPSSPP dump atomic-swap.
        let partial_path = state_partial_sibling(&out_path)?;
        let result = (|| {
            let mut save_items = self.save_lua_save_items(&save_items_dir)?;
            save_items.insert("dir".into(), json!(SAVE_ITEMS_DIR));
            let save_items_members = save_item_members(&save_items_dir)?;
            let file = File::create(&partial_path)?;
            let mut zip = ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            let mut regions = Vec::new();
            for name in DUMP_REGION_NAMES {
                let region = memory_region(name).expect("dump region is declared");
                let member = format!("{name}.bin");
                zip.start_file(&member, options)?;
                let mut offset = 0usize;
                while offset < region.size as usize {
                    let chunk = MAX_READ_CHUNK.min(region.size as usize - offset);
                    zip.write_all(&self.read_region_bytes(name, offset, chunk)?)?;
                    offset += chunk;
                }
                regions.push(json!({
                    "name": name,
                    "memory_type": name,
                    "base_address": region.base,
                    "size": region.size,
                    "file": member,
                }));
            }
            for (src_path, member) in save_items_members {
                zip.start_file(member, options)?;
                let mut file = File::open(src_path)?;
                std::io::copy(&mut file, &mut zip)?;
            }
            zip.start_file("state.json", options)?;
            let manifest = json!({
                "format": STATE_FORMAT,
                "system": "pc98",
                "adapter": "mame-pc98-rust-gdb",
                "registers_hex": regs_hex,
                "regions": regions,
                "save_items": save_items,
                "state_restore": state_restore_info(),
            });
            zip.write_all(&serde_json::to_vec(&manifest)?)?;
            zip.finish()?;
            fs::rename(&partial_path, &out_path)?;
            let bytes = out_path.metadata()?.len();
            Ok(json!({
                "path": path.display().to_string(),
                "format": STATE_FORMAT,
                "regions": regions.len(),
                "save_items": save_items,
                "bytes": bytes,
                "state_restore": state_restore_info(),
            }))
        })();
        let _ = fs::remove_dir_all(&save_items_dir);
        if result.is_err() {
            // The rename never ran, so out_path (any prior save) is untouched; drop the partial zip.
            let _ = fs::remove_file(&partial_path);
        }
        result
    }

    pub(super) fn load_state(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "path")?);
        if !path.is_file() {
            return Err(BridgeError::BadParams(format!(
                "save state not found: {}",
                path.display()
            )));
        }
        self.stop_for_state_restore()?;
        let load_items_dir = unique_temp_dir("emucap_pc98_loaditems_")?;
        let result = (|| {
            let file = File::open(&path)?;
            let mut archive = ZipArchive::new(file)?;
            let manifest = read_state_manifest(&mut archive)?;
            let state_format = state_format(&manifest)?;
            let save_items_dir = extract_save_items(&mut archive, &manifest, &load_items_dir)?;
            let regions = read_state_regions(&mut archive, &manifest)?;
            let regs_hex = manifest
                .get("registers_hex")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            drop(archive);
            let save_items_result = match save_items_dir {
                Some(dir) => self.load_lua_save_items(&dir)?,
                None => serde_json::Map::new(),
            };
            self.write_state_regions(&regions)?;
            let mut restore_result = serde_json::Map::new();
            restore_result.insert("restore_strategy".into(), json!("memory_only"));
            restore_result.insert("post_restore_instruction_exact".into(), json!(true));
            if !regs_hex.is_empty() {
                restore_result = self.restore_regs_after_state_load(&regs_hex)?;
            }
            self.frozen = true;
            let mut out = serde_json::Map::new();
            out.insert("path".into(), json!(path.display().to_string()));
            out.insert("format".into(), json!(state_format));
            out.insert("regions".into(), json!(regions.len()));
            out.insert("state_restore".into(), state_restore_info());
            out.extend(save_items_result);
            out.extend(restore_result);
            Ok(Value::Object(out))
        })();
        let _ = fs::remove_dir_all(&load_items_dir);
        result
    }

    pub(super) fn probe(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = PathBuf::from(required_str(params, "state")?);
        if !path.is_file() {
            return Err(BridgeError::BadParams(format!(
                "save state not found: {}",
                path.display()
            )));
        }
        let frame = match optional_num(params, "frame")? {
            Some(frame) => frame,
            None => optional_num(params, "frames")?.unwrap_or(0),
        };
        let memory_type = params
            .get("memory_type")
            .and_then(Value::as_str)
            .unwrap_or("physical");
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let address = region.base as u64 + required_num(params, "address")?;
        let length = required_num(params, "length")? as usize;
        self.stop_for_state_restore()?;
        let load_items_dir = unique_temp_dir("emucap_pc98_probeitems_")?;
        let result = (|| {
            let file = File::open(&path)?;
            let mut archive = ZipArchive::new(file)?;
            let manifest = read_state_manifest(&mut archive)?;
            let _ = state_format(&manifest)?;
            let save_items_dir = extract_save_items(&mut archive, &manifest, &load_items_dir)?;
            let regions = read_state_regions(&mut archive, &manifest)?;
            let regs_hex = manifest
                .get("registers_hex")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    BridgeError::BadParams("PC-98 probe state is missing registers_hex".into())
                })?
                .to_string();
            drop(archive);
            let save_items_result = match save_items_dir {
                Some(dir) => self.load_lua_save_items(&dir)?,
                None => serde_json::Map::new(),
            };
            self.write_state_regions(&regions)?;
            let mut result = self.register_probe(&regs_hex, frame, address, length)?;
            if let Some(obj) = result.as_object_mut() {
                obj.extend(save_items_result);
            }
            self.frozen = true;
            Ok(result)
        })();
        let _ = fs::remove_dir_all(&load_items_dir);
        result
    }
}
