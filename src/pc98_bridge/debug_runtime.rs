use super::*;

impl<G: GdbTransport> Bridge<G> {
    pub(super) fn set_trace(&mut self, params: &Value) -> BridgeResult<Value> {
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if enabled {
            let path = match &self.trace_path {
                Some(path) => {
                    let _ = fs::remove_file(path);
                    path.clone()
                }
                None => {
                    let path = std::env::temp_dir().join(format!(
                        "emucap_pc98_trace_{}_{}.log",
                        std::process::id(),
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or_default()
                    ));
                    self.trace_path = Some(path.clone());
                    path
                }
            };
            self.lua_cmd("tracestart", Some(path.to_string_lossy().as_ref()))?;
            self.tracing = true;
            return Ok(json!({ "tracing": true, "path": path.display().to_string() }));
        }
        if self.tracing {
            self.lua_cmd("traceflush", None)?;
            self.lua_cmd("tracestop", None)?;
        }
        self.tracing = false;
        Ok(json!({
            "tracing": false,
            "path": self.trace_path.as_ref().map(|p| p.display().to_string()),
        }))
    }

    pub(super) fn get_trace(&mut self, params: &Value) -> BridgeResult<Value> {
        let count = optional_num(params, "count")?
            .unwrap_or(64)
            .clamp(1, TRACE_CAP as u64) as usize;
        let rows = self.read_trace_rows()?;
        let start = rows.len().saturating_sub(count);
        Ok(json!({
            "trace": rows[start..].to_vec(),
            "tracing": self.tracing,
            "total": rows.len(),
            "path": self.trace_path.as_ref().map(|p| p.display().to_string()),
        }))
    }

    pub(super) fn call_stack(&mut self) -> BridgeResult<Value> {
        // 트레이싱 중이면 call/ret 트레이스 스캔이 정확하니 그대로 쓴다. 아니면 정지 상태의
        // BP(EBP) 체인을 걸어 트레이스 없이 복원한다 — method 필드로 호출자가 신뢰도를 판단한다.
        if self.tracing {
            self.call_stack_from_trace()
        } else {
            self.call_stack_from_frame_pointer()
        }
    }

    pub(super) fn call_stack_from_trace(&mut self) -> BridgeResult<Value> {
        let rows = self.read_trace_rows()?;
        let mut stack = Vec::new();
        let mut frames = Vec::new();
        for row in &rows {
            let text = row
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let pc = row.get("pc").and_then(Value::as_u64);
            if text.starts_with("call") {
                if let Some(pc) = pc {
                    stack.push(pc);
                    frames.push(json!({ "pc": pc, "text": row.get("text").cloned().unwrap_or(Value::String(String::new())) }));
                }
            } else if text.starts_with("ret") && !stack.is_empty() {
                stack.pop();
                frames.pop();
            }
        }
        Ok(json!({
            "call_stack": stack,
            "frames": frames,
            "depth": stack.len(),
            "method": "trace",
            "tracing": self.tracing,
            "total": rows.len(),
        }))
    }

    pub(super) fn call_stack_from_frame_pointer(&mut self) -> BridgeResult<Value> {
        // 표준 BP 프롤로그(push bp; mov bp,sp)를 가정한다 — 모든 루틴이 이를 지키진 않으므로
        // method="frame_pointer"로 알려 호출자가 신뢰도를 판단하게 한다.
        let state = state_from_regs_hex(&self.read_regs_hex()?);
        let get = |name: &str| state.get(name).and_then(Value::as_u64).unwrap_or(0);
        let (ebp, esp, eip, ss) = (
            get("cpu.ebp"),
            get("cpu.esp"),
            get("cpu.eip"),
            get("cpu.ss"),
        );
        // CR0.PE는 RSP 레지스터 셋에 없다. 값 크기로 real16 vs protected32를 추정한다(caveat:
        // 라이브 검증 필요). 모두 16비트 안이면 real16, 아니면 32비트 평면으로 본다.
        let real_mode = ebp <= 0xFFFF && esp <= 0xFFFF && eip <= 0xFFFF;
        let (ptr_size, seg_base, bp_mask) = if real_mode {
            (2usize, ss << 4, 0xFFFFu64)
        } else {
            (4usize, 0u64, 0xFFFF_FFFFu64)
        };
        let mut bp = ebp & bp_mask;
        let mut stack = Vec::new();
        let mut frames = Vec::new();
        for _ in 0..64 {
            if bp == 0 {
                break;
            }
            let base = seg_base.wrapping_add(bp);
            // 1MB+A20 상한을 넘는 주소는 무효로 보고 멈춘다.
            if base.saturating_add(2 * ptr_size as u64) > 0x0011_0000 {
                break;
            }
            let Some(saved_bp) = self.read_ptr_le(base, ptr_size) else {
                break;
            };
            let Some(ret_addr) = self.read_ptr_le(base + ptr_size as u64, ptr_size) else {
                break;
            };
            stack.push(ret_addr);
            frames.push(json!({ "pc": ret_addr, "frame_pointer": bp }));
            if saved_bp <= bp {
                // 비-증가/무효 bp → 프레임 체인 종료.
                break;
            }
            bp = saved_bp & bp_mask;
        }
        Ok(json!({
            "call_stack": stack,
            "frames": frames,
            "depth": stack.len(),
            "method": "frame_pointer",
            "mode": if real_mode { "real16" } else { "protected32" },
            "pointer_size": ptr_size,
            "frame_pointer": ebp & bp_mask,
            "tracing": self.tracing,
        }))
    }

    pub(super) fn read_ptr_le(&mut self, address: u64, size: usize) -> Option<u64> {
        let hex = self.read_abs_hex(address, size).ok()?;
        little_hex_to_u64(&hex)
    }

    pub(super) fn step_instruction_count(&mut self, count: u64) -> BridgeResult<Value> {
        if count > crate::live::temporal::MAX_SYNC_ADVANCE_COUNT {
            return Err(BridgeError::BadParams(format!(
                "instruction count {count} exceeds the synchronous cap {}; split the advance and verify each terminal response",
                crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
            )));
        }
        let deadline = std::time::Instant::now() + crate::live::temporal::MAX_SYNC_OPERATION_TIME;
        for completed in 0..count {
            if std::time::Instant::now() >= deadline {
                self.frozen = true;
                return Err(BridgeError::Emulator(format!(
                    "instruction step deadline exceeded after {completed} of {count}; the CPU remains frozen"
                )));
            }
            // s는 정상 응답 자체가 stop이라 send_cmd의 demux(command_expects_stop 아닌 명령만)가
            // 스킵된다. 그래서 s 앞에 낀 stale async stop(직전 framestep/BP 히트)은 send_cmd로도
            // 안 걷혀 s의 응답 자리에 오배달되고, 스텝이 실제로 안 돌고도 완료로 오인돼 off-by-one
            // 디싱크가 남는다. 스텝 전에 버퍼의 stale stop을 이벤트 큐로 걷어낸 뒤(=note_stop) s를
            // send_cmd로 보내 진짜 스텝 완료 stop을 응답으로 받는다(=re-read).
            self.drain_buffered_stops()?;
            let resp = self.send_cmd("s")?;
            if resp.starts_with('E') {
                return Err(BridgeError::Emulator(format!(
                    "GDB instruction step failed: {resp}"
                )));
            }
            if !is_stop_packet(&resp) {
                return Err(BridgeError::Emulator(format!(
                    "GDB instruction step returned unexpected response: {resp}"
                )));
            }
            self.frozen = true;
        }
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "count": count,
        }))
    }

    pub(super) fn stop_for_state_restore(&mut self) -> BridgeResult<()> {
        self.lua_cmd("stop", None)?;
        self.frozen = true;
        Ok(())
    }

    pub(super) fn save_lua_save_items(
        &mut self,
        path: &Path,
    ) -> BridgeResult<serde_json::Map<String, Value>> {
        fs::create_dir_all(path)?;
        let resp = self.lua_cmd_reply("saveitems", Some(path.to_string_lossy().as_ref()))?;
        parse_save_items_response(&resp, "saveitems")
    }

    pub(super) fn load_lua_save_items(
        &mut self,
        path: &Path,
    ) -> BridgeResult<serde_json::Map<String, Value>> {
        let parsed = parse_save_items_response(
            &self.lua_cmd_reply("loaditems", Some(path.to_string_lossy().as_ref()))?,
            "loaditems",
        )?;
        let mut out = serde_json::Map::new();
        out.insert(
            "save_items_restored".into(),
            parsed
                .get("items")
                .cloned()
                .unwrap_or_else(|| Value::Number(0.into())),
        );
        out.insert(
            "save_items_skipped".into(),
            parsed
                .get("skipped")
                .cloned()
                .unwrap_or_else(|| Value::Number(0.into())),
        );
        Ok(out)
    }

    pub(super) fn write_state_regions(
        &mut self,
        regions: &[(String, Vec<u8>)],
    ) -> BridgeResult<()> {
        for (memory_type, data) in regions {
            self.write_region_bytes(memory_type, 0, data)?;
        }
        Ok(())
    }

    pub(super) fn write_region_bytes(
        &mut self,
        memory_type: &str,
        start: usize,
        data: &[u8],
    ) -> BridgeResult<()> {
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk = MAX_READ_CHUNK.min(data.len() - offset);
            let hex = hex::encode(&data[offset..offset + chunk]);
            let address = region.base as u64 + start as u64 + offset as u64;
            let resp = self.send_cmd(&format!("M{address:x},{chunk:x}:{hex}"))?;
            if resp != "OK" {
                return Err(BridgeError::Emulator(format!(
                    "GDB memory write failed: {resp}"
                )));
            }
            offset += chunk;
        }
        Ok(())
    }

    pub(super) fn restore_regs_after_state_load(
        &mut self,
        regs_hex: &str,
    ) -> BridgeResult<serde_json::Map<String, Value>> {
        let current = self.load_regs_via_lua(regs_hex)?;
        self.frozen = true;
        let target = state_from_regs_hex(regs_hex);
        let exact = state_matches_real_mode_pc(&current, &target);
        let mut out = serde_json::Map::new();
        out.insert("restore_strategy".into(), json!("lua_register_load_hold"));
        out.insert("post_restore_instruction_exact".into(), json!(exact));
        out.insert(
            "observed_register_packet_matches_target".into(),
            json!(exact),
        );
        for key in ["cpu.pc", "cpu.eip", "cpu.cs"] {
            if let Some(value) = current.get(key).cloned() {
                let out_key = match key {
                    "cpu.pc" => "observed_pc",
                    "cpu.eip" => "observed_eip",
                    "cpu.cs" => "observed_cs",
                    _ => key,
                };
                out.insert(out_key.into(), value);
            }
        }
        Ok(out)
    }

    pub(super) fn load_regs_via_lua(&mut self, regs_hex: &str) -> BridgeResult<Value> {
        let resp = self.lua_cmd_reply("regload", Some(regs_hex))?;
        let Some(regs) = resp.strip_prefix("OK|") else {
            return Err(BridgeError::Emulator(format!(
                "MAME register load failed: {resp}"
            )));
        };
        Ok(state_from_regs_hex(regs))
    }

    pub(super) fn register_probe(
        &mut self,
        regs_hex: &str,
        frames: u64,
        address: u64,
        length: usize,
    ) -> BridgeResult<Value> {
        let spec = format!("{regs_hex}|{frames}|{address:x}|{length:x}");
        let resp = self.lua_cmd_reply("regprobe", Some(&spec))?;
        let result = parse_register_probe_response(&resp)?;
        let actual = result
            .get("hex")
            .and_then(Value::as_str)
            .map(str::len)
            .unwrap_or(0);
        if actual != length.saturating_mul(2) {
            return Err(BridgeError::Emulator(format!(
                "MAME register probe returned {} bytes, expected {length}",
                actual / 2
            )));
        }
        Ok(result)
    }

    pub(super) fn read_abs_hex(&mut self, address: u64, length: usize) -> BridgeResult<String> {
        let mut out = String::with_capacity(length.saturating_mul(2));
        let mut offset = 0usize;
        while offset < length {
            let chunk = std::cmp::min(MAX_READ_CHUNK, length - offset);
            // send_cmd 경유로 demux한다(raw send 금지). m 응답 앞에 낀 stale async stop이 이
            // 읽기의 응답 자리에 오배달되면 stop 문자열이 hex로 디코드돼 실패하고 이후 요청이
            // 통째로 off-by-one 디싱크된다. m은 command_expects_stop이 아니라 send_cmd가 앞선
            // stop을 이벤트 큐로 걷어내고 진짜 hex 응답을 이어 읽는다.
            let resp =
                self.send_cmd_data(&format!("m{:x},{:x}", address + offset as u64, chunk))?;
            if resp.starts_with('E') {
                return Err(BridgeError::Emulator(format!(
                    "GDB memory read failed: {resp}"
                )));
            }
            out.push_str(&resp);
            offset += chunk;
        }
        Ok(out)
    }

    pub(super) fn read_regs_hex(&mut self) -> BridgeResult<String> {
        let resp = self.send_cmd_data("g")?;
        if resp.starts_with('E') {
            return Err(BridgeError::Emulator(format!(
                "GDB register read failed: {resp}"
            )));
        }
        Ok(resp)
    }

    pub(super) fn read_region_bytes(
        &mut self,
        memory_type: &str,
        start: usize,
        length: usize,
    ) -> BridgeResult<Vec<u8>> {
        let region = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported memory_type: {memory_type}"))
        })?;
        let hex = self.read_abs_hex(region.base as u64 + start as u64, length)?;
        hex::decode(hex).map_err(|_| BridgeError::Emulator("GDB returned invalid hex".into()))
    }

    pub(super) fn send_cmd(&mut self, payload: &str) -> BridgeResult<String> {
        // framestep/BP/WP 히트의 async stop이 drain 창 밖에서 도착하면 버퍼에 남아, 이 데이터
        // 명령의 응답 자리에 오배달돼 이후 요청/응답이 통째로 off-by-one 디싱크된다. stop을
        // 정상 응답으로 받는 명령이 아니면, 앞선 stale stop을 이벤트 큐로 걷어내고 진짜 응답을
        // 이어 읽는다.
        let mut resp = self.gdb.send(payload)?;
        if !command_expects_stop(payload) {
            while is_stop_packet(&resp) {
                self.note_stop(resp, false);
                resp = self.gdb.recv_reply()?;
            }
        }
        Ok(resp)
    }

    /// 데이터(hex/숫자) 응답을 기대하는 명령용 — send_cmd의 stale-stop demux에 더해 스트림에 낀 stale "OK"도
    /// 걷어낸다. 트레이싱 중 runframes의 frame-target이 pause_on_hit BP 히트와 겹치면 하나의 runframes에
    /// 완료 "OK"와 BP stop이 이중 응답되고, 브리지가 그중 하나를 소비하면 나머지(늦게 도착한 stale "OK")가
    /// 다음 데이터 명령(g 레지스터·m 메모리·qEmucap,frame·기타 lua_cmd_reply 읽기)의 응답 자리에 오배달돼
    /// off-by-one desync된다(get_state가 raw_register_bytes로 깨지고 이후 traceflush가 register 패킷을 받음).
    /// 데이터를 기대하는 호출자만 이 경로를 쓰고(호출자가 의도 선언), "OK"가 유효 응답인 명령(쓰기 M/G,
    /// lua_cmd)은 send_cmd를 그대로 써 정상 OK를 소비한다. 드레인 창 크기에 의존하지 않는 결정론적 재정렬.
    pub(super) fn send_cmd_data(&mut self, payload: &str) -> BridgeResult<String> {
        debug_assert!(!command_expects_stop(payload));
        let mut resp = self.gdb.send(payload)?;
        // 데이터 응답 앞에 낀 stale stop(이벤트 큐로)과 stale "OK"(폐기)를 모두 걷어내고 진짜 응답을 읽는다.
        loop {
            if is_stop_packet(&resp) {
                self.note_stop(resp, false);
            } else if resp == "OK" {
                // stale 완료 OK — 이 데이터 명령의 유효 응답이 아니므로 폐기(이중 응답의 잔재).
            } else {
                return Ok(resp);
            }
            resp = self.gdb.recv_reply()?;
        }
    }

    pub(super) fn lua_cmd(&mut self, name: &str, arg: Option<&str>) -> BridgeResult<String> {
        let mut payload = format!("qEmucap,{name}");
        if let Some(arg) = arg {
            payload.push(',');
            payload.push_str(&hex::encode(arg.as_bytes()));
        }
        let resp = self.send_cmd(&payload)?;
        if resp.is_empty() || resp.starts_with('E') {
            return Err(BridgeError::Emulator(format!(
                "MAME Lua command {name} failed: {resp}"
            )));
        }
        if resp != "OK" {
            return Err(BridgeError::Emulator(format!(
                "MAME Lua command {name} failed: {resp}"
            )));
        }
        Ok(resp)
    }

    pub(super) fn lua_cmd_reply(&mut self, name: &str, arg: Option<&str>) -> BridgeResult<String> {
        let resp = self.lua_cmd_raw(name, arg)?;
        if resp.is_empty() || resp.starts_with('E') {
            Err(BridgeError::Emulator(format!(
                "MAME Lua command {name} failed: {resp}"
            )))
        } else {
            Ok(resp)
        }
    }

    pub(super) fn lua_cmd_raw(&mut self, name: &str, arg: Option<&str>) -> BridgeResult<String> {
        let mut payload = format!("qEmucap,{name}");
        if let Some(arg) = arg {
            payload.push(',');
            payload.push_str(&hex::encode(arg.as_bytes()));
        }
        // 주의: lua_cmd_reply 명령 중 clearpoint 등은 bare "OK"를 정상 반환하므로 여기서 send_cmd_data로
        // 드레인하면 안 된다(진짜 OK를 stale로 오인해 hang). stale-OK 드레인은 응답이 절대 bare "OK"가 아닌
        // 데이터 명령(g/m/qEmucap,frame)에서만 명시적으로 send_cmd_data로 한다.
        self.send_cmd(&payload)
    }

    pub(super) fn current_frame(&mut self) -> Option<u64> {
        // frames_op(runframes/framestep) 직후 run_frames/step 핸들러가 필수로 호출한다 — 그 직전 이벤트
        // (BP 히트가 frame-target과 겹침)의 spurious bare "OK"가 이 frame 응답 자리에 오배달되면, 이후 g가
        // 프레임 10진수를 레지스터 hex로 오소비해 desync된다. frame 응답은 10진수(bare OK가 아님)라
        // send_cmd_data로 앞에 낀 stale bare "OK"를 걷어낸다.
        self.send_cmd_data("qEmucap,frame")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
    }

    pub(super) fn frames_op(&mut self, name: &str, frames: u64) -> BridgeResult<Option<String>> {
        self.deferred_lua_op(name, &frames.to_string(), frames)
    }

    pub(super) fn deferred_lua_op(
        &mut self,
        name: &str,
        arg: &str,
        budget_frames: u64,
    ) -> BridgeResult<Option<String>> {
        let per_frame_ms = self.frame_operation_budget_ms();
        let estimated_ms = 5_000u64.saturating_add(budget_frames.saturating_mul(per_frame_ms));
        let deadline_ms = crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64;
        if budget_frames > crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
            || estimated_ms > deadline_ms
        {
            return Err(BridgeError::BadParams(format!(
                "{name} frame count {budget_frames} exceeds the current synchronous limit {} ({} ms/frame estimate, {deadline_ms} ms deadline); split the advance and verify each terminal response",
                self.max_sync_frame_count(),
                per_frame_ms
            )));
        }
        // s(step)와 마찬가지로 framestep/runframes도 응답 자체가 stop이라 send_cmd의 stale-stop demux가
        // command_expects_stop로 스킵된다. 직전 resume()가 pause_on_hit BP를 물어 남긴 버퍼된 stop이
        // 앞에 끼면 이 프레임 명령의 응답 자리에 오배달돼(drain_immediate_stops가 프레임 결과로 오소비)
        // 프레임을 안 돌리고도 interrupted+frozen로 오인되고 응답 스트림이 desync된다. step_instruction_count
        // 처럼 명령 전에 버퍼의 stale stop을 이벤트 큐로 걷어낸다.
        self.drain_buffered_stops()?;
        let previous = self.gdb.get_timeout()?;
        // 트레이싱 중이면 프레임마다 수십만 명령을 디스어셈+기록하므로 무트레이스 50ms/frame
        // 예산으론 타임아웃→지연 stop이 늦게 도착한다. 트레이스일 때 프레임당 예산을 크게 잡아
        // 지연 응답이 이 recv 창 안에서 매칭되게 한다.
        let timeout = Duration::from_millis(estimated_ms);
        self.gdb.set_timeout(timeout)?;
        let result = self.lua_cmd_allow_stop(name, Some(arg));
        let restore = self.gdb.set_timeout(previous);
        match (result, restore) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(err), Ok(())) => Err(err),
            (Ok(_), Err(err)) => Err(err),
            (Err(err), Err(_)) => Err(err),
        }
    }

    pub(super) fn frame_operation_budget_ms(&self) -> u64 {
        if self.tracing {
            5_000
        } else {
            50
        }
    }

    pub(super) fn max_sync_frame_count(&self) -> u64 {
        let deadline_ms = crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64;
        deadline_ms
            .saturating_sub(5_000)
            .checked_div(self.frame_operation_budget_ms())
            .unwrap_or(0)
            .min(crate::live::temporal::MAX_SYNC_ADVANCE_COUNT)
    }

    pub(super) fn lua_cmd_allow_stop(
        &mut self,
        name: &str,
        arg: Option<&str>,
    ) -> BridgeResult<Option<String>> {
        let mut payload = format!("qEmucap,{name}");
        if let Some(arg) = arg {
            payload.push(',');
            payload.push_str(&hex::encode(arg.as_bytes()));
        }
        let resp = self.send_cmd(&payload)?;
        if resp == "OK" {
            return self.drain_immediate_stops();
        }
        if is_stop_packet(&resp) {
            self.note_stop(resp.clone(), false);
            let _ = self.drain_immediate_stops()?;
            return Ok(Some(resp));
        }
        Err(BridgeError::Emulator(format!(
            "MAME Lua command {name} failed: {resp}"
        )))
    }

    pub(super) fn drain_stop(&mut self) -> BridgeResult<()> {
        if self.frozen {
            return Ok(());
        }
        if let Some(stop) = self.gdb.recv_nonblocking()? {
            if is_stop_packet(&stop) {
                self.note_stop(stop, false);
            }
        }
        Ok(())
    }

    /// 버퍼에 남은 stale async stop을 블로킹 없이 이벤트 큐로 걷어낸다. s처럼 응답 자체가
    /// stop인 명령(command_expects_stop)을 보내기 전에, 앞선 미소비 stop이 그 명령의 응답
    /// 자리에 오배달되는 걸 막는다. drain_stop은 frozen이면 조기 반환하지만 스텝 직전엔 frozen
    /// 중에도 직전 프레임 진행의 지연 stop이 남을 수 있어 별도로 항상 버퍼를 비운다.
    pub(super) fn drain_buffered_stops(&mut self) -> BridgeResult<()> {
        while let Some(pkt) = self.gdb.recv_nonblocking()? {
            if is_stop_packet(&pkt) {
                self.note_stop(pkt, false);
            } else {
                break;
            }
        }
        Ok(())
    }

    pub(super) fn drain_immediate_stops(&mut self) -> BridgeResult<Option<String>> {
        let mut first = None;
        for _ in 0..12 {
            match self.gdb.recv_nonblocking()? {
                Some(stop) if is_stop_packet(&stop) => {
                    // note_stop이 인터럽트 에코로 억제하면(true) 이 stop은 우리가 만든 에코일 뿐이므로
                    // 프레임 명령 결과(first)로 오소비하지 않는다.
                    let suppressed = self.note_stop(stop.clone(), false);
                    if !suppressed && first.is_none() {
                        first = Some(stop);
                    }
                }
                Some(_) => return Ok(first),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        Ok(first)
    }

    /// stop을 이벤트 큐에 넣는다. 단, 우리가 pause/interrupt로 주입한 인터럽트 에코 stop은 async
    /// 이벤트가 아니므로 큐에 넣으면 phantom stop으로 샌다 — interrupt()가 남긴 트레일링 stop 개수만큼
    /// 억제한다(pc98 인터럽트는 S05라 signal로 구분 불가 — NDS is_interrupt_stop(S02)에 상응하는 카운터
    /// 방식). 억제했으면 `true`를 반환해 호출부가 이 stop을 명령 결과로 오소비하지 않게 한다.
    pub(super) fn note_stop(&mut self, stop: String, enrich: bool) -> bool {
        self.frozen = true;
        if self.pending_interrupt_stops > 0 && is_stop_packet(&stop) {
            self.pending_interrupt_stops -= 1;
            return true;
        }
        let mut event = stop_event(&stop);
        if enrich {
            self.enrich_event(&mut event);
        }
        self.events.push(event);
        false
    }

    pub(super) fn drain_reset_event(&mut self) -> BridgeResult<bool> {
        let resp = self.lua_cmd_reply("pollreset", None)?;
        if resp == "NONE" {
            return Ok(false);
        }
        let Some(rest) = resp.strip_prefix("RESET:") else {
            return Err(BridgeError::Emulator(format!(
                "MAME reset poll failed: {resp}"
            )));
        };
        let (pc_hex, regs_hex) = rest.split_once('|').unwrap_or((rest, ""));
        let mut event = json!({ "type": "reset", "raw": resp });
        if let Some(pc) = little_hex_to_u64(pc_hex) {
            if let Some(obj) = event.as_object_mut() {
                obj.insert("pc".into(), json!(pc));
                obj.insert("address".into(), json!(pc));
            }
        } else if let Some(obj) = event.as_object_mut() {
            obj.insert("pc_error".into(), json!(pc_hex));
        }
        if !regs_hex.is_empty() {
            if let Some(obj) = event.as_object_mut() {
                obj.insert("regs".into(), state_from_regs_hex(regs_hex));
            }
        }
        self.events.push(event);
        Ok(true)
    }

    pub(super) fn enrich_event(&mut self, event: &mut Value) {
        if event.get("_pc98_enriched").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match event_type.as_str() {
            "stop" => self.enrich_stop_event(event),
            "register_break" => self.enrich_register_event(event),
            "breakpoint_hit" => self.enrich_breakpoint_event(event),
            _ => mark_event_enriched(event),
        }
    }

    pub(super) fn enrich_stop_event(&mut self, event: &mut Value) {
        self.ensure_event_regs(event);
        let mut pc_values = Vec::new();
        if let Some(regs) = event.get("regs") {
            for key in ["cpu.offset_pc", "cpu.pc"] {
                if let Some(value) = regs.get(key).and_then(Value::as_u64) {
                    pc_values.push(value);
                }
            }
            if event.get("pc").is_none() {
                if let Some(pc) = regs.get("cpu.pc").and_then(Value::as_u64) {
                    set_event_field(event, "pc", json!(pc));
                }
            }
        }
        for (id, bp) in &self.bps {
            if bp.kind == "exec" && bp.addr.is_some_and(|addr| pc_values.contains(&addr)) {
                set_event_field(event, "type", json!("breakpoint_hit"));
                set_event_field(event, "kind", json!("exec"));
                set_event_field(event, "address", json!(bp.addr.unwrap_or(0)));
                set_event_field(event, "id", json!(*id));
                set_event_field(event, "breakpoint_id", json!(*id));
                if bp.pause_on_hit {
                    self.frozen = true;
                }
                if !bp.snapshots.is_empty() && event.get("snapshot").is_none() {
                    let snapshots = bp.snapshots.clone();
                    match self.capture_snapshots(&snapshots) {
                        Ok(snapshot) => set_event_field(event, "snapshot", Value::Array(snapshot)),
                        Err(err) => {
                            set_event_field(event, "snapshot_error", json!(err.to_string()))
                        }
                    }
                }
                break;
            }
        }
        mark_event_enriched(event);
    }

    pub(super) fn enrich_register_event(&mut self, event: &mut Value) {
        let matched = self.find_regwatch_for_event(event);
        if let Some((id, bp)) = &matched {
            set_event_field(event, "id", json!(*id));
            set_event_field(event, "breakpoint_id", json!(*id));
            set_event_field(event, "register", json!(bp.register.clone()));
            set_event_field(event, "min", json!(bp.min));
            set_event_field(event, "max", json!(bp.max));
            if bp.pause_on_hit {
                self.frozen = true;
            }
        }
        self.ensure_event_regs(event);
        let pc = event
            .get("regs")
            .and_then(|regs| regs.get("cpu.pc"))
            .and_then(Value::as_u64);
        let value = matched.as_ref().and_then(|(_, bp)| {
            bp.state_key.as_ref().and_then(|state_key| {
                event
                    .get("regs")
                    .and_then(|regs| regs.get(state_key))
                    .and_then(Value::as_u64)
            })
        });
        if event.get("pc").is_none() {
            if let Some(pc) = pc {
                set_event_field(event, "pc", json!(pc));
            }
        }
        if let Some(value) = value {
            set_event_field(event, "value", json!(value));
        }
        mark_event_enriched(event);
    }

    pub(super) fn enrich_breakpoint_event(&mut self, event: &mut Value) {
        let matched = self.find_bp_for_event(event);
        if let Some((id, bp)) = &matched {
            set_event_field(event, "id", json!(*id));
            set_event_field(event, "breakpoint_id", json!(*id));
            if bp.pause_on_hit {
                self.frozen = true;
            }
        }
        self.ensure_event_regs(event);
        if let Some((_, bp)) = &matched {
            if !bp.snapshots.is_empty() && event.get("snapshot").is_none() {
                match self.capture_snapshots(&bp.snapshots) {
                    Ok(snapshot) => set_event_field(event, "snapshot", Value::Array(snapshot)),
                    Err(err) => set_event_field(event, "snapshot_error", json!(err.to_string())),
                }
            }
        }
        mark_event_enriched(event);
    }

    pub(super) fn ensure_event_regs(&mut self, event: &mut Value) {
        if event.get("regs").is_some() {
            return;
        }
        match self.read_regs_hex() {
            Ok(regs) => set_event_field(event, "regs", state_from_regs_hex(&regs)),
            Err(err) => set_event_field(event, "regs_error", json!(err.to_string())),
        }
    }

    pub(super) fn capture_snapshots(&mut self, specs: &[SnapshotSpec]) -> BridgeResult<Vec<Value>> {
        let mut out = Vec::new();
        for spec in specs {
            let bytes = self.read_region_bytes(&spec.memory_type, spec.address, spec.length)?;
            out.push(json!({
                "memory_type": spec.memory_type.clone(),
                "address": spec.address,
                "hex": hex::encode(bytes),
            }));
        }
        Ok(out)
    }

    pub(super) fn find_bp_for_event(&self, event: &Value) -> Option<(u64, Breakpoint)> {
        if event.get("type").and_then(Value::as_str) != Some("breakpoint_hit") {
            return None;
        }
        let event_kind = event.get("kind").and_then(Value::as_str)?;
        let backend_id = event.get("backend_id").and_then(Value::as_u64);
        if let Some(backend_id) = backend_id {
            for (id, bp) in &self.bps {
                if bp.backend_id == backend_id && bp.kind == event_kind {
                    return Some((*id, bp.clone()));
                }
            }
        }
        let event_addr = event.get("address").and_then(Value::as_u64)?;
        for (id, bp) in &self.bps {
            let Some(start) = bp.addr else {
                continue;
            };
            let end = start + bp.size.unwrap_or(1).saturating_sub(1);
            if bp.kind == event_kind && start <= event_addr && event_addr <= end {
                return Some((*id, bp.clone()));
            }
        }
        None
    }

    pub(super) fn find_regwatch_for_event(&self, event: &Value) -> Option<(u64, Breakpoint)> {
        if event.get("type").and_then(Value::as_str) != Some("register_break") {
            return None;
        }
        let backend_id = event.get("backend_id").and_then(Value::as_u64)?;
        for (id, bp) in &self.bps {
            if bp.kind == "reg" && bp.backend_id == backend_id {
                return Some((*id, bp.clone()));
            }
        }
        None
    }

    pub(super) fn read_trace_rows(&mut self) -> BridgeResult<Vec<Value>> {
        if self.tracing {
            let _ = self.lua_cmd("traceflush", None);
        }
        let Some(path) = &self.trace_path else {
            return Ok(Vec::new());
        };
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(TRACE_CAP * 4);
        let mut rows = Vec::new();
        for line in &lines[start..] {
            if let Some(row) = parse_trace_line(line) {
                rows.push(row);
            }
        }
        let start = rows.len().saturating_sub(TRACE_CAP);
        Ok(rows[start..].to_vec())
    }
}
