use std::collections::{BTreeSet, VecDeque};

use serde_json::{json, Value};

use super::{
    optional_num, required_num, BridgeResult, OpenMsxBridge, OpenMsxBridgeError, OpenMsxControl,
};

const MAX_BREAKPOINTS: usize = 128;
const MAX_SNAPSHOTS: usize = 16;
const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024;
const MAX_DISASSEMBLY_COUNT: u64 = 64;
const REGISTER_NAMES: [&str; 16] = [
    "AF", "BC", "DE", "HL", "AF2", "BC2", "DE2", "HL2", "IX", "IY", "PC", "SP", "I", "R", "IM",
    "IFF",
];

pub(super) const DEBUGGER_EXCEPTION: &str = "openmsx.breakpoint.pausing-subset";

const DEBUGGER_TCL: &str = r#"namespace eval ::emucap {
    variable seq 0
    variable queue {}
    variable queue_bytes 0
    variable dropped 0
    variable specs
    array set specs {}

    proc set_spec {id values} {
        variable specs
        set specs($id) $values
    }

    proc unset_spec {id} {
        variable specs
        unset -nocomplain specs($id)
    }

    proc hit {id kind} {
        variable seq
        variable queue
        variable queue_bytes
        variable dropped
        variable specs
        incr seq

        set pc 0
        set address -
        set value -
        set registers {}
        set snapshots {}
        set capture_error {}
        if {[catch {
            set pc [reg PC]
            if {$kind ne "x"} {
                set address $::wp_last_address
            }
            if {$kind eq "w"} {
                set value $::wp_last_value
            }
            foreach register {AF BC DE HL AF2 BC2 DE2 HL2 IX IY PC SP I R IM IFF} {
                lappend registers [reg $register]
            }
            if {[info exists specs($id)]} {
                foreach spec $specs($id) {
                    lassign $spec space snapshot_address snapshot_length
                    switch -- $space {
                        m { set debuggable memory }
                        r { set debuggable {Main RAM} }
                        v { set debuggable VRAM }
                        default { error "invalid snapshot space" }
                    }
                    set bytes [binary encode hex \
                        [debug read_block $debuggable $snapshot_address $snapshot_length]]
                    lappend snapshots \
                        "$space,$snapshot_address,$snapshot_length,$bytes"
                }
            }
        } capture_error]} {
            set capture_error [binary encode hex \
                [encoding convertto utf-8 $capture_error]]
        }

        set record "$seq|$id|$kind|$pc|$address|$value|[join $registers ,]|[join $snapshots ;]|$capture_error"
        if {[catch {
            set record_bytes [expr {[string length $record] + 1}]
            while {[llength $queue] >= 64 || ($queue_bytes + $record_bytes) > 1048576} {
                if {[llength $queue] == 0} {
                    incr dropped
                    set record {}
                    break
                }
                set oldest [lindex $queue 0]
                set queue [lrange $queue 1 end]
                incr queue_bytes [expr {-[string length $oldest] - 1}]
                incr dropped
            }
            if {$record ne {}} {
                lappend queue $record
                incr queue_bytes $record_bytes
            }
        } queue_error]} {
            incr dropped [llength $queue]
            set queue {}
            set queue_bytes 0
            set queue_error [binary encode hex \
                [encoding convertto utf-8 $queue_error]]
            set record "$seq|$id|$kind|$pc|$address|$value|||$queue_error"
            lappend queue $record
            set queue_bytes [expr {[string length $record] + 1}]
        }
        set ::pause on
        debug break
    }

    proc drain {} {
        variable queue
        variable queue_bytes
        variable dropped
        set payload "$dropped\n[join $queue \n]"
        set queue {}
        set queue_bytes 0
        set dropped 0
        return [binary encode hex [encoding convertto utf-8 $payload]]
    }

    proc inventory {} {
        set records {}
        dict for {id spec} [debug breakpoint list] {
            set valid [expr {
                [dict get $spec -enabled] &&
                ![dict get $spec -once] &&
                [dict get $spec -condition] eq {} &&
                [regexp {^::emucap::hit ([0-9]+) ([xrw])$} \
                    [dict get $spec -command] ignored public_id callback_kind] &&
                $callback_kind eq "x"
            }]
            if {!$valid || [catch {
                set address [expr [dict get $spec -address]]
            }]} {
                lappend records "$id|!|0|0|0"
            } else {
                lappend records "$id|x|$address|$address|$public_id"
            }
        }
        dict for {id spec} [debug watchpoint list] {
            set native_type [dict get $spec -type]
            set kind [expr {
                $native_type eq "read_mem" ? "r" :
                ($native_type eq "write_mem" ? "w" : "!")
            }]
            set valid [expr {
                $kind ne "!" &&
                [dict get $spec -enabled] &&
                ![dict get $spec -once] &&
                [dict get $spec -condition] eq {} &&
                [regexp {^::emucap::hit ([0-9]+) ([xrw])$} \
                    [dict get $spec -command] ignored public_id callback_kind] &&
                $callback_kind eq $kind
            }]
            set native_address [dict get $spec -address]
            if {!$valid || ![llength $native_address] || [llength $native_address] > 2 ||
                [catch {
                    set begin [expr [lindex $native_address 0]]
                    set end [expr [lindex $native_address end]]
                }]} {
                lappend records "$id|!|0|0|0"
            } else {
                lappend records "$id|$kind|$begin|$end|$public_id"
            }
        }
        return [binary encode hex \
            [encoding convertto utf-8 [join [lsort $records] \n]]]
    }
}"#;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NativePoint {
    native_id: String,
    kind: String,
    start: u64,
    end: u64,
    public_id: u64,
}

#[derive(Clone, Debug)]
pub(super) struct SnapshotSpec {
    memory_type: &'static str,
    code: char,
    address: u64,
    length: u64,
}

#[derive(Clone, Debug)]
pub(super) struct PublicBreakpoint {
    kind: String,
    start: u64,
    end: u64,
    native_id: String,
    snapshots: Vec<SnapshotSpec>,
}

pub(super) fn breakpoint_kinds() -> Value {
    json!([
        {
            "kind": "exec",
            "range_unit": "address",
            "range_mode": "exact",
            "memory_type_used": false,
            "pause_on_hit": [true],
            "snapshot": true,
            "snapshot_timing": "pre_instruction",
        },
        {
            "kind": "read",
            "range_unit": "address",
            "range_mode": "inclusive",
            "memory_type_used": true,
            "memory_types": ["memory"],
            "pause_on_hit": [true],
            "snapshot": true,
            "snapshot_timing": "callback_stop",
        },
        {
            "kind": "write",
            "range_unit": "address",
            "range_mode": "inclusive",
            "memory_type_used": true,
            "memory_types": ["memory"],
            "pause_on_hit": [true],
            "snapshot": true,
            "snapshot_timing": "callback_stop",
        }
    ])
}

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub(super) fn initialize_debugger(&mut self) -> BridgeResult<()> {
        self.control.command(DEBUGGER_TCL)?;
        self.control.command("debug break")?;
        self.control.command("set pause on")?;
        self.require_stop_conjunction("debugger initialization")?;
        self.verify_native_breakpoints_with(None, None)?;
        self.initialize_frame_monitor()
    }

    pub(super) fn set_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_debugger_healthy()?;
        if self.breakpoints.len() >= MAX_BREAKPOINTS {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "MSX supports at most {MAX_BREAKPOINTS} public breakpoints"
            )));
        }
        reject_breakpoint_options(params)?;
        if !params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return Err(OpenMsxBridgeError::BadParams(
                "MSX breakpoints require pause_on_hit=true".into(),
            ));
        }
        let kind = params.get("kind").and_then(Value::as_str).unwrap_or("exec");
        let start = required_num(params, "start")?;
        let end = optional_num(params, "end")?.unwrap_or(start);
        if end < start || end > 0xffff {
            return Err(OpenMsxBridgeError::BadParams(
                "MSX breakpoint range must be an ordered 16-bit logical address range".into(),
            ));
        }
        match kind {
            "exec" => {
                if end != start {
                    return Err(OpenMsxBridgeError::BadParams(
                        "MSX exec breakpoints require an exact address".into(),
                    ));
                }
                if params
                    .get("memory_type")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !matches!(value, "cpu" | "memory"))
                {
                    return Err(OpenMsxBridgeError::BadParams(
                        "MSX exec breakpoints use the Z80 logical CPU address space".into(),
                    ));
                }
            }
            "read" | "write" => {
                if params
                    .get("memory_type")
                    .and_then(Value::as_str)
                    .unwrap_or("memory")
                    != "memory"
                {
                    return Err(OpenMsxBridgeError::BadParams(
                        "MSX read/write breakpoints require memory_type=memory".into(),
                    ));
                }
            }
            other => {
                return Err(OpenMsxBridgeError::BadParams(format!(
                    "unsupported MSX breakpoint kind: {other}; supported: exec, read, write"
                )))
            }
        }
        if self
            .breakpoints
            .values()
            .any(|point| point.kind == kind && point.start <= end && start <= point.end)
        {
            return Err(OpenMsxBridgeError::BadParams(
                "MSX trigger identity requires non-overlapping ranges of the same kind".into(),
            ));
        }
        let snapshots = self.parse_snapshot_specs(params.get("snapshot"))?;
        let id = self.next_breakpoint_id;
        let next_id = id.checked_add(1).ok_or_else(|| {
            OpenMsxBridgeError::BadState("MSX breakpoint ID space is exhausted".into())
        })?;
        self.install_snapshot_spec(id, &snapshots)?;

        let token = kind_token(kind);
        let create = if kind == "exec" {
            format!(
                "debug breakpoint create -address {start} -condition {{}} \
                 -command {{::emucap::hit {id} {token}}} -enabled true -once false"
            )
        } else {
            format!(
                "debug watchpoint create -type {kind}_mem -address {{{start} {end}}} \
                 -condition {{}} -command {{::emucap::hit {id} {token}}} \
                 -enabled true -once false"
            )
        };
        let native_id = match self.control.command(&create) {
            Ok(native_id) => native_id,
            Err(error) => {
                return match self.uninstall_snapshot_spec(id) {
                    Ok(()) => Err(error),
                    Err(cleanup) => self.fail_debugger(format!(
                        "{error}; failed to remove unarmed snapshot spec {id}: {cleanup}"
                    )),
                };
            }
        };
        if !valid_native_id(kind, &native_id) {
            let _ = self.uninstall_snapshot_spec(id);
            return self.fail_debugger(format!(
                "openMSX returned an unsafe native breakpoint ID: {native_id:?}"
            ));
        }
        let temporary = PublicBreakpoint {
            kind: kind.into(),
            start,
            end,
            native_id: native_id.clone(),
            snapshots,
        };
        if let Err(error) = self.verify_native_breakpoints_with(Some((id, &temporary)), None) {
            return self.cleanup_failed_arm(id, kind, &native_id, error.to_string());
        }
        self.breakpoints.insert(id, temporary);
        self.next_breakpoint_id = next_id;
        Ok(json!({
            "id":id,
            "set":true,
            "native_id":native_id,
            "arm_state":"armed",
        }))
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_debugger_healthy()?;
        let id = required_num(params, "id")?;
        let breakpoint = self.breakpoints.get(&id).cloned().ok_or_else(|| {
            OpenMsxBridgeError::BadParams(format!("unknown MSX breakpoint id: {id}"))
        })?;
        self.remove_native_breakpoint(&breakpoint)?;
        if let Err(error) = self.verify_native_breakpoints_with(None, Some(id)) {
            return self.fail_debugger(format!(
                "native breakpoint set diverged after clearing {id}: {error}"
            ));
        }
        if let Err(error) = self.uninstall_snapshot_spec(id) {
            return self.fail_debugger(format!(
                "snapshot spec cleanup failed after clearing {id}: {error}"
            ));
        }
        self.breakpoints.remove(&id);
        Ok(json!({"cleared":id}))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> BridgeResult<Value> {
        self.require_debugger_healthy()?;
        let ids = self.breakpoints.keys().copied().collect::<Vec<_>>();
        let mut cleared = Vec::new();
        for id in ids {
            self.clear_breakpoint(&json!({"id":id}))?;
            cleared.push(id);
        }
        Ok(json!({"cleared":cleared}))
    }

    pub(super) fn list_breakpoints(&self) -> BridgeResult<Value> {
        self.require_debugger_healthy()?;
        let breakpoints = self
            .breakpoints
            .iter()
            .map(|(id, breakpoint)| {
                json!({
                    "id":id,
                    "kind":breakpoint.kind,
                    "start":breakpoint.start,
                    "end":breakpoint.end,
                    "native_id":breakpoint.native_id,
                    "arm_state":"armed",
                    "pause_on_hit":true,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"breakpoints":breakpoints}))
    }

    pub(super) fn poll_events(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_debugger_healthy()?;
        let dropped = self.drain_debug_events()?;
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
        Ok(json!({"events":returned, "dropped":dropped}))
    }

    pub(super) fn disassemble(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_debugger_healthy()?;
        self.require_frozen("disassemble")?;
        let mut address = required_num(params, "address")?;
        if address > 0xffff {
            return Err(OpenMsxBridgeError::BadParams(
                "MSX disassembly address exceeds the 16-bit Z80 space".into(),
            ));
        }
        let count = optional_num(params, "count")?.unwrap_or(8);
        if !(1..=MAX_DISASSEMBLY_COUNT).contains(&count) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "MSX disassembly count must be in 1..={MAX_DISASSEMBLY_COUNT}"
            )));
        }
        let mut instructions = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let reply = self.control.command(&format!(
                "set emucap_dasm [debug disasm {address}]; \
                 format \"%s|%s\" \
                 [binary encode hex [encoding convertto utf-8 [lindex $emucap_dasm 0]]] \
                 [join [lrange $emucap_dasm 1 end] ,]"
            ))?;
            let (text, bytes) = parse_disassembly_reply(&reply)?;
            let encoded = hex::encode(&bytes);
            instructions.push(json!({
                "addr":address,
                "text":text,
                "bytes":encoded,
            }));
            address = (address + bytes.len() as u64) & 0xffff;
        }
        Ok(json!({"cpu":"z80", "instructions":instructions}))
    }

    pub(super) fn reconcile_breakpoints(&mut self, operation: &str) -> BridgeResult<()> {
        self.require_debugger_healthy()?;
        if let Err(error) = self.verify_native_breakpoints_with(None, None) {
            return self.fail_debugger(format!(
                "MSX native breakpoint identity changed during {operation}: {error}"
            ));
        }
        Ok(())
    }

    pub(super) fn prepare_temporal_request(&mut self, operation: &str) -> BridgeResult<()> {
        self.require_debugger_healthy()?;
        let dropped = self.drain_debug_events()?;
        if dropped != 0 || !self.debug_events.is_empty() {
            return Err(OpenMsxBridgeError::BadState(format!(
                "{operation} cannot start while breakpoint events are pending; \
                 drain them with poll_events first"
            )));
        }
        Ok(())
    }

    pub(super) fn drain_debug_events(&mut self) -> BridgeResult<u64> {
        let encoded = self.control.command("::emucap::drain")?;
        let bytes = hex::decode(encoded.trim()).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!(
                "openMSX returned invalid debugger queue hex: {error}"
            ))
        })?;
        let payload = String::from_utf8(bytes).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!("openMSX debugger queue was not UTF-8: {error}"))
        })?;
        let mut lines = payload.split('\n');
        let dropped = lines
            .next()
            .ok_or_else(|| {
                OpenMsxBridgeError::Protocol(
                    "openMSX debugger queue omitted its dropped count".into(),
                )
            })?
            .parse::<u64>()
            .map_err(|error| {
                OpenMsxBridgeError::Protocol(format!(
                    "openMSX debugger queue has invalid dropped count: {error}"
                ))
            })?;
        let records = lines.filter(|line| !line.is_empty()).collect::<Vec<_>>();
        if records.is_empty() && dropped != 0 {
            return self.fail_debugger(
                "openMSX debugger queue reported drops without a retained event".into(),
            );
        }
        let before = self.last_hit_seq;
        let mut parsed = Vec::with_capacity(records.len());
        for record in records {
            match self.parse_hit_record(record) {
                Ok(event) => parsed.push(event),
                Err(error) => {
                    return self.fail_debugger(format!(
                        "openMSX debugger event validation failed: {error}"
                    ))
                }
            }
        }
        if let Some(last) = parsed
            .last()
            .and_then(|event| event.get("hit_seq"))
            .and_then(Value::as_u64)
        {
            let retained = parsed.len() as u64;
            let observed_span = last.checked_sub(before).ok_or_else(|| {
                OpenMsxBridgeError::Protocol("openMSX debugger hit sequence moved backwards".into())
            })?;
            if observed_span != retained + dropped {
                return self.fail_debugger(format!(
                    "openMSX debugger sequence/drop mismatch: before={before}, \
                     last={last}, retained={retained}, dropped={dropped}"
                ));
            }
            self.last_hit_seq = last;
        }
        self.debug_events.extend(parsed);
        Ok(dropped)
    }

    fn parse_hit_record(&self, record: &str) -> BridgeResult<Value> {
        let fields = record.split('|').collect::<Vec<_>>();
        if fields.len() != 9 {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "debugger event has {} fields instead of 9",
                fields.len()
            )));
        }
        if !fields[8].is_empty() {
            let bytes = hex::decode(fields[8]).map_err(|error| {
                OpenMsxBridgeError::Protocol(format!(
                    "debugger callback error was not valid hex: {error}"
                ))
            })?;
            let message = String::from_utf8_lossy(&bytes);
            return Err(OpenMsxBridgeError::Emulator(format!(
                "openMSX debugger callback failed: {message}"
            )));
        }
        let hit_seq = parse_event_num(fields[0], "hit sequence")?;
        if hit_seq <= self.last_hit_seq {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "debugger hit sequence did not advance past {}",
                self.last_hit_seq
            )));
        }
        let id = parse_event_num(fields[1], "public breakpoint ID")?;
        let breakpoint = self.breakpoints.get(&id).ok_or_else(|| {
            OpenMsxBridgeError::Protocol(format!(
                "debugger event references unknown public breakpoint {id}"
            ))
        })?;
        let kind = match fields[2] {
            "x" => "exec",
            "r" => "read",
            "w" => "write",
            other => {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "debugger event has invalid kind token {other:?}"
                )))
            }
        };
        if breakpoint.kind != kind {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "debugger event kind {kind} does not match breakpoint {}",
                breakpoint.kind
            )));
        }
        let pc = parse_event_num(fields[3], "PC")?;
        let address = if kind == "exec" {
            if fields[4] != "-" || fields[5] != "-" {
                return Err(OpenMsxBridgeError::Protocol(
                    "exec event unexpectedly contains access evidence".into(),
                ));
            }
            pc
        } else {
            parse_event_num(fields[4], "access address")?
        };
        if !(breakpoint.start..=breakpoint.end).contains(&address) {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "debugger event address {address:#x} is outside breakpoint range \
                 {:#x}..={:#x}",
                breakpoint.start, breakpoint.end
            )));
        }
        let register_values = fields[6].split(',').collect::<Vec<_>>();
        if register_values.len() != REGISTER_NAMES.len() {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "debugger event has {} registers instead of {}",
                register_values.len(),
                REGISTER_NAMES.len()
            )));
        }
        let mut registers = serde_json::Map::new();
        for (name, raw) in REGISTER_NAMES.iter().zip(register_values) {
            let value = parse_event_num(raw, name)?;
            let max = if matches!(*name, "I" | "R" | "IM" | "IFF") {
                0xff
            } else {
                0xffff
            };
            if value > max {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "debugger register {name} exceeds {max:#x}: {value:#x}"
                )));
            }
            registers.insert((*name).into(), json!(value));
        }
        if registers.get("PC").and_then(Value::as_u64) != Some(pc) {
            return Err(OpenMsxBridgeError::Protocol(
                "debugger event PC disagrees with its register snapshot".into(),
            ));
        }
        let snapshots = self.parse_hit_snapshots(fields[7], &breakpoint.snapshots)?;
        let mut event = json!({
            "type":"breakpoint_hit",
            "id":id,
            "breakpoint_id":id,
            "native_id":breakpoint.native_id,
            "kind":kind,
            "address":address,
            "pc":pc,
            "hit_seq":hit_seq,
            "active_cpu":"z80",
            "regs":registers,
            "snapshot":snapshots,
            "snapshot_timing":if kind == "exec" {"pre_instruction"} else {"callback_stop"},
            "evidence_complete":true,
        });
        if kind == "write" {
            let value = parse_event_num(fields[5], "write value")?;
            if value > 0xff {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "debugger write value exceeds one byte: {value:#x}"
                )));
            }
            event
                .as_object_mut()
                .expect("event object")
                .insert("value".into(), json!(value));
        } else if kind == "read" && fields[5] != "-" {
            return Err(OpenMsxBridgeError::Protocol(
                "read event contains a backend-unsupported access value".into(),
            ));
        }
        if let Some(launch_id) = &self.launch_id {
            event
                .as_object_mut()
                .expect("event object")
                .insert("launch_id".into(), json!(launch_id));
        }
        Ok(event)
    }

    fn parse_hit_snapshots(
        &self,
        raw: &str,
        expected: &[SnapshotSpec],
    ) -> BridgeResult<Vec<Value>> {
        let segments = if raw.is_empty() {
            Vec::new()
        } else {
            raw.split(';').collect::<Vec<_>>()
        };
        if segments.len() != expected.len() {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "debugger event returned {} snapshots instead of {}",
                segments.len(),
                expected.len()
            )));
        }
        let mut values = Vec::with_capacity(expected.len());
        for (segment, spec) in segments.into_iter().zip(expected) {
            let fields = segment.split(',').collect::<Vec<_>>();
            if fields.len() != 4
                || fields[0] != spec.code.to_string()
                || parse_event_num(fields[1], "snapshot address")? != spec.address
                || parse_event_num(fields[2], "snapshot length")? != spec.length
            {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "debugger snapshot metadata does not match request: {segment:?}"
                )));
            }
            let bytes = hex::decode(fields[3]).map_err(|error| {
                OpenMsxBridgeError::Protocol(format!(
                    "debugger snapshot bytes are invalid hex: {error}"
                ))
            })?;
            if bytes.len() as u64 != spec.length {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "debugger snapshot returned {} bytes instead of {}",
                    bytes.len(),
                    spec.length
                )));
            }
            values.push(json!({
                "memory_type":spec.memory_type,
                "address":spec.address,
                "length":spec.length,
                "hex":hex::encode(bytes),
            }));
        }
        Ok(values)
    }

    fn parse_snapshot_specs(&self, value: Option<&Value>) -> BridgeResult<Vec<SnapshotSpec>> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let entries = value
            .as_array()
            .ok_or_else(|| OpenMsxBridgeError::BadParams("snapshot must be a list".into()))?;
        if entries.len() > MAX_SNAPSHOTS {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "snapshot accepts at most {MAX_SNAPSHOTS} ranges"
            )));
        }
        let mut total = 0_u64;
        let mut snapshots = Vec::with_capacity(entries.len());
        for entry in entries {
            let raw = entry.as_str().ok_or_else(|| {
                OpenMsxBridgeError::BadParams(
                    "snapshot entries must be memory_type:address:length".into(),
                )
            })?;
            let parts = raw.split(':').collect::<Vec<_>>();
            if parts.len() != 3 {
                return Err(OpenMsxBridgeError::BadParams(format!(
                    "invalid MSX snapshot range: {raw}"
                )));
            }
            let (memory_type, code) = match parts[0] {
                "memory" => ("memory", 'm'),
                "ram" => ("ram", 'r'),
                "vram" => ("vram", 'v'),
                other => {
                    return Err(OpenMsxBridgeError::BadParams(format!(
                        "unsupported MSX snapshot memory_type: {other}"
                    )))
                }
            };
            let address = parse_snapshot_num(parts[1]).ok_or_else(|| {
                OpenMsxBridgeError::BadParams(format!("invalid snapshot address: {raw}"))
            })?;
            let length = parse_snapshot_num(parts[2])
                .filter(|length| *length != 0)
                .ok_or_else(|| {
                    OpenMsxBridgeError::BadParams(format!("invalid snapshot length: {raw}"))
                })?;
            self.validate_range(memory_type, address, length)?;
            total = total.checked_add(length).ok_or_else(|| {
                OpenMsxBridgeError::BadParams("snapshot byte total overflow".into())
            })?;
            if total > MAX_SNAPSHOT_BYTES {
                return Err(OpenMsxBridgeError::BadParams(format!(
                    "snapshot byte total exceeds {MAX_SNAPSHOT_BYTES}"
                )));
            }
            snapshots.push(SnapshotSpec {
                memory_type,
                code,
                address,
                length,
            });
        }
        Ok(snapshots)
    }

    fn install_snapshot_spec(&mut self, id: u64, snapshots: &[SnapshotSpec]) -> BridgeResult<()> {
        let values = snapshots
            .iter()
            .map(|spec| format!("{{{} {} {}}}", spec.code, spec.address, spec.length))
            .collect::<Vec<_>>()
            .join(" ");
        self.control
            .command(&format!("::emucap::set_spec {id} {{{values}}}"))
            .map(|_| ())
    }

    fn uninstall_snapshot_spec(&mut self, id: u64) -> BridgeResult<()> {
        self.control
            .command(&format!("::emucap::unset_spec {id}"))
            .map(|_| ())
    }

    fn cleanup_failed_arm<T>(
        &mut self,
        id: u64,
        kind: &str,
        native_id: &str,
        reason: String,
    ) -> BridgeResult<T> {
        let temporary = PublicBreakpoint {
            kind: kind.into(),
            start: 0,
            end: 0,
            native_id: native_id.into(),
            snapshots: Vec::new(),
        };
        let cleanup = self
            .remove_native_breakpoint(&temporary)
            .and_then(|_| self.uninstall_snapshot_spec(id))
            .and_then(|_| self.verify_native_breakpoints_with(None, None));
        match cleanup {
            Ok(()) => Err(OpenMsxBridgeError::Emulator(reason)),
            Err(cleanup) => self.fail_debugger(format!(
                "{reason}; failed to restore the prior native breakpoint set: {cleanup}"
            )),
        }
    }

    fn remove_native_breakpoint(&mut self, breakpoint: &PublicBreakpoint) -> BridgeResult<()> {
        let collection = if breakpoint.kind == "exec" {
            "breakpoint"
        } else {
            "watchpoint"
        };
        self.control
            .command(&format!(
                "debug {collection} remove {}",
                breakpoint.native_id
            ))
            .map(|_| ())
    }

    fn verify_native_breakpoints_with(
        &mut self,
        extra: Option<(u64, &PublicBreakpoint)>,
        excluded: Option<u64>,
    ) -> BridgeResult<()> {
        let actual = parse_inventory(&self.control.command("::emucap::inventory")?)?;
        let mut expected = self
            .breakpoints
            .iter()
            .filter(|(id, _)| excluded != Some(**id))
            .map(|(id, breakpoint)| native_point(*id, breakpoint))
            .collect::<BTreeSet<_>>();
        if let Some((id, breakpoint)) = extra {
            expected.insert(native_point(id, breakpoint));
        }
        if actual != expected {
            return Err(OpenMsxBridgeError::Emulator(format!(
                "native breakpoint inventory mismatch: expected {expected:?}, observed {actual:?}"
            )));
        }
        Ok(())
    }

    fn require_debugger_healthy(&self) -> BridgeResult<()> {
        match &self.debugger_fatal {
            Some(reason) => Err(OpenMsxBridgeError::Emulator(format!(
                "MSX debugger generation is terminating: {reason}"
            ))),
            None => Ok(()),
        }
    }

    pub(super) fn fail_debugger<T>(&mut self, reason: String) -> BridgeResult<T> {
        self.debugger_fatal = Some(reason.clone());
        Err(OpenMsxBridgeError::Emulator(reason))
    }

    pub(super) fn require_stop_conjunction(&mut self, operation: &str) -> BridgeResult<()> {
        let (paused, breaked) = self.query_stop_state()?;
        self.frozen = paused && breaked;
        if self.frozen {
            Ok(())
        } else {
            Err(OpenMsxBridgeError::Emulator(format!(
                "{operation} did not establish global pause and CPU debug break"
            )))
        }
    }

    pub(super) fn query_stop_state(&mut self) -> BridgeResult<(bool, bool)> {
        let raw = self
            .control
            .command("format \"%s|%s\" [set pause] [debug breaked]")?;
        let (pause, breaked) = raw.split_once('|').ok_or_else(|| {
            OpenMsxBridgeError::Protocol(format!(
                "openMSX returned invalid atomic stop state: {raw:?}"
            ))
        })?;
        let paused = match pause {
            "true" | "1" => true,
            "false" | "0" => false,
            value => {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "openMSX returned invalid pause state: {value:?}"
                )))
            }
        };
        let breaked = match breaked {
            "1" | "true" => true,
            "0" | "false" => false,
            value => {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "openMSX returned invalid debug break state: {value:?}"
                )))
            }
        };
        Ok((paused, breaked))
    }
}

fn reject_breakpoint_options(params: &Value) -> BridgeResult<()> {
    for name in [
        "condition",
        "value",
        "value_mask",
        "value_len",
        "pc_min",
        "pc_max",
    ] {
        if params.get(name).is_some_and(|value| !value.is_null()) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "MSX breakpoints do not support {name}"
            )));
        }
    }
    Ok(())
}

fn kind_token(kind: &str) -> char {
    match kind {
        "exec" => 'x',
        "read" => 'r',
        "write" => 'w',
        _ => unreachable!("validated breakpoint kind"),
    }
}

fn valid_native_id(kind: &str, value: &str) -> bool {
    let prefix = if kind == "exec" { "bp#" } else { "wp#" };
    value.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn native_point(id: u64, breakpoint: &PublicBreakpoint) -> NativePoint {
    NativePoint {
        native_id: breakpoint.native_id.clone(),
        kind: kind_token(&breakpoint.kind).to_string(),
        start: breakpoint.start,
        end: breakpoint.end,
        public_id: id,
    }
}

fn parse_inventory(encoded: &str) -> BridgeResult<BTreeSet<NativePoint>> {
    let bytes = hex::decode(encoded.trim()).map_err(|error| {
        OpenMsxBridgeError::Protocol(format!(
            "openMSX returned invalid breakpoint inventory hex: {error}"
        ))
    })?;
    let inventory = String::from_utf8(bytes).map_err(|error| {
        OpenMsxBridgeError::Protocol(format!(
            "openMSX breakpoint inventory was not UTF-8: {error}"
        ))
    })?;
    let mut points = BTreeSet::new();
    for line in inventory.lines().filter(|line| !line.is_empty()) {
        let fields = line.split('|').collect::<Vec<_>>();
        if fields.len() != 5 || !matches!(fields[1], "x" | "r" | "w") {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "invalid native breakpoint inventory record: {line:?}"
            )));
        }
        let native_id = fields[0].to_string();
        if !valid_native_id(if fields[1] == "x" { "exec" } else { "read" }, &native_id) {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "invalid native breakpoint inventory ID: {native_id:?}"
            )));
        }
        let point = NativePoint {
            native_id,
            kind: fields[1].into(),
            start: parse_event_num(fields[2], "native start")?,
            end: parse_event_num(fields[3], "native end")?,
            public_id: parse_event_num(fields[4], "native public ID")?,
        };
        if !points.insert(point) {
            return Err(OpenMsxBridgeError::Protocol(
                "duplicate native breakpoint inventory record".into(),
            ));
        }
    }
    Ok(points)
}

fn parse_event_num(raw: &str, label: &str) -> BridgeResult<u64> {
    raw.parse::<u64>().map_err(|error| {
        OpenMsxBridgeError::Protocol(format!(
            "openMSX debugger {label} was not an unsigned decimal integer: {raw:?}: {error}"
        ))
    })
}

fn parse_snapshot_num(raw: &str) -> Option<u64> {
    crate::numparse::parse_num_str(raw).ok()
}

fn parse_disassembly_reply(reply: &str) -> BridgeResult<(String, Vec<u8>)> {
    let (text_hex, byte_list) = reply.split_once('|').ok_or_else(|| {
        OpenMsxBridgeError::Protocol("openMSX disassembly reply omitted its separator".into())
    })?;
    let text = String::from_utf8(hex::decode(text_hex).map_err(|error| {
        OpenMsxBridgeError::Protocol(format!("openMSX disassembly text is invalid hex: {error}"))
    })?)
    .map_err(|error| {
        OpenMsxBridgeError::Protocol(format!("openMSX disassembly text is not UTF-8: {error}"))
    })?;
    let raw_bytes = byte_list.split(',').collect::<Vec<_>>();
    if raw_bytes.is_empty() || raw_bytes.len() > 4 {
        return Err(OpenMsxBridgeError::Protocol(format!(
            "openMSX returned invalid Z80 instruction length: {}",
            raw_bytes.len()
        )));
    }
    let mut bytes = Vec::with_capacity(raw_bytes.len());
    for raw in raw_bytes {
        if raw.len() != 2 {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "openMSX returned an invalid instruction byte: {raw:?}"
            )));
        }
        bytes.push(u8::from_str_radix(raw, 16).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!(
                "openMSX returned an invalid instruction byte {raw:?}: {error}"
            ))
        })?);
    }
    Ok((text, bytes))
}
