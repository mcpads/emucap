//! R4300 breakpoint, event, reset, and disassembly support.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::ptr;

use serde_json::{json, Value};

use super::*;

pub(super) const BKP_ENABLED: u32 = 0x01;
pub(super) const BKP_READ: u32 = 0x02;
pub(super) const BKP_WRITE: u32 = 0x04;
pub(super) const BKP_EXEC: u32 = 0x08;

const BKP_CMD_ADD_STRUCT: c_int = 2;
const BKP_CMD_REMOVE_IDX: c_int = 5;
const DBG_NUM_BREAKPOINTS: c_int = 3;
const MAX_BREAKPOINTS: usize = 128;
const MAX_SNAPSHOTS: usize = 16;
const MAX_SNAPSHOT_BYTES: u64 = 0x4000;
const MAX_DISASSEMBLY_COUNT: u64 = 64;
const N64_ADDRESS_SPACE_SIZE: u64 = u32::MAX as u64 + 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeBreakpoint {
    pub(super) address: u32,
    pub(super) endaddr: u32,
    pub(super) flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SnapshotSpec {
    pub(super) offset: u64,
    pub(super) length: u64,
}

#[derive(Clone, Debug)]
pub(super) struct BreakpointSpec {
    pub(super) kind: String,
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) native: NativeBreakpoint,
    pub(super) snapshots: Vec<SnapshotSpec>,
}

#[derive(Clone, Debug)]
pub(super) enum ArmState {
    Armed,
    Failed(String),
}

#[derive(Clone, Debug)]
pub(super) struct PublicBreakpoint {
    pub(super) spec: BreakpointSpec,
    pub(super) native_slot: u32,
    pub(super) arm_state: ArmState,
}

pub(super) fn breakpoint_kinds() -> Value {
    json!([
        {
            "kind":"exec",
            "range_mode":"exact",
            "range_unit":"address",
            "memory_type_used":false,
            "pause_on_hit":[true],
            "snapshot":true,
            "snapshot_timing":"pre_instruction",
        },
        {
            "kind":"read",
            "range_mode":"inclusive",
            "range_unit":"address",
            "memory_type_used":true,
            "pause_on_hit":[true],
            "snapshot":true,
            "snapshot_timing":"pre_access",
        },
        {
            "kind":"write",
            "range_mode":"inclusive",
            "range_unit":"address",
            "memory_type_used":true,
            "pause_on_hit":[true],
            "snapshot":true,
            "snapshot_timing":"pre_access",
        }
    ])
}

pub(super) fn breakpoint_spec(params: &Value) -> N64Result<BreakpointSpec> {
    for key in [
        "condition",
        "value",
        "value_mask",
        "value_len",
        "pc_min",
        "pc_max",
    ] {
        if params.get(key).is_some() {
            return Err(N64Error::BadParams(format!(
                "N64 breakpoint option {key} is not supported"
            )));
        }
    }
    if params
        .get("pause_on_hit")
        .and_then(Value::as_bool)
        .is_some_and(|pause| !pause)
    {
        return Err(N64Error::BadParams(
            "N64 breakpoints require pause_on_hit=true".into(),
        ));
    }
    let kind = params.get("kind").and_then(Value::as_str).unwrap_or("exec");
    let start = required_num(params, "start")?;
    let end = optional_num(params, "end")?.unwrap_or(start);
    if end < start {
        return Err(N64Error::BadParams("breakpoint end is below start".into()));
    }
    let (native_start, native_end, flag) = match kind {
        "exec" => {
            if end != start {
                return Err(N64Error::BadParams(
                    "N64 exec breakpoints require an exact address".into(),
                ));
            }
            if params
                .get("memory_type")
                .and_then(Value::as_str)
                .is_some_and(|memory_type| memory_type != "cpu")
            {
                return Err(N64Error::BadParams(
                    "N64 exec breakpoints use an absolute R4300 address".into(),
                ));
            }
            let address = u32::try_from(start)
                .map_err(|_| N64Error::BadParams("N64 exec address exceeds u32".into()))?;
            (address, address, BKP_EXEC)
        }
        "read" | "write" => {
            let memory_type = params
                .get("memory_type")
                .and_then(Value::as_str)
                .unwrap_or("rdram");
            if memory_type != "rdram" {
                return Err(N64Error::BadParams(
                    "N64 read/write breakpoints currently support memory_type=rdram only".into(),
                ));
            }
            if end >= RDRAM_SIZE {
                return Err(N64Error::BadParams(format!(
                    "N64 RDRAM breakpoint exceeds {RDRAM_SIZE:#x}"
                )));
            }
            (
                start as u32,
                end as u32,
                if kind == "read" { BKP_READ } else { BKP_WRITE },
            )
        }
        other => {
            return Err(N64Error::BadParams(format!(
                "unsupported N64 breakpoint kind: {other}"
            )))
        }
    };
    Ok(BreakpointSpec {
        kind: kind.into(),
        start,
        end,
        native: NativeBreakpoint {
            address: native_start,
            endaddr: native_end,
            flags: BKP_ENABLED | flag,
        },
        snapshots: snapshot_specs(params.get("snapshot"))?,
    })
}

fn snapshot_specs(value: Option<&Value>) -> N64Result<Vec<SnapshotSpec>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| N64Error::BadParams("snapshot must be a list".into()))?;
    if entries.len() > MAX_SNAPSHOTS {
        return Err(N64Error::BadParams(format!(
            "snapshot accepts at most {MAX_SNAPSHOTS} ranges"
        )));
    }
    let mut total = 0_u64;
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        let raw = entry.as_str().ok_or_else(|| {
            N64Error::BadParams("snapshot entries must be memory_type:address:length".into())
        })?;
        let parts = raw.split(':').collect::<Vec<_>>();
        if parts.len() != 3 || parts[0] != "rdram" {
            return Err(N64Error::BadParams(format!(
                "invalid N64 snapshot range: {raw}"
            )));
        }
        let offset = parse_snapshot_number(parts[1])
            .ok_or_else(|| N64Error::BadParams(format!("invalid snapshot address: {raw}")))?;
        let length = parse_snapshot_number(parts[2])
            .filter(|length| *length > 0)
            .ok_or_else(|| N64Error::BadParams(format!("invalid snapshot length: {raw}")))?;
        if !matches!(offset.checked_add(length), Some(end) if end <= RDRAM_SIZE) {
            return Err(N64Error::BadParams(format!(
                "N64 snapshot crosses the RDRAM boundary: {raw}"
            )));
        }
        total = total
            .checked_add(length)
            .ok_or_else(|| N64Error::BadParams("snapshot byte total overflow".into()))?;
        if total > MAX_SNAPSHOT_BYTES {
            return Err(N64Error::BadParams(format!(
                "snapshot byte total exceeds {MAX_SNAPSHOT_BYTES:#x}"
            )));
        }
        result.push(SnapshotSpec { offset, length });
    }
    Ok(result)
}

fn parse_snapshot_number(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix('$')) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        raw.parse().ok()
    }
}

pub(super) fn ranges_overlap(left: &BreakpointSpec, right: &BreakpointSpec) -> bool {
    left.kind == right.kind
        && left.native.address <= right.native.endaddr
        && right.native.address <= left.native.endaddr
}

pub(super) fn slot_after_clear(slot: u32, removed: u32) -> Option<u32> {
    if slot == removed {
        None
    } else if slot > removed {
        Some(slot - 1)
    } else {
        Some(slot)
    }
}

pub(super) fn trigger_matches(spec: &BreakpointSpec, flags: u32, accessed: u32, pc: u32) -> bool {
    let address = match spec.kind.as_str() {
        "exec" if flags & BKP_EXEC != 0 => pc,
        "read" if flags & BKP_READ != 0 => accessed,
        "write" if flags & BKP_WRITE != 0 => accessed,
        _ => return false,
    };
    (spec.native.address..=spec.native.endaddr).contains(&address)
}

pub(super) fn disassembly_range(params: &Value, count: u64) -> N64Result<(u64, u64)> {
    let address = required_num(params, "address")?;
    let length = count
        .checked_mul(4)
        .ok_or_else(|| N64Error::BadParams("N64 disassembly length overflow".into()))?;
    if !matches!(
        address.checked_add(length),
        Some(end) if end <= N64_ADDRESS_SPACE_SIZE
    ) {
        return Err(N64Error::BadParams(
            "N64 disassembly crosses the 32-bit virtual address boundary".into(),
        ));
    }
    Ok((address, length))
}

impl Mupen64PlusHost {
    pub(super) fn run_frames(&mut self, params: &Value) -> N64Result<Value> {
        require_r4300(params)?;
        let count = optional_num(params, "n")?
            .or(optional_num(params, "count")?)
            .or(optional_num(params, "frames")?)
            .unwrap_or(1);
        if !(1..=MAX_STEP_COUNT).contains(&count) {
            return Err(N64Error::BadParams(format!(
                "N64 run_frames count must be in 1..={MAX_STEP_COUNT}"
            )));
        }
        let started_frozen = self.is_frozen_boundary();
        if !started_frozen {
            let paused = self.pause(&json!({"cpu":"r4300"}))?;
            if paused.get("reason").and_then(Value::as_str) == Some("breakpoint") {
                return Ok(json!({
                    "status":"interrupted",
                    "reason":"breakpoint",
                    "breakpoint_id":paused["breakpoint_id"],
                    "event":paused["event"],
                    "count":count,
                    "completed":0,
                    "state":"frozen",
                }));
            }
        }
        let advanced = self.step(&json!({"frames":count, "cpu":"r4300"}))?;
        if advanced.get("status").and_then(Value::as_str) == Some("interrupted") {
            return Ok(advanced);
        }
        self.resume(&json!({"cpu":"r4300"}))?;
        Ok(json!({
            "status":"completed",
            "unit":"frames",
            "count":count,
            "completed":count,
            "started_state":if started_frozen {"frozen"} else {"running"},
            "state":"running",
            "at_least":count,
            "exact":false,
            "controlled_segment_exact":true,
            "frame_before":advanced["frame_before"],
            "controlled_segment_end_frame":advanced["frame"],
        }))
    }

    pub(super) fn set_breakpoint(&mut self, params: &Value) -> N64Result<Value> {
        self.require_connected()?;
        if self.breakpoints.len() >= MAX_BREAKPOINTS {
            return Err(N64Error::BadParams(format!(
                "N64 supports at most {MAX_BREAKPOINTS} breakpoints"
            )));
        }
        let spec = breakpoint_spec(params)?;
        if self
            .breakpoints
            .values()
            .any(|breakpoint| ranges_overlap(&breakpoint.spec, &spec))
        {
            return Err(N64Error::BadParams(
                "N64 trigger identity requires non-overlapping ranges of the same kind".into(),
            ));
        }
        let native_before = unsafe { (self.api.debug_get_state)(DBG_NUM_BREAKPOINTS) };
        let mut native = spec.native;
        let slot = unsafe {
            (self.api.debug_breakpoint_command)(
                BKP_CMD_ADD_STRUCT,
                0,
                &mut native as *mut NativeBreakpoint,
            )
        };
        if slot < 0 {
            return Err(N64Error::BadState(
                "Mupen64Plus rejected the breakpoint".into(),
            ));
        }
        let native_after = unsafe { (self.api.debug_get_state)(DBG_NUM_BREAKPOINTS) };
        if slot != native_before || native_after != native_before + 1 {
            let _ = unsafe {
                (self.api.debug_breakpoint_command)(
                    BKP_CMD_REMOVE_IDX,
                    slot as u32,
                    ptr::null_mut(),
                )
            };
            self.fail_debugger_surface(format!(
                "native arm count mismatch: slot={slot}, before={native_before}, after={native_after}"
            ));
            return Err(N64Error::BadState(
                "N64 native breakpoint arm did not complete coherently".into(),
            ));
        }
        let id = self.next_breakpoint_id;
        self.next_breakpoint_id = self
            .next_breakpoint_id
            .checked_add(1)
            .ok_or_else(|| N64Error::BadState("N64 breakpoint id space is exhausted".into()))?;
        self.breakpoints.insert(
            id,
            PublicBreakpoint {
                spec,
                native_slot: slot as u32,
                arm_state: ArmState::Armed,
            },
        );
        if let Err(error) = self.verify_native_breakpoints() {
            self.breakpoints.remove(&id);
            let _ = unsafe {
                (self.api.debug_breakpoint_command)(
                    BKP_CMD_REMOVE_IDX,
                    slot as u32,
                    ptr::null_mut(),
                )
            };
            self.fail_debugger_surface(error.to_string());
            return Err(error);
        }
        Ok(json!({"id":id, "set":true, "native_slot":slot, "arm_state":"armed"}))
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> N64Result<Value> {
        let id = required_num(params, "id")?;
        let breakpoint = self
            .breakpoints
            .get(&id)
            .cloned()
            .ok_or_else(|| N64Error::BadParams(format!("unknown N64 breakpoint id: {id}")))?;
        let before = unsafe { (self.api.debug_get_state)(DBG_NUM_BREAKPOINTS) };
        let result = unsafe {
            (self.api.debug_breakpoint_command)(
                BKP_CMD_REMOVE_IDX,
                breakpoint.native_slot,
                ptr::null_mut(),
            )
        };
        let after = unsafe { (self.api.debug_get_state)(DBG_NUM_BREAKPOINTS) };
        if result != 0 || before <= 0 || after != before - 1 {
            self.fail_debugger_surface(format!(
                "native clear count mismatch: result={result}, before={before}, after={after}"
            ));
            return Err(N64Error::BadState(
                "N64 native breakpoint clear did not complete coherently".into(),
            ));
        }
        self.breakpoints.remove(&id);
        for remaining in self.breakpoints.values_mut() {
            remaining.native_slot = slot_after_clear(remaining.native_slot, breakpoint.native_slot)
                .ok_or_else(|| {
                    N64Error::BadState("N64 breakpoint slot identity collided".into())
                })?;
        }
        if let Err(error) = self.verify_native_breakpoints() {
            self.fail_debugger_surface(error.to_string());
            return Err(error);
        }
        Ok(json!({"cleared":id}))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> N64Result<Value> {
        let ids = self.breakpoints.keys().copied().collect::<Vec<_>>();
        let mut cleared = Vec::new();
        for id in ids {
            self.clear_breakpoint(&json!({"id":id}))?;
            cleared.push(id);
        }
        Ok(json!({"cleared":cleared}))
    }

    pub(super) fn list_breakpoints(&self) -> N64Result<Value> {
        let breakpoints = self
            .breakpoints
            .iter()
            .map(|(id, breakpoint)| {
                let (arm_state, arm_error) = match &breakpoint.arm_state {
                    ArmState::Armed => ("armed", None),
                    ArmState::Failed(error) => ("failed", Some(error.as_str())),
                };
                json!({
                    "id":id,
                    "kind":breakpoint.spec.kind,
                    "start":breakpoint.spec.start,
                    "end":breakpoint.spec.end,
                    "native_slot":breakpoint.native_slot,
                    "arm_state":arm_state,
                    "arm_error":arm_error,
                    "pause_on_hit":true,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"breakpoints":breakpoints}))
    }

    pub(super) fn poll_events(&mut self, params: &Value) -> N64Result<Value> {
        self.drain_debug_update()?;
        let filter = optional_num(params, "breakpoint_id")?;
        let mut returned = Vec::new();
        let mut remaining = VecDeque::new();
        while let Some(event) = self.debug_events.pop_front() {
            if filter.is_none() || event.get("breakpoint_id").and_then(Value::as_u64) == filter {
                returned.push(event);
            } else {
                remaining.push_back(event);
            }
        }
        self.debug_events = remaining;
        Ok(json!({"events":returned, "dropped":0}))
    }

    pub(super) fn drain_debug_update(&mut self) -> N64Result<Option<Value>> {
        let update = UPDATE_COUNT.load(Ordering::Acquire);
        if update <= self.last_debug_update_seen {
            return Ok(None);
        }
        let mut flags = 0_u32;
        let mut accessed = 0_u32;
        unsafe { (self.api.debug_breakpoint_consume)(&mut flags, &mut accessed) };
        self.last_debug_update_seen = update;
        if flags == 0 {
            return Ok(None);
        }
        self.frozen = true;
        self.frame_paused = false;
        let pc = LAST_PC.load(Ordering::Acquire);
        let matches = self
            .breakpoints
            .iter()
            .filter_map(|(id, breakpoint)| {
                trigger_matches(&breakpoint.spec, flags, accessed, pc).then_some(*id)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            let reason = format!(
                "N64 breakpoint trigger matched {} public records",
                matches.len()
            );
            self.fail_debugger_surface(reason.clone());
            return Err(N64Error::BadState(reason));
        }
        let id = matches[0];
        let breakpoint = self
            .breakpoints
            .get(&id)
            .expect("matched N64 breakpoint exists")
            .clone();
        let hit_seq = self.next_hit_seq;
        self.next_hit_seq = self
            .next_hit_seq
            .checked_add(1)
            .ok_or_else(|| N64Error::BadState("N64 hit sequence is exhausted".into()))?;
        let mut event = json!({
            "type":"breakpoint_hit",
            "id":id,
            "breakpoint_id":id,
            "native_slot":breakpoint.native_slot,
            "kind":breakpoint.spec.kind,
            "address": if breakpoint.spec.kind == "exec" { pc } else { accessed },
            "pc":pc,
            "hit_seq":hit_seq,
            "debug_update_seq":update,
            "snapshot_timing": if breakpoint.spec.kind == "exec" {
                "pre_instruction"
            } else {
                "pre_access"
            },
        });
        let mut evidence_errors = Vec::new();
        match self.get_state() {
            Ok(state) => {
                event
                    .as_object_mut()
                    .expect("N64 breakpoint event object")
                    .insert("regs".into(), state["state"].clone());
            }
            Err(error) => evidence_errors.push(format!("register capture failed: {error}")),
        }
        let mut snapshots = Vec::new();
        for snapshot in &breakpoint.spec.snapshots {
            match self.capture_snapshot(snapshot) {
                Ok(value) => snapshots.push(value),
                Err(error) => evidence_errors.push(format!("snapshot capture failed: {error}")),
            }
        }
        let object = event.as_object_mut().expect("N64 breakpoint event object");
        object.insert("snapshot".into(), Value::Array(snapshots));
        object.insert(
            "evidence_complete".into(),
            json!(evidence_errors.is_empty()),
        );
        if !evidence_errors.is_empty() {
            object.insert("evidence_error".into(), json!(evidence_errors));
        }
        self.debug_events.push_back(event.clone());
        Ok(Some(event))
    }

    fn capture_snapshot(&self, snapshot: &SnapshotSpec) -> N64Result<Value> {
        let address = RDRAM_BASE + snapshot.offset;
        let data = (0..snapshot.length)
            .map(|index| unsafe { (self.api.debug_mem_read8)((address + index) as u32) })
            .collect::<Vec<_>>();
        Ok(json!({
            "memory_type":"rdram",
            "address":snapshot.offset,
            "length":snapshot.length,
            "hex":hex::encode(data),
        }))
    }

    fn fail_debugger_surface(&mut self, reason: String) {
        for breakpoint in self.breakpoints.values_mut() {
            breakpoint.arm_state = ArmState::Failed(reason.clone());
        }
    }

    fn verify_native_breakpoints(&self) -> N64Result<()> {
        let expected = i32::try_from(self.breakpoints.len())
            .map_err(|_| N64Error::BadState("N64 breakpoint count overflow".into()))?;
        let native_count = unsafe { (self.api.debug_get_state)(DBG_NUM_BREAKPOINTS) };
        if native_count != expected {
            return Err(N64Error::BadState(format!(
                "N64 native breakpoint count mismatch: expected {expected}, got {native_count}"
            )));
        }
        for (id, breakpoint) in &self.breakpoints {
            let size = breakpoint
                .spec
                .native
                .endaddr
                .checked_sub(breakpoint.spec.native.address)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| N64Error::BadState("N64 native breakpoint range overflow".into()))?;
            let found = unsafe {
                (self.api.debug_breakpoint_lookup)(
                    breakpoint.spec.native.address,
                    size,
                    breakpoint.spec.native.flags,
                )
            };
            if found < 0 || found as u32 != breakpoint.native_slot {
                return Err(N64Error::BadState(format!(
                    "N64 public breakpoint {id} expected native slot {}, got {found}",
                    breakpoint.native_slot
                )));
            }
        }
        Ok(())
    }

    pub(super) fn reset(&mut self, params: &Value) -> N64Result<Value> {
        require_r4300(params)?;
        self.require_connected()?;
        let started_frozen = self.is_frozen_boundary();
        if self.frame_paused && frame_gate_is_blocked() {
            let before = UPDATE_COUNT.load(Ordering::Acquire);
            check_core("DebugSetRunState(reset boundary)", unsafe {
                (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_PAUSED)
            })?;
            release_frame_gate()?;
            wait_until("reset boundary", OPERATION_DEADLINE, || {
                UPDATE_COUNT.load(Ordering::Acquire) > before
                    && unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) }
                        == M64P_DBG_RUNSTATE_PAUSED
            })?;
            self.frame_paused = false;
        } else if !started_frozen {
            self.pause(&json!({"cpu":"r4300"}))?;
        }
        let reset_seq = self.next_reset_seq;
        self.next_reset_seq = self
            .next_reset_seq
            .checked_add(1)
            .ok_or_else(|| N64Error::BadState("N64 reset sequence is exhausted".into()))?;
        let hard = params.get("hard").and_then(Value::as_bool).unwrap_or(false);
        check_core("RESET", unsafe {
            (self.api.core_do_command)(M64CMD_RESET, i32::from(hard), ptr::null_mut())
        })?;
        self.reapply_held_buttons_after_reset()?;
        if let Err(error) = self.verify_native_breakpoints() {
            let reason = format!("N64 reset changed native breakpoints: {error}");
            self.fail_debugger_surface(reason.clone());
            return Err(N64Error::BadState(reason));
        }
        self.last_debug_update_seen = UPDATE_COUNT.load(Ordering::Acquire);
        self.frame_clock_synchronized = false;
        self.frozen = true;
        if !started_frozen {
            self.resume(&json!({"cpu":"r4300"}))?;
        }
        Ok(json!({
            "status":"completed",
            "reset":"completed",
            "reset_seq":reset_seq,
            "hard":hard,
            "state":if started_frozen {"frozen"} else {"running"},
            "pc":LAST_PC.load(Ordering::Acquire),
        }))
    }

    pub(super) fn disassemble(&self, params: &Value) -> N64Result<Value> {
        self.require_frozen("disassemble")?;
        let count = optional_num(params, "count")?.unwrap_or(8);
        if !(1..=MAX_DISASSEMBLY_COUNT).contains(&count) {
            return Err(N64Error::BadParams(format!(
                "N64 disassemble count must be in 1..={MAX_DISASSEMBLY_COUNT}"
            )));
        }
        let (base, _) = disassembly_range(params, count)?;
        let mut instructions = Vec::with_capacity(count as usize);
        for index in 0..count {
            let address = base + index * 4;
            let bytes = (0..4)
                .map(|byte| unsafe { (self.api.debug_mem_read8)((address + byte) as u32) })
                .collect::<Vec<_>>();
            let opcode =
                u32::from_be_bytes(bytes.as_slice().try_into().expect("four opcode bytes"));
            let mut mnemonic = [0_i8; 128];
            let mut arguments = [0_i8; 128];
            unsafe {
                (self.api.debug_decode_op)(
                    opcode,
                    mnemonic.as_mut_ptr(),
                    arguments.as_mut_ptr(),
                    address as c_int,
                )
            };
            let mnemonic = unsafe { CStr::from_ptr(mnemonic.as_ptr()) }.to_string_lossy();
            let arguments = unsafe { CStr::from_ptr(arguments.as_ptr()) }.to_string_lossy();
            let text = if arguments.is_empty() {
                mnemonic.into_owned()
            } else {
                format!("{mnemonic} {arguments}")
            };
            instructions.push(json!({
                "addr":address,
                "bytes":hex::encode(bytes),
                "length":4,
                "text":text,
            }));
        }
        Ok(json!({"instructions":instructions, "cpu":"r4300"}))
    }
}
