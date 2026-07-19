use super::*;

impl<G: GdbTransport> NdsBridge<G> {
    pub(super) fn set_breakpoint(&mut self, params: &Value) -> NdsResult<Value> {
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("exec")
            .to_string();
        if kind != "exec" {
            return Err(NdsBridgeError::BadParams(format!(
                "nds bridge supports exec breakpoints (kind=exec); got kind={kind}"
            )));
        }
        // NDS GDB-RSP는 단일 주소 exec BP만이다(Z0/Z1 @ addr, 4바이트). 코어 BP 페이로드의 범위(end)·pc/value
        // 필터·비-pausing·auto_savestate·snapshot은 브리지가 지원하지 않는다. 이들을 조용히 무시하면(성공인데
        // start만 걸리거나 GDB가 무조건 halt) 호출자가 오해하므로, 지원 서브셋만 통과시키고 나머지는
        // 거부한다.
        if let (Some(s), Some(e)) = (optional_num(params, "start")?, optional_num(params, "end")?) {
            if e != s {
                return Err(NdsBridgeError::Unsupported(
                    "nds bridge: 범위 BP 미지원 — 단일 주소 exec만(start==end)".into(),
                ));
            }
        }
        for opt in ["pc_min", "pc_max", "value"] {
            if optional_num(params, opt)?.is_some() {
                return Err(NdsBridgeError::Unsupported(format!(
                    "nds bridge: {opt} 미지원 — 단일 주소 exec BP만(GDB Z0/Z1)"
                )));
            }
        }
        if params.get("pause_on_hit").and_then(Value::as_bool) == Some(false) {
            return Err(NdsBridgeError::Unsupported(
                "nds bridge: pause_on_hit=false 미지원 — GDB BP는 항상 코어를 halt한다".into(),
            ));
        }
        if params.get("auto_savestate").and_then(Value::as_bool) == Some(true) {
            return Err(NdsBridgeError::Unsupported(
                "nds bridge: auto_savestate 미지원".into(),
            ));
        }
        if params
            .get("snapshot")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            return Err(NdsBridgeError::Unsupported(
                "nds bridge: snapshot 미지원 — 히트 후 read_memory로 직접 캡처하라".into(),
            ));
        }
        let hardware = params
            .get("hardware")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let ztype = if hardware { "1" } else { "0" };
        let (cpu, addr, _region) = route(params, 4)?;
        let resp = self
            .cpu_mut(cpu)?
            .send_cmd(&format!("Z{ztype},{addr:x},4"))?;
        if resp != "OK" {
            return Err(NdsBridgeError::Emulator(format!(
                "GDB breakpoint set failed: {resp}"
            )));
        }
        let id = self.next_bp;
        self.next_bp += 1;
        self.bps.insert(
            id,
            NdsBreakpoint {
                cpu,
                kind,
                addr,
                ztype,
            },
        );
        Ok(json!({ "id": id, "cpu": cpu.as_str(), "address": addr, "hardware": hardware }))
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> NdsResult<Value> {
        let id = required_num(params, "id")?;
        let bp = self
            .bps
            .get(&id)
            .cloned()
            .ok_or_else(|| NdsBridgeError::BadParams(format!("unknown breakpoint id: {id}")))?;
        let resp = self
            .cpu_mut(bp.cpu)?
            .send_cmd(&format!("z{},{:x},4", bp.ztype, bp.addr))?;
        if resp != "OK" && resp != "E00" {
            return Err(NdsBridgeError::Emulator(format!(
                "GDB breakpoint clear failed: {resp}"
            )));
        }
        self.bps.remove(&id);
        Ok(json!({ "cleared": id }))
    }

    pub(super) fn list_breakpoints(&self) -> NdsResult<Value> {
        let mut rows = Vec::new();
        for (id, bp) in &self.bps {
            rows.push(json!({
                "id": id,
                "cpu": bp.cpu.as_str(),
                "kind": bp.kind.clone(),
                "address": bp.addr,
                "hardware": bp.ztype == "1",
            }));
        }
        Ok(json!({ "breakpoints": rows }))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> NdsResult<Value> {
        let mut cleared = Vec::new();
        for id in self.bps.keys().copied().collect::<Vec<_>>() {
            if self.clear_breakpoint(&json!({ "id": id })).is_ok() {
                cleared.push(id);
            }
        }
        Ok(json!({ "cleared": cleared }))
    }

    pub(super) fn pause(&mut self, params: &Value) -> NdsResult<Value> {
        self.drain_scheduler_stops()?;
        let target = self.pause_target(params)?;
        if !self.primary_frozen() {
            self.cpu_mut(target)?.pause()?;
        }
        self.set_scheduler_frozen(true);
        let mut states = serde_json::Map::new();
        states.insert("arm9".into(), json!("frozen"));
        if self.arm7.is_some() {
            states.insert("arm7".into(), json!("frozen"));
        }
        Ok(json!({ "state": "frozen", "cpus": Value::Object(states) }))
    }

    pub(super) fn resume(&mut self, params: &Value) -> NdsResult<Value> {
        self.drain_scheduler_stops()?;
        let target = self.resume_target(params)?;
        self.cpu_mut(target)?.resume()?;
        self.set_scheduler_frozen(false);
        let mut states = serde_json::Map::new();
        states.insert("arm9".into(), json!("running"));
        if self.arm7.is_some() {
            states.insert("arm7".into(), json!("running"));
        }
        Ok(json!({ "state": "running", "cpus": Value::Object(states) }))
    }
}
