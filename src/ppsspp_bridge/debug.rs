use super::*;

impl<T: WsTransport> PpssppBridge<T> {
    /// `cpu.getAllRegs` → the `id:0` ("GPR") category flattened into `cpu.<name>: value` (MIPS GPRs
    /// plus the fork-appended synthetic `pc`/`hi`/`lo`, per `CPUCoreSubscriber.cpp`). FPU/VFPU
    /// categories are out of scope for v1 — PSP has one CPU context, so unlike the NDS bridge's
    /// per-core `{"cpu": ..., "state": {...}}` this returns just `{"state": {...}}`.
    pub(super) fn get_state(&mut self, _params: &Value) -> BridgeResult<Value> {
        Ok(json!({ "state": self.fetch_cpu_state()? }))
    }

    /// `cpu.getAllRegs` → the `id:0` ("GPR") category flattened into `cpu.<name>: value` (MIPS GPRs
    /// plus the fork-appended synthetic `pc`/`hi`/`lo`, per `CPUCoreSubscriber.cpp`). Shared by
    /// `get_state`, `step_instructions` (final state after stepping), and `poll_events` (regs on a
    /// stop event, which PPSSPP's `cpu.stepping` event never carries itself).
    pub(super) fn fetch_cpu_state(&mut self) -> BridgeResult<Value> {
        let result = self.ws.call("cpu.getAllRegs", json!({}))?;
        let categories = result
            .get("categories")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BridgeError::Emulator("cpu.getAllRegs: reply had no categories array".into())
            })?;
        let gpr = categories
            .iter()
            .find(|c| c.get("id").and_then(Value::as_u64) == Some(0))
            .ok_or_else(|| {
                BridgeError::Emulator("cpu.getAllRegs: reply had no id:0 (GPR) category".into())
            })?;
        let names = gpr
            .get("registerNames")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BridgeError::Emulator("cpu.getAllRegs: GPR category had no registerNames".into())
            })?;
        let values = gpr
            .get("uintValues")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BridgeError::Emulator("cpu.getAllRegs: GPR category had no uintValues".into())
            })?;
        let mut state = serde_json::Map::new();
        for (name, value) in names.iter().zip(values.iter()) {
            if let (Some(name), Some(value)) = (name.as_str(), value.as_u64()) {
                state.insert(format!("cpu.{name}"), json!(value));
            }
        }
        Ok(Value::Object(state))
    }

    /// `memory.disasm {address, count}` → `[{addr, bytes, text}]`. `address` is a raw absolute PSP
    /// address (e.g. from `get_state`'s `cpu.pc`) — unlike `read_memory` this does not add a
    /// `memory_type` base, matching the NDS bridge's `disassemble` convention. `bytes` re-emits
    /// PPSSPP's `encoding` (the instruction word, MIPS is little-endian) as little-endian in-memory
    /// hex; `text` joins the mnemonic (`name`) and its formatted operands (`params`).
    pub(super) fn disassemble(&mut self, params: &Value) -> BridgeResult<Value> {
        let addr = absolute_address(params)?;
        let count = optional_num(params, "count")?.unwrap_or(8).clamp(1, 4096);
        let result = self
            .ws
            .call("memory.disasm", json!({ "address": addr, "count": count }))?;
        let lines = result
            .get("lines")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BridgeError::Emulator("memory.disasm: reply had no lines array".into())
            })?;
        let mut instructions = Vec::with_capacity(lines.len());
        for line in lines {
            let addr = line.get("address").and_then(Value::as_u64).unwrap_or(0);
            let encoding = line.get("encoding").and_then(Value::as_u64).unwrap_or(0) as u32;
            let name = line.get("name").and_then(Value::as_str).unwrap_or("");
            let params = line.get("params").and_then(Value::as_str).unwrap_or("");
            let text = if params.is_empty() {
                name.to_string()
            } else {
                format!("{name} {params}")
            };
            instructions.push(json!({
                "addr": addr,
                "bytes": hex::encode(encoding.to_le_bytes()),
                "text": text,
            }));
        }
        Ok(json!({ "instructions": instructions }))
    }

    /// `cpu.status.stepping` — the real CPU-debugger halt indicator (see the `status()` note on why
    /// `game.status.paused` is not it).
    pub(super) fn cpu_is_stepping(&mut self) -> BridgeResult<bool> {
        let status = self.ws.call("cpu.status", json!({}))?;
        Ok(status
            .get("stepping")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// kind `exec` → `cpu.breakpoint.add {address, enabled, condition?}`; kind `read`/`write` →
    /// `memory.breakpoint.add {address, size, read/write, condition?}`. Address resolution follows the
    /// kind. An exec `address`/`start` is a raw absolute PSP address — a PC straight from `get_state`'s
    /// `cpu.pc` or `disassemble` — and `memory_type` is ignored (a PC is not a `main` offset and is not
    /// always inside `main` RAM; PPSSPP's cpu breakpoint takes an absolute address either way). A
    /// read/write `address`/`start` is symmetric with `read_memory`/`write_memory`: a `memory_type`
    /// region offset routed through the same `route_main_address` (→ `PSP_MAIN_RAM_BASE + offset`,
    /// out-of-range rejected), so the watchpoint lands where `read_memory` reads instead of at a raw
    /// low address that never fires. `pc_min`/`pc_max` (optional) compile
    /// into a PPSSPP `condition` expression
    /// (`"(pc >= ..) && (pc <= ..)"`); an explicit `condition` string is passed through verbatim and
    /// ANDed with any pc_min/pc_max clauses — PPSSPP parses/validates it and a bad expression comes
    /// back as an `emulator_error`, not a silently-ignored one. `pause_on_hit` (default true) maps to
    /// PPSSPP's `enabled` (a `false` value is honored — unlike the NDS/GDB bridge, PPSSPP natively
    /// supports a log-only, non-pausing breakpoint). `auto_savestate`/`snapshot`/value filters
    /// (`value`/`value_mask`/`value_len`) are unsupported and rejected
    /// rather than silently ignored.
    pub(super) fn set_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("exec")
            .to_string();
        if !matches!(kind.as_str(), "exec" | "read" | "write") {
            return Err(BridgeError::BadParams(format!(
                "psp bridge supports exec/read/write breakpoints (kind=exec|read|write); got kind={kind}"
            )));
        }
        if params.get("auto_savestate").and_then(Value::as_bool) == Some(true) {
            return Err(BridgeError::Unsupported(
                "psp bridge: auto_savestate is unsupported".into(),
            ));
        }
        if params
            .get("snapshot")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            return Err(BridgeError::Unsupported(
                "psp bridge: snapshot is unsupported — read_memory after the hit instead".into(),
            ));
        }
        for opt in ["value", "value_mask", "value_len"] {
            if params.get(opt).is_some() {
                return Err(BridgeError::Unsupported(format!(
                    "psp bridge: {opt} is unsupported — use pc_min/pc_max or a raw condition expression instead"
                )));
            }
        }
        // Watched span: 1 for an exec point (a single PC, no routing), the memory breakpoint's
        // `length` for read/write — used for both the routed bounds check and the memcheck size below.
        let route_len = if kind == "exec" {
            1
        } else {
            optional_num(params, "length")?.unwrap_or(1).max(1)
        };
        // Reject a range exec point (start != end): PPSSPP's cpu breakpoint is a single address, not
        // a span. `end` and `start` are compared as-is — both are raw absolute addresses (an exec
        // breakpoint does not route, unlike a read/write watchpoint), so the comparison is direct.
        if kind == "exec" {
            if let Some(end) = optional_num(params, "end")? {
                if end != region_offset(params)? {
                    return Err(BridgeError::Unsupported(
                        "psp bridge: range exec breakpoints are unsupported — single address only (start==end)".into(),
                    ));
                }
            }
        }
        let address = route_breakpoint_address(&kind, params, route_len)?;
        let pause_on_hit = params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let condition = breakpoint_condition(params)?;

        if kind == "exec" {
            // Range/end check already done above, in caller offset coordinates, before routing.
            // A same-address exec duplicate is accepted (not refused) and refcounted by
            // clear_breakpoint: an exec hit is attributed by PC == address, so duplicates are
            // semantically equivalent — unlike a memory read/write pair on one range, which the shared
            // memcheck cannot tell apart (refused below). This mirrors the memory branch, which also
            // accepts same-kind duplicates without comparing options; PPSSPP's single cpu breakpoint
            // holds the last-written enabled/condition, so a caller wanting distinct conditions uses
            // distinct addresses or clears the existing one first.
            let mut req = json!({ "address": address, "enabled": pause_on_hit });
            if let Some(cond) = &condition {
                req["condition"] = json!(cond);
            }
            self.ws.call("cpu.breakpoint.add", req)?;
            let id = self.next_bp;
            self.next_bp += 1;
            self.bps.insert(
                id,
                PpssppBreakpoint {
                    kind: kind.clone(),
                    address,
                    length: 1,
                    last_hits: 0,
                },
            );
            Ok(json!({ "id": id, "kind": kind, "address": address }))
        } else {
            let length = route_len;
            // PPSSPP keeps ONE memcheck per (address, size) with a single shared hit counter and no
            // per-access attribution. A read and a write breakpoint on the SAME range would collapse
            // into that one memcheck, so a hit could not be told apart between the two bridge ids
            // (enrich_stop would credit whichever sorts first). Refuse the ambiguous pair rather than
            // advertise a disambiguation the shared counter cannot provide. Same-kind duplicates are
            // fine (equivalent) and are refcounted by clear_breakpoint.
            if let Some(existing_kind) = self
                .bps
                .values()
                .find(|bp| {
                    bp.kind != "exec"
                        && bp.kind != kind
                        && bp.address == address
                        && bp.length == length
                })
                .map(|bp| bp.kind.clone())
            {
                return Err(BridgeError::BadParams(format!(
                    "psp bridge: a {existing_kind} breakpoint already watches {address:#x}+{length}; PPSSPP \
                     shares one memcheck and hit counter per (address, size), so a {kind} breakpoint on the \
                     same range could not be distinguished from it on a hit. Clear the existing one first, or \
                     watch a different address/size."
                )));
            }
            let mut req = json!({
                "address": address,
                "size": length,
                "enabled": pause_on_hit,
                "read": kind == "read",
                "write": kind == "write",
            });
            if let Some(cond) = &condition {
                req["condition"] = json!(cond);
            }
            self.ws.call("memory.breakpoint.add", req)?;
            // Seed `last_hits` from PPSSPP's ACTUAL current `numHits` for this memcheck, read back
            // from `memory.breakpoint.list`, not from a bridge-side sibling. `enrich_stop` attributes
            // a stop to the first memory breakpoint whose `hits` grew since its `last_hits`, so a
            // mismatch between `last_hits` and PPSSPP's live counter either fabricates a hit (seed too
            // low) or swallows a real one (seed too high). PPSSPP keeps ONE memcheck per address/size:
            // it PRESERVES `numHits` when a duplicate re-adds an existing memcheck, but RESETS it to 0
            // when the memcheck is removed and later recreated (`Core/Debugger/Breakpoints.cpp`). A
            // sibling's `last_hits` is stale in the clear-then-re-add case — clearing one duplicate
            // removes the shared memcheck (numHits→gone), so a still-tracked sibling holds 1 while a
            // freshly re-added memcheck starts at 0; inheriting that 1 would miss the first real hit.
            // Reading the live counter is the only value that matches what `enrich_stop` compares
            // against, and it also covers the plain duplicate case (list returns the preserved count).
            // If the read-back fails or the entry is absent, fall back to 0 — the bp was still added,
            // and 0 favors reporting a possibly-stale hit over silently missing a real one.
            let last_hits = self
                .ws
                .call("memory.breakpoint.list", json!({}))
                .ok()
                .and_then(|list| {
                    list.get("breakpoints")
                        .and_then(Value::as_array)
                        .and_then(|entries| {
                            entries.iter().find_map(|e| {
                                let matches = e.get("address").and_then(Value::as_u64)
                                    == Some(address)
                                    && e.get("size").and_then(Value::as_u64) == Some(length);
                                matches
                                    .then(|| e.get("hits").and_then(Value::as_u64))
                                    .flatten()
                            })
                        })
                })
                .unwrap_or(0);
            let id = self.next_bp;
            self.next_bp += 1;
            self.bps.insert(
                id,
                PpssppBreakpoint {
                    kind: kind.clone(),
                    address,
                    length,
                    last_hits,
                },
            );
            Ok(json!({ "id": id, "kind": kind, "address": address, "length": length }))
        }
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        let id = required_num(params, "id")?;
        let bp = self
            .bps
            .get(&id)
            .cloned()
            .ok_or_else(|| BridgeError::BadParams(format!("unknown breakpoint id: {id}")))?;
        if bp.kind == "exec" {
            // PPSSPP keeps ONE cpu breakpoint per address; several bridge ids may map to it (a
            // duplicate set_breakpoint, or a retry after a lost response). Only tear it down when THIS
            // id is the last bridge exec breakpoint on that address — otherwise removing it would
            // silently disarm the still-tracked survivor (it would stay in list_breakpoints but never
            // halt again). exec breakpoints are single-address (no size), so the survivor check is by
            // address alone — the same refcount discipline as the memory branch below.
            let survivor_shares_address = self
                .bps
                .iter()
                .any(|(&other, ob)| other != id && ob.kind == "exec" && ob.address == bp.address);
            if !survivor_shares_address {
                self.ws
                    .call("cpu.breakpoint.remove", json!({ "address": bp.address }))?;
            }
        } else {
            // PPSSPP keeps ONE memcheck per (address, size); several bridge ids may share it (same-kind
            // duplicates). Only tear the memcheck down when THIS id was the last bridge breakpoint on
            // that range — otherwise removing it would silently disarm the still-tracked siblings (they
            // would stay in list_breakpoints but never stop again). Cross-kind coexistence is refused
            // at add time, so any survivor shares this range's access mode: leaving the memcheck as-is
            // is exactly the union of the remaining read/write modes, so no re-add is needed.
            let survivor_shares_range = self.bps.iter().any(|(&other, ob)| {
                other != id
                    && ob.kind != "exec"
                    && ob.address == bp.address
                    && ob.length == bp.length
            });
            if !survivor_shares_range {
                self.ws.call(
                    "memory.breakpoint.remove",
                    json!({ "address": bp.address, "size": bp.length }),
                )?;
            }
        }
        self.bps.remove(&id);
        Ok(json!({ "cleared": id }))
    }

    pub(super) fn list_breakpoints(&self) -> BridgeResult<Value> {
        let mut rows = Vec::new();
        for (id, bp) in &self.bps {
            let mut row = json!({ "id": id, "kind": bp.kind, "address": bp.address });
            if bp.kind != "exec" {
                row["length"] = json!(bp.length);
            }
            rows.push(row);
        }
        Ok(json!({ "breakpoints": rows }))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> BridgeResult<Value> {
        let mut cleared = Vec::new();
        for id in self.bps.keys().copied().collect::<Vec<_>>() {
            if self.clear_breakpoint(&json!({ "id": id })).is_ok() {
                cleared.push(id);
            }
        }
        Ok(json!({ "cleared": cleared }))
    }

    /// `cpu.stepping` — no-op (idempotent) if the CPU is already stepping, since PPSSPP's own
    /// `WebSocketCPUStepping` silently does nothing when already stepping (no `Core_Break`, no
    /// state-change, so no ack event ever arrives) — calling it unconditionally would hang the
    /// bridge waiting for an ack that never comes.
    pub(super) fn pause(&mut self, _params: &Value) -> BridgeResult<Value> {
        if !self.cpu_is_stepping()? {
            self.ws.call("cpu.stepping", json!({}))?;
        }
        Ok(json!({ "state": "frozen" }))
    }

    /// `cpu.resume` — no-op (idempotent) if the CPU is already running, mirroring `pause` (PPSSPP's
    /// `WebSocketCPUResume` fails with "CPU not stepping" when called on a running CPU).
    pub(super) fn resume(&mut self, _params: &Value) -> BridgeResult<Value> {
        if self.cpu_is_stepping()? {
            self.ws.call("cpu.resume", json!({}))?;
        }
        Ok(json!({ "state": "running" }))
    }

    /// Wire method `step` retained for older hosts and direct calls. The current public MCP routes
    /// instruction stepping to the `step_instructions` wire method. PPSSPP has no frame-advance
    /// primitive, so a frame-step request is rejected rather than silently reinterpreted as an
    /// instruction count (which would make a 60-frame advance step 60 instructions and derail
    /// freeze-step/tap).
    /// `unit:"instructions"` (and the lenient bare `step` with no unit and no `frames`) route to the
    /// same `cpu.stepInto` logic as the `step_instructions` wire method.
    ///
    /// Advertisement: this wire method is *not* in `METHODS` (so the MCP's `has("step")` frame-step
    /// composites — `tap`/`hold_until` — stay correctly disabled on PSP, since they
    /// drive frame `step` which PPSSPP cannot do), and it is *not* claimed as "planned" either
    /// (frame-step is a permanent gap, not a pending feature). The stepping that does work is
    /// advertised as `step_instructions` in `METHODS` plus `step_units == ["instructions"]`.
    /// The dispatch arm stays for wire compatibility and lets a frame-step request return a precise
    /// `unsupported` rather than `unknown_method`.
    pub(super) fn step(&mut self, params: &Value) -> BridgeResult<Value> {
        match params.get("unit").and_then(Value::as_str) {
            Some("instructions") => {}
            Some(other) => {
                return Err(BridgeError::Unsupported(format!(
                    "step unit={other} (psp bridge steps by instructions only — PPSSPP has no frame-advance)"
                )));
            }
            None => {
                if params.get("frames").is_some() {
                    return Err(BridgeError::Unsupported(
                        "psp bridge: frame step unsupported — PPSSPP has no frame-advance primitive. \
                         Use step(unit=instructions) instead."
                            .into(),
                    ));
                }
            }
        }
        self.step_instructions(params)
    }

    /// `cpu.stepInto`, called `count` times (PPSSPP has no step-count parameter — see
    /// `SteppingSubscriber.cpp`). Ensures the CPU is stepping first: `cpu.stepInto` on a *running*
    /// CPU just pauses it without executing anything (`WebSocketSteppingState::Into`'s
    /// `if (!Core_IsStepping()) { Core_Break(...); return; }` branch) — real single-instruction
    /// stepping only happens once already stepping. Each `cpu.stepInto` acks via a `cpu.stepping`
    /// event (a different name — see the module doc), so this rides `call_and_wait_for`, not `call`.
    /// The final `pc`/`state` come from a fresh `cpu.getAllRegs`, not the ack event's own `pc` field
    /// (undocumented-accurate only "while stepping", and this bridge does not depend on its
    /// precision).
    pub(super) fn step_instructions(&mut self, params: &Value) -> BridgeResult<Value> {
        let count = step_count(params)?;
        if count > crate::live::temporal::MAX_SYNC_ADVANCE_COUNT {
            return Err(BridgeError::BadParams(format!(
                "instruction count {count} exceeds the synchronous cap {}; split the advance and verify each terminal response",
                crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
            )));
        }
        if !self.cpu_is_stepping()? {
            self.ws.call("cpu.stepping", json!({}))?;
        }
        let deadline = std::time::Instant::now() + crate::live::temporal::MAX_SYNC_OPERATION_TIME;
        for completed in 0..count {
            if std::time::Instant::now() >= deadline {
                return Err(BridgeError::Emulator(format!(
                    "instruction step deadline exceeded after {completed} of {count}; the CPU remains halted"
                )));
            }
            self.ws
                .call_and_wait_for("cpu.stepInto", json!({}), "cpu.stepping")?;
        }
        let state = self.fetch_cpu_state()?;
        let pc = state.get("cpu.pc").and_then(Value::as_u64);
        Ok(json!({
            "status": "completed",
            "unit": "instructions",
            "count": count,
            "pc": pc,
            "state": state,
        }))
    }

    /// Drain PPSSPP's spontaneous events, keep the `cpu.stepping` stops (a breakpoint hit or a
    /// stepping-request completion — PPSSPP does not distinguish them at the wire, see the module
    /// doc), and normalize each into `{type, pc, ticks, regs, [breakpoint_id, kind, address, id]}`.
    /// Non-stop events (`cpu.resume`, `input.*`, log lines, ...) are dropped, not queued — mirroring
    /// the NDS bridge dropping its own SIGINT stops. `breakpoint_id` filters like the NDS bridge: a
    /// non-matching event is held in `self.events` for a later poll instead of being dropped.
    pub(super) fn poll_events(&mut self, params: &Value) -> BridgeResult<Value> {
        // Validate the `breakpoint_id` filter BEFORE draining the transport (which is destructive) or
        // touching `self.events`: a malformed filter must fail without consuming — and thereby losing
        // forever — already-buffered breakpoint-hit events.
        let filter_id = optional_num(params, "breakpoint_id")?;
        let raw = self.ws.drain_events();
        let mut fresh = Vec::new();
        // Count the spontaneous events this drain actually discards (log lines, `cpu.resume`,
        // `input.*`, ... — anything that is not a `cpu.stepping` stop), rather than reporting a
        // hardcoded 0. A `breakpoint_id` filter below does NOT drop — it holds non-matching stops
        // in `self.events` for a later poll — so those are not counted here.
        let mut dropped = 0u64;
        for event in raw {
            if event.get("event").and_then(Value::as_str) != Some("cpu.stepping") {
                dropped += 1;
                continue;
            }
            fresh.push(self.enrich_stop(event)?);
        }
        let mut all = std::mem::take(&mut self.events);
        all.append(&mut fresh);

        let mut out = Vec::new();
        for event in all {
            let matches_filter = match filter_id {
                Some(fid) => event.get("breakpoint_id").and_then(Value::as_u64) == Some(fid),
                None => true,
            };
            if matches_filter {
                out.push(event);
            } else {
                self.events.push(event);
            }
        }
        Ok(json!({ "events": out, "dropped": dropped }))
    }

    /// Build the base `{type:"stop", pc, ticks, regs}` shape from a raw `cpu.stepping` event, then
    /// classify it as a breakpoint hit if possible. An exec breakpoint is matched directly (the
    /// event's `pc` equals the breakpoint address). A memory breakpoint cannot be matched that way
    /// — the event's `pc` is the accessing instruction's address, not the watched address — so it is
    /// attributed via a `memory.breakpoint.list` hit-count delta: the first tracked memory
    /// breakpoint whose `hits` grew since the last check is reported as the source. Simultaneous
    /// memory breakpoint hits are a known best-effort limitation (only one is attributed per event).
    pub(super) fn enrich_stop(&mut self, event: Value) -> BridgeResult<Value> {
        let pc = event.get("pc").and_then(Value::as_u64);
        let ticks = event.get("ticks").cloned().unwrap_or(Value::Null);
        let mut out = json!({ "type": "stop", "pc": pc, "ticks": ticks });
        match self.fetch_cpu_state() {
            Ok(state) => out["regs"] = state,
            Err(err) => out["regs_error"] = json!(err.to_string()),
        }

        if let Some(pc) = pc {
            if let Some((&id, _)) = self
                .bps
                .iter()
                .find(|(_, bp)| bp.kind == "exec" && bp.address == pc)
            {
                mark_breakpoint_hit(&mut out, id, "exec", pc);
                return Ok(out);
            }
        }

        if self.bps.values().any(|bp| bp.kind != "exec") {
            if let Ok(list) = self.ws.call("memory.breakpoint.list", json!({})) {
                if let Some(entries) = list.get("breakpoints").and_then(Value::as_array) {
                    let mut hit = None;
                    for (&id, bp) in self.bps.iter_mut() {
                        if bp.kind == "exec" {
                            continue;
                        }
                        let Some(entry) = entries.iter().find(|e| {
                            e.get("address").and_then(Value::as_u64) == Some(bp.address)
                                && e.get("size").and_then(Value::as_u64) == Some(bp.length)
                        }) else {
                            continue;
                        };
                        let hits = entry.get("hits").and_then(Value::as_u64).unwrap_or(0);
                        if hits > bp.last_hits {
                            bp.last_hits = hits;
                            if hit.is_none() {
                                hit = Some((id, bp.kind.clone(), bp.address));
                            }
                        }
                    }
                    if let Some((id, kind, address)) = hit {
                        mark_breakpoint_hit(&mut out, id, &kind, address);
                    }
                }
            }
        }
        Ok(out)
    }
}
