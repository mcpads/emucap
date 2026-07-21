use super::*;
use base64::Engine;
use std::collections::{HashMap, VecDeque};

#[derive(Default)]
struct FakeWs {
    replies: VecDeque<(String, Value)>,
    pending_events: VecDeque<Value>,
    /// Every `(event, params)` pair seen, in order — lets a test assert the bridge computed
    /// the right PPSSPP request (address/size/base64/...), not just the right event name.
    calls: Vec<(String, Value)>,
    /// Read budget each `call_with_timeout` was invoked with, keyed by event — lets a test
    /// assert the bridge threads the extended savestate budget rather than the default.
    call_timeouts: Vec<(String, Duration)>,
    /// Models PPSSPP replies that arrive slowly: event name → the minimum read budget under
    /// which the reply arrives in time. A `call_with_timeout` with a shorter budget times out
    /// (a `bridge_error`) when the read budget is too short — reproducing the desync.
    slow_replies: HashMap<String, Duration>,
    /// Event names whose `call_ticketed` should time out (a `WouldBlock` read) without consuming
    /// a reply — models a breakpoint halting the CPU mid-press so the timed release ack never
    /// fires, reproducing the desync/stuck-button.
    timeout_events: std::collections::HashSet<String>,
    terminal: bool,
}

impl FakeWs {
    fn with(replies: &[(&str, Value)]) -> Self {
        Self {
            replies: replies
                .iter()
                .map(|(event, reply)| (event.to_string(), reply.clone()))
                .collect(),
            ..Default::default()
        }
    }

    /// Queue a spontaneous event as if it arrived on the wire ahead of any request — models
    /// PPSSPP's unprompted `cpu.stepping`/`cpu.resume`/log/input notifications for `poll_events`.
    fn push_event(&mut self, event: Value) {
        self.pending_events.push_back(event);
    }
}

impl WsTransport for FakeWs {
    fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn call(&mut self, event: &str, params: Value) -> Result<Value, BridgeError> {
        self.calls.push((event.to_string(), params));
        let Some((expected, reply)) = self.replies.pop_front() else {
            return Err(BridgeError::Emulator(format!(
                "unexpected fake WS call: {event}"
            )));
        };
        assert_eq!(event, expected);
        Ok(reply)
    }

    fn call_and_wait_for(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
    ) -> Result<Value, BridgeError> {
        self.calls.push((event.to_string(), params));
        let Some((expected, reply)) = self.replies.pop_front() else {
            return Err(BridgeError::Emulator(format!(
                "unexpected fake WS call: {event} (awaiting {expect_event})"
            )));
        };
        assert_eq!(expect_event, expected);
        Ok(reply)
    }

    fn call_and_wait_for_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
        _timeout: Duration,
    ) -> Result<Value, BridgeError> {
        self.call_and_wait_for(event, params, expect_event)
    }

    fn call_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        self.call_timeouts.push((event.to_string(), timeout));
        // A slow reply only arrives if the read budget outlasts its required wait; too small a
        // budget times out like a socket read (a `bridge_error`), without consuming the
        // reply — reproducing the desync.
        if let Some(required) = self.slow_replies.get(event).copied() {
            if timeout < required {
                return Err(BridgeError::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "fake ws read timed out before the slow reply arrived",
                )));
            }
        }
        self.call(event, params)
    }

    fn call_ticketed(
        &mut self,
        event: &str,
        params: Value,
        ticket: &str,
    ) -> Result<Value, BridgeError> {
        let mut obj = match params {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => panic!("ticketed params must be an object, got {other}"),
        };
        obj.insert("ticket".into(), json!(ticket));
        self.calls.push((event.to_string(), Value::Object(obj)));
        if self.timeout_events.contains(event) {
            // No reply consumed — exactly like a real socket read that times out with the
            // press ack still stranded on PPSSPP's side.
            return Err(BridgeError::Io(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "fake ws ticketed read timed out (press interrupted)",
            )));
        }
        // Model `read_until_ticketed`: skip (queue) any reply whose event or echoed ticket does
        // not match, so a stale off-ticket ack can never satisfy this call.
        loop {
            let Some((expected, reply)) = self.replies.pop_front() else {
                return Err(BridgeError::Emulator(format!(
                    "unexpected fake WS ticketed call: {event}"
                )));
            };
            let reply_ticket = reply.get("ticket").and_then(Value::as_str);
            if expected == event && reply_ticket == Some(ticket) {
                return Ok(reply);
            }
            self.pending_events.push_back(reply);
        }
    }

    fn drain_events(&mut self) -> Vec<Value> {
        self.pending_events.drain(..).collect()
    }
}

#[test]
fn reports_terminal_backend_state_from_transport() {
    let bridge = PpssppBridge::new(FakeWs {
        terminal: true,
        ..Default::default()
    });
    assert!(bridge.backend_terminal());
}

#[test]
fn status_reports_connected_and_version() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("version", json!({"event":"version","version":"v1.17"})),
        (
            "game.status",
            json!({"event":"game.status","game":{"id":"ULJS00001","title":"Tales"},"paused":false}),
        ),
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(1, "status", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["system"], "psp");
    assert_eq!(
        result["execution_limits"]["max_sync_advance_count"],
        crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
    );
    assert_eq!(
        result["execution_limits"]["max_sync_operation_ms"],
        crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64
    );
}

#[test]
fn status_reports_stepping_cpu_as_frozen_even_when_game_status_paused_is_false() {
    // game.status.paused is GetUIState()==UISTATE_PAUSEMENU (the GUI pause menu) — it stays
    // false even while the CPU is halted at a breakpoint. `state` must come from
    // cpu.status.stepping, not game.status.paused.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("version", json!({"event":"version","version":"v1.17"})),
        (
            "game.status",
            json!({"event":"game.status","game":{"id":"ULJS00097","title":"Tales of Destiny 2"},"paused":false}),
        ),
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0x08804128u32,"ticks":0}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(1, "status", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["state"], "frozen");
    assert_eq!(result["ppsspp_version"], "v1.17");
    assert_eq!(result["memory_types"][0], "main");
}

#[test]
fn unknown_method_reports_unknown_method_kind() {
    // A genuinely unknown wire method (typo/garbage) must be unknown_method, distinct from the
    // `unsupported` kind reserved for real-but-ungapped emucap tool names.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(1, "florble", json!({})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "unknown_method");
}

#[test]
fn step_frame_request_is_unsupported_not_reinterpreted_as_instructions() {
    // The MCP frame-step tool sends wire `step` with `{frames:n}` and no `unit`. PPSSPP has no
    // frame-advance, so this must be rejected — not silently stepped as n instructions.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(1, "step", json!({"frames": 60})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "unsupported");
}

#[test]
fn step_wire_method_with_instructions_unit_dispatches_to_stepping() {
    // Older hosts may send instruction stepping as wire `step` with
    // `{frames:n, unit:"instructions"}`. Keep that route compatible with cpu.stepInto.
    let regs = json!({
        "event": "cpu.getAllRegs",
        "categories": [{
            "id": 0, "name": "GPR",
            "registerNames": ["pc"],
            "uintValues": [0x0880_4004u32],
            "floatValues": ["0.0"],
        }],
    });
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "cpu.stepping",
            json!({"event":"cpu.stepping","pc":0x0880_4004u32,"ticks":0}),
        ),
        ("cpu.getAllRegs", regs),
    ]));
    let resp = bridge.handle_request(Request::new(
        1,
        "step",
        json!({"frames": 1, "unit": "instructions"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["unit"], "instructions");
    assert_eq!(result["count"], 1);
    assert_eq!(result["pc"], 0x0880_4004u32);
    // Straight into a single cpu.stepInto (already stepping, so no pre-pause).
    assert_eq!(bridge.ws.calls[1].0, "cpu.stepInto");
}

#[test]
fn hello_advertises_psp_surface_and_truthful_methods() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(1, "hello", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["system"], "psp");
    assert_eq!(result["adapter"], "ppsspp-rust-ws");
    assert_eq!(result["backend"], "ppsspp-debugger-ws");
    assert_eq!(result["debugger"], true);
    assert_eq!(result["memory_types"], json!(["main"]));
    assert_eq!(result["breakpoint_kinds"][0]["kind"], "exec");
    assert_eq!(result["breakpoint_kinds"][0]["memory_type_used"], false);
    assert_eq!(result["breakpoint_kinds"][2]["kind"], "write");
    assert_eq!(
        result["execution_limits"]["max_sync_advance_count"],
        crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
    );
    assert_eq!(
        result["execution_limits"]["max_sync_operation_ms"],
        crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64
    );

    let methods = result["methods"].as_array().unwrap();
    for wanted in [
        "hello",
        "status",
        "get_rom_info",
        "read_memory",
        "write_memory",
        "dump_memory",
        "get_state",
        "disassemble",
        "set_breakpoint",
        "clear_breakpoint",
        "list_breakpoints",
        "clear_all_breakpoints",
        "step_instructions",
        "pause",
        "resume",
        "poll_events",
        "screenshot",
        "set_input",
        "press_buttons",
        "save_state",
        "load_state",
        "reset",
    ] {
        assert!(methods.iter().any(|m| m == wanted), "missing {wanted}");
    }
    // Frame-based `step` must not be advertised as callable (PPSSPP has no frame advance), so
    // the MCP's `has("step")` frame-step composites stay off on PSP.
    assert!(
        !methods.iter().any(|m| m == "step"),
        "should not advertise frame-based step"
    );

    let caps = &result["capability_notes"];
    assert_eq!(caps["disassemble"], true);
    assert_eq!(caps["breakpoints"], true);
    // Stepping IS available, at instruction granularity only — this is the disclosure of
    // the step capability, so `step` is *not* listed as a "planned"/not-yet-callable method.
    assert_eq!(caps["step_units"], json!(["instructions"]));
    assert_eq!(caps["screenshot"], true);
    assert_eq!(caps["input"], true);
    assert_eq!(caps["state_restore"], true);

    let contracts = &result["contracts"];
    assert_eq!(contracts["catalog"], crate::contracts::CATALOG_ID);
    assert_eq!(
        contracts["active_exceptions"],
        json!([
            "ppsspp.execution.frame-step-absent",
            "ppsspp.input-hold.port-zero-only",
            "ppsspp.input-pulse.constraints"
        ])
    );
    assert!(contracts.get("constraints").is_none());
    let advertised_methods: Vec<String> = methods
        .iter()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect();
    let contract_status = crate::contracts::validate_advertisement(
        &crate::contracts::advertisement_from_hello(&result),
        result["adapter"].as_str(),
        result["system"].as_str(),
        &advertised_methods,
    );
    assert_eq!(
        contract_status.state, "validated",
        "{:?}",
        contract_status.errors
    );

    // capability_notes.planned_methods discloses real emucap tool names not dispatched today
    // (all platform-gapped → an "unsupported"). Frame `step` is NOT here: it is a
    // permanent gap conveyed by step_units, not a pending feature — advertising it as planned
    // while wire `step {unit:instructions}` is dispatched-and-working would misrepresent it.
    let planned = caps["planned_methods"].as_array().unwrap();
    assert!(
        !planned.iter().any(|m| m == "step"),
        "frame `step` must not be advertised as a planned/not-yet-callable method"
    );
    for undispatched in [
        "run_frames",
        "probe",
        "find_pattern",
        "watch_register",
        "set_trace",
        "get_trace",
        "break_on_reset",
    ] {
        assert!(
            planned.iter().any(|m| m == undispatched),
            "planned_methods missing {undispatched}"
        );
    }
    // dump_memory is implemented now — it must be a callable method, not advertised as planned.
    assert!(
        methods.iter().any(|m| m == "dump_memory"),
        "dump_memory must be advertised as a callable method"
    );
    assert!(
        !planned.iter().any(|m| m == "dump_memory"),
        "dump_memory must not be advertised as planned/unsupported once implemented"
    );
}

#[test]
fn hello_echoes_session_token_and_name_when_launcher_set_them() {
    // The launcher (`src/launch/ppsspp.rs`) sets EMUCAP_NAME/EMUCAP_SESSION_TOKEN on the
    // bridge process; emucap-mcp's TCP handshake (`live/tcp.rs`) sends a "hello" and rejects
    // the connection with IdentityMismatch unless the reply echoes back the same
    // session_token. `with_identity` supplies both without mutating process env.
    let mut bridge = PpssppBridge::with_identity(
        FakeWs::with(&[]),
        None,
        Some("psp_session".to_string()),
        Some("tok-abc123".to_string()),
    );
    let resp = bridge.handle_request(Request::new(1, "hello", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["name"], "psp_session");
    assert_eq!(result["session_token"], "tok-abc123");
}

#[test]
fn hello_omits_name_and_session_token_when_unset() {
    let mut bridge = PpssppBridge::with_content(FakeWs::with(&[]), None);
    let resp = bridge.handle_request(Request::new(1, "hello", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert!(result.get("name").is_none());
    assert!(result.get("session_token").is_none());
}

#[test]
fn unsupported_whole_methods_return_unsupported_not_unknown_method() {
    // Real emucap tool names with no PPSSPP WS/fork primitive behind them yet must report the
    // "unsupported" kind, not "unknown_method" (reserved for genuine typos).
    for name in [
        "run_frames",
        "probe",
        "find_pattern",
        "watch_register",
        "set_trace",
        "get_trace",
        "break_on_reset",
    ] {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, name, json!({})));
        assert!(!resp.ok, "{name} unexpectedly succeeded");
        assert_eq!(
            resp.error.unwrap().kind,
            "unsupported",
            "{name} should be unsupported, not unknown_method"
        );
        assert!(bridge.ws.calls.is_empty(), "{name} must not call PPSSPP");
    }
}

#[test]
fn read_memory_maps_main_offset_to_absolute_address_and_decodes_hex() {
    let payload = [0xde_u8, 0xad, 0xbe, 0xef];
    let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "memory.read",
        json!({"event":"memory.read","base64": b64}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "main", "address": 0x4000, "length": 4}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["hex"], "deadbeef");

    let (event, params) = &bridge.ws.calls[0];
    assert_eq!(event, "memory.read");
    assert_eq!(params["address"], 0x0880_4000u64);
    assert_eq!(params["size"], 4);
}

#[test]
fn read_memory_defaults_memory_type_to_main() {
    let b64 = base64::engine::general_purpose::STANDARD.encode([0xAAu8]);
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "memory.read",
        json!({"event":"memory.read","base64": b64}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"address": 0, "length": 1}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(bridge.ws.calls[0].1["address"], 0x0880_0000u64);
}

#[test]
fn read_memory_rejects_unsupported_memory_type() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "vram", "address": 0, "length": 4}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(
        bridge.ws.calls.is_empty(),
        "must not call PPSSPP for a rejected memory_type"
    );
}

#[test]
fn read_memory_rejects_length_over_cap() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "main", "address": 0, "length": 0x30_0000}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(
        bridge.ws.calls.is_empty(),
        "must reject before calling PPSSPP"
    );
}

#[test]
fn write_memory_encodes_hex_to_base64_at_absolute_address() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "memory.write",
        json!({"event":"memory.write"}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "main", "address": 0x100, "hex": "aabbccdd"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["written"], 4);

    let (event, params) = &bridge.ws.calls[0];
    assert_eq!(event, "memory.write");
    assert_eq!(params["address"], 0x0880_0100u64);
    let expected_b64 = base64::engine::general_purpose::STANDARD.encode([0xaa, 0xbb, 0xcc, 0xdd]);
    assert_eq!(params["base64"], expected_b64);
}

#[test]
fn write_memory_rejects_odd_length_hex() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "main", "address": 0, "hex": "abc"}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}

#[test]
fn write_memory_rejects_invalid_hex() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "main", "address": 0, "hex": "zzzz"}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}

#[test]
fn read_memory_rejects_offset_past_user_ram_end() {
    // An offset at/past the `main` (user RAM) extent would be forwarded to PPSSPP as an aliased
    // region, so a read there does not read `main`. Reject it before touching the wire.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "main", "address": PSP_MAIN_RAM_SIZE, "length": 4}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(
        bridge.ws.calls.is_empty(),
        "must reject an out-of-range read before calling PPSSPP"
    );
}

#[test]
fn write_memory_rejects_offset_len_straddling_user_ram_end() {
    // The last two bytes fit but the write's 4 bytes run two bytes past the region end — a write
    // that spills out of `main` could corrupt non-`main` memory while reporting success. Reject.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "main", "address": PSP_MAIN_RAM_SIZE - 2, "hex": "aabbccdd"}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(
        bridge.ws.calls.is_empty(),
        "must reject a straddling write before calling PPSSPP"
    );
}

#[test]
fn read_memory_at_last_in_range_bytes_is_allowed() {
    // [offset, offset+len) ending exactly at the region end is in range — the last valid access.
    let payload = [0x11_u8, 0x22, 0x33, 0x44];
    let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "memory.read",
        json!({"event":"memory.read","base64": b64}),
    )]));
    let offset = PSP_MAIN_RAM_SIZE - 4;
    let resp = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "main", "address": offset, "length": 4}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["hex"], "11223344");
    assert_eq!(bridge.ws.calls[0].1["address"], PSP_MAIN_RAM_BASE + offset);
}

#[test]
fn get_state_flattens_gpr_category_into_cpu_prefixed_map() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.getAllRegs",
        json!({
            "event": "cpu.getAllRegs",
            "categories": [
                {
                    "id": 0,
                    "name": "GPR",
                    "registerNames": ["zero", "sp", "ra", "pc"],
                    "uintValues": [0, 0x08900000u32, 0x08900010u32, 0x08900020u32],
                    "floatValues": ["0.000000", "0.000000", "0.000000", "0.000000"],
                },
                {
                    "id": 1,
                    "name": "FPU",
                    "registerNames": ["f0"],
                    "uintValues": [999],
                    "floatValues": ["nan"],
                },
            ],
        }),
    )]));
    let resp = bridge.handle_request(Request::new(1, "get_state", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let state = &resp.result.unwrap()["state"];
    assert_eq!(state["cpu.zero"], 0);
    assert_eq!(state["cpu.sp"], 0x08900000u32);
    assert_eq!(state["cpu.ra"], 0x08900010u32);
    assert_eq!(state["cpu.pc"], 0x08900020u32);
    // FPU/VFPU categories are out of scope for v1 — must not leak in under any prefix.
    assert!(state.get("cpu.f0").is_none());
}

#[test]
fn get_state_errors_when_gpr_category_missing() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.getAllRegs",
        json!({"event": "cpu.getAllRegs", "categories": []}),
    )]));
    let resp = bridge.handle_request(Request::new(1, "get_state", json!({})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "emulator_error");
}

#[test]
fn disassemble_maps_lines_to_addr_bytes_text() {
    let syscall: u32 = 0x0000_000c;
    let jr_ra: u32 = 0x03e0_0008;
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "memory.disasm",
        json!({
            "event": "memory.disasm",
            "range": {"start": 0x0880_4000u32, "end": 0x0880_4008u32},
            "lines": [
                {"address": 0x0880_4000u32, "encoding": syscall, "name": "syscall", "params": ""},
                {"address": 0x0880_4004u32, "encoding": jr_ra, "name": "jr", "params": "ra"},
            ],
            "branchGuides": [],
        }),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "disassemble",
        json!({"address": 0x0880_4000u32, "count": 2}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    let insns = result["instructions"].as_array().unwrap();
    assert_eq!(insns.len(), 2);
    assert_eq!(insns[0]["addr"], 0x0880_4000u64);
    assert_eq!(insns[0]["text"], "syscall");
    assert_eq!(insns[0]["bytes"], hex::encode(syscall.to_le_bytes()));
    assert_eq!(insns[1]["addr"], 0x0880_4004u64);
    assert_eq!(insns[1]["text"], "jr ra");
    assert_eq!(insns[1]["bytes"], hex::encode(jr_ra.to_le_bytes()));

    let (event, params) = &bridge.ws.calls[0];
    assert_eq!(event, "memory.disasm");
    assert_eq!(params["address"], 0x0880_4000u64);
    assert_eq!(params["count"], 2);
}

#[test]
fn disassemble_requires_address() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(1, "disassemble", json!({"count": 2})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}

// --- set_breakpoint / clear_breakpoint / list_breakpoints / clear_all_breakpoints ---

#[test]
fn set_breakpoint_exec_calls_cpu_breakpoint_add_and_tracks_id() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.breakpoint.add",
        json!({"event":"cpu.breakpoint.add"}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"address": 0x0880_4128u32}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["id"], 1);
    assert_eq!(result["kind"], "exec");
    assert_eq!(result["address"], 0x0880_4128u32);

    let (event, params) = &bridge.ws.calls[0];
    assert_eq!(event, "cpu.breakpoint.add");
    assert_eq!(params["address"], 0x0880_4128u32);
    assert_eq!(params["enabled"], true);
    assert!(params.get("condition").is_none());
}

#[test]
fn set_breakpoint_read_calls_memory_breakpoint_add_with_size_and_read_flag() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        // The add reads back the live hit counter to seed last_hits (fresh memcheck → 0).
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
        ),
    ]));
    // A read/write watchpoint takes a memory_type offset (symmetric with read_memory), so offset
    // 0x100 in `main` resolves to PSP_MAIN_RAM_BASE + 0x100 = 0x0880_0100.
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "read", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["kind"], "read");
    assert_eq!(result["length"], 4);

    let (event, params) = &bridge.ws.calls[0];
    assert_eq!(event, "memory.breakpoint.add");
    assert_eq!(params["address"], 0x0880_0100u32);
    assert_eq!(params["size"], 4);
    assert_eq!(params["read"], true);
    assert_eq!(params["write"], false);
}

#[test]
fn set_breakpoint_write_defaults_length_to_one() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0200u32, "size": 1, "hits": 0}]}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": 0x200}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let (_, params) = &bridge.ws.calls[0];
    assert_eq!(params["address"], 0x0880_0200u32);
    assert_eq!(params["size"], 1);
    assert_eq!(params["write"], true);
    assert_eq!(params["read"], false);
}

#[test]
fn set_breakpoint_memory_type_main_routes_offset_and_rejects_out_of_range() {
    // A read/write watchpoint's memory_type:"main" + offset resolves exactly like read_memory
    // (PSP_MAIN_RAM_BASE + offset) instead of a raw low address that never fires, and an offset
    // that leaves the region is rejected before any WS call — symmetric with read/write_memory.
    // (Exec breakpoints take an absolute PC and do NOT route; see
    // set_breakpoint_exec_ignores_memory_type_and_arms_at_raw_pc.)
    let offset = 0x4000u64;
    let expected = PSP_MAIN_RAM_BASE + offset;
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "memory.breakpoint.add",
            json!({"event": "memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": expected, "size": 1, "hits": 0}]}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": offset}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["address"], expected);
    assert_eq!(bridge.ws.calls[0].0, "memory.breakpoint.add");
    assert_eq!(bridge.ws.calls[0].1["address"], expected);

    // An out-of-range main offset is rejected, not silently armed at a bad address.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        2,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": PSP_MAIN_RAM_SIZE}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(
        bridge.ws.calls.is_empty(),
        "must not arm a breakpoint for an out-of-range main offset"
    );
}

#[test]
fn set_breakpoint_exec_ignores_memory_type_and_arms_at_raw_pc() {
    // Regression (0.5.0 exec-BP contract): the MCP wrapper ALWAYS sends `memory_type` (a required
    // field) and, for a single-address call, end==start. An exec breakpoint's address is a raw PC
    // (absolute, e.g. `cpu.pc` from get_state) — it must NOT be offset-routed like a read/write
    // watchpoint, or a README-documented cpu.pc anchor (0x0880_4128) would be mis-read as a `main`
    // offset (0x0880_4128 > region size) and rejected as out of range. Arm at the raw address;
    // memory_type is ignored.
    let pc = 0x0880_4128u64;
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.breakpoint.add",
        json!({"event": "cpu.breakpoint.add"}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "exec", "memory_type": "main", "start": pc, "end": pc}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["address"], pc);
    assert_eq!(bridge.ws.calls[0].0, "cpu.breakpoint.add");
    assert_eq!(bridge.ws.calls[0].1["address"], pc);

    // A genuine range exec point (end != start) is still rejected — PPSSPP's cpu breakpoint is a
    // single address.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        2,
        "set_breakpoint",
        json!({"kind": "exec", "memory_type": "main", "start": pc, "end": pc + 0x10}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "unsupported");
    assert!(bridge.ws.calls.is_empty());
}

#[test]
fn set_breakpoint_rejects_unsupported_kind() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "access", "address": 0}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(bridge.ws.calls.is_empty());
}

#[test]
fn set_breakpoint_rejects_range_exec_breakpoint() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"address": 0x100, "end": 0x110}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "unsupported");
    assert!(bridge.ws.calls.is_empty());
}

#[test]
fn set_breakpoint_translates_pc_min_max_into_condition() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.breakpoint.add",
        json!({"event":"cpu.breakpoint.add"}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"address": 0x100, "pc_min": 0x10, "pc_max": 0x200}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let (_, params) = &bridge.ws.calls[0];
    assert_eq!(params["condition"], "(pc >= 0x10) && (pc <= 0x200)");
}

#[test]
fn set_breakpoint_pause_on_hit_false_maps_to_enabled_false() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.breakpoint.add",
        json!({"event":"cpu.breakpoint.add"}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"address": 0x100, "pause_on_hit": false}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(bridge.ws.calls[0].1["enabled"], false);
}

#[test]
fn set_breakpoint_rejects_auto_savestate_and_snapshot() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"address": 0x100, "auto_savestate": true}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "unsupported");

    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"address": 0x100, "snapshot": ["main:0:4"]}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "unsupported");
}

#[test]
fn clear_breakpoint_exec_calls_cpu_breakpoint_remove() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        (
            "cpu.breakpoint.remove",
            json!({"event":"cpu.breakpoint.remove"}),
        ),
    ]));
    bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x100})));
    let resp = bridge.handle_request(Request::new(2, "clear_breakpoint", json!({"id": 1})));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["cleared"], 1);
    let (event, params) = &bridge.ws.calls[1];
    assert_eq!(event, "cpu.breakpoint.remove");
    assert_eq!(params["address"], 0x100);
}

#[test]
fn clearing_one_duplicate_exec_breakpoint_keeps_the_survivor_armed() {
    // Two exec breakpoints at the SAME address (a duplicate set_breakpoint, or a retry after a
    // lost response) map to ONE PPSSPP cpu breakpoint. Clearing one bridge id must NOT send
    // cpu.breakpoint.remove while the other id still lives — else the survivor would stay in
    // list_breakpoints but never halt again. Only the LAST duplicate on the address tears it down.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        // Exactly ONE remove — emitted only when the last duplicate is cleared.
        (
            "cpu.breakpoint.remove",
            json!({"event":"cpu.breakpoint.remove"}),
        ),
    ]));
    let a = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"address": 0x0880_4128u32}),
    ));
    let b = bridge.handle_request(Request::new(
        2,
        "set_breakpoint",
        json!({"address": 0x0880_4128u32}),
    ));
    assert_eq!(a.result.unwrap()["id"], 1);
    assert_eq!(b.result.unwrap()["id"], 2);
    // Clear the first duplicate: survivor bp2 remains, so NO cpu.breakpoint.remove yet.
    let c1 = bridge.handle_request(Request::new(3, "clear_breakpoint", json!({"id": 1})));
    assert!(c1.ok, "{:?}", c1.error);
    assert_eq!(
        bridge.ws.calls.len(),
        2,
        "clearing one duplicate must not disarm the shared PPSSPP breakpoint"
    );
    // The survivor is still armed and listed.
    let list = bridge.handle_request(Request::new(4, "list_breakpoints", json!({})));
    assert_eq!(
        list.result.unwrap()["breakpoints"],
        json!([{"id": 2, "kind": "exec", "address": 0x0880_4128u32}])
    );
    // Clearing the last duplicate finally removes the PPSSPP breakpoint.
    let c2 = bridge.handle_request(Request::new(5, "clear_breakpoint", json!({"id": 2})));
    assert!(c2.ok, "{:?}", c2.error);
    let (event, params) = &bridge.ws.calls[2];
    assert_eq!(event, "cpu.breakpoint.remove");
    assert_eq!(params["address"], 0x0880_4128u32);
}

#[test]
fn clear_breakpoint_memory_calls_memory_breakpoint_remove_with_size() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        // add reads back the live counter to seed last_hits (calls[1]). A write watchpoint routes
        // its memory_type offset 0x200 to PSP_MAIN_RAM_BASE + 0x200 = 0x0880_0200.
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0200u32, "size": 8, "hits": 0}]}),
        ),
        (
            "memory.breakpoint.remove",
            json!({"event":"memory.breakpoint.remove"}),
        ),
    ]));
    bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": 0x200, "length": 8}),
    ));
    let resp = bridge.handle_request(Request::new(2, "clear_breakpoint", json!({"id": 1})));
    assert!(resp.ok, "{:?}", resp.error);
    // calls: [0]=add, [1]=list (seed), [2]=remove.
    let (event, params) = &bridge.ws.calls[2];
    assert_eq!(event, "memory.breakpoint.remove");
    assert_eq!(params["address"], 0x0880_0200u32);
    assert_eq!(params["size"], 8);
}

#[test]
fn clear_breakpoint_unknown_id_is_bad_params() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(1, "clear_breakpoint", json!({"id": 99})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}

#[test]
fn list_breakpoints_returns_tracked_entries() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0200u32, "size": 2, "hits": 0}]}),
        ),
    ]));
    // exec BP takes a raw absolute PC (0x100); the read watchpoint's memory_type offset 0x200
    // routes to PSP_MAIN_RAM_BASE + 0x200 = 0x0880_0200.
    bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x100})));
    bridge.handle_request(Request::new(
        2,
        "set_breakpoint",
        json!({"kind": "read", "memory_type": "main", "start": 0x200, "length": 2}),
    ));
    let resp = bridge.handle_request(Request::new(3, "list_breakpoints", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let rows = resp.result.unwrap()["breakpoints"].clone();
    assert_eq!(
        rows,
        json!([
            {"id": 1, "kind": "exec", "address": 0x100},
            {"id": 2, "kind": "read", "address": 0x0880_0200u32, "length": 2},
        ])
    );
}

#[test]
fn clear_all_breakpoints_clears_every_tracked_id() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        (
            "cpu.breakpoint.remove",
            json!({"event":"cpu.breakpoint.remove"}),
        ),
        (
            "cpu.breakpoint.remove",
            json!({"event":"cpu.breakpoint.remove"}),
        ),
    ]));
    bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x100})));
    bridge.handle_request(Request::new(2, "set_breakpoint", json!({"address": 0x200})));
    let resp = bridge.handle_request(Request::new(3, "clear_all_breakpoints", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["cleared"], json!([1, 2]));
    let list = bridge.handle_request(Request::new(4, "list_breakpoints", json!({})));
    assert_eq!(list.result.unwrap()["breakpoints"], json!([]));
}

// --- pause / resume ---

#[test]
fn pause_sends_cpu_stepping_when_running() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "cpu.stepping",
            json!({"event":"cpu.stepping","pc":0x100,"ticks":0}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(1, "pause", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["state"], "frozen");
    assert_eq!(bridge.ws.calls[1].0, "cpu.stepping");
}

#[test]
fn pause_is_a_noop_when_already_stepping() {
    // PPSSPP's WebSocketCPUStepping silently does nothing when already stepping (no state
    // change, so no ack ever arrives) — calling it here would hang the bridge. The FakeWs has
    // no "cpu.stepping" reply queued, so this test would fail loudly if the guard were missing.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.status",
        json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
    )]));
    let resp = bridge.handle_request(Request::new(1, "pause", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["state"], "frozen");
    assert_eq!(bridge.ws.calls.len(), 1, "must not call cpu.stepping again");
}

#[test]
fn resume_sends_cpu_resume_when_stepping() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
        ),
        ("cpu.resume", json!({"event":"cpu.resume"})),
    ]));
    let resp = bridge.handle_request(Request::new(1, "resume", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["state"], "running");
    assert_eq!(bridge.ws.calls[1].0, "cpu.resume");
}

#[test]
fn resume_is_a_noop_when_already_running() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.status",
        json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
    )]));
    let resp = bridge.handle_request(Request::new(1, "resume", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["state"], "running");
    assert_eq!(bridge.ws.calls.len(), 1, "must not call cpu.resume");
}

// --- step_instructions ---

#[test]
fn step_instructions_pauses_first_when_running_then_steps_and_reports_state() {
    let regs = json!({
        "event": "cpu.getAllRegs",
        "categories": [{
            "id": 0, "name": "GPR",
            "registerNames": ["zero", "pc"],
            "uintValues": [0, 0x0880_4004u32],
            "floatValues": ["0.0", "0.0"],
        }],
    });
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "cpu.stepping",
            json!({"event":"cpu.stepping","pc":0x0880_4000u32,"ticks":0}),
        ),
        (
            "cpu.stepping",
            json!({"event":"cpu.stepping","pc":0x0880_4004u32,"ticks":0}),
        ),
        ("cpu.getAllRegs", regs),
    ]));
    let resp = bridge.handle_request(Request::new(1, "step_instructions", json!({"count": 1})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["unit"], "instructions");
    assert_eq!(result["count"], 1);
    assert_eq!(result["pc"], 0x0880_4004u32);
    assert_eq!(result["state"]["cpu.pc"], 0x0880_4004u32);

    // First call must be the pre-pause (cpu.status found it running, so a cpu.stepping pause
    // request goes out) followed by exactly one cpu.stepInto for count=1.
    assert_eq!(bridge.ws.calls[1].0, "cpu.stepping");
    assert_eq!(bridge.ws.calls[2].0, "cpu.stepInto");
}

#[test]
fn step_instructions_rejects_over_sync_cap_before_backend_calls() {
    let mut bridge = PpssppBridge::with_content(FakeWs::default(), None);
    let response = bridge.handle_request(Request::new(
        1,
        "step_instructions",
        json!({"count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT + 1}),
    ));

    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert!(bridge.ws.calls.is_empty());
}

#[test]
fn step_instructions_skips_pre_pause_when_already_stepping() {
    let regs = json!({
        "event": "cpu.getAllRegs",
        "categories": [{
            "id": 0, "name": "GPR",
            "registerNames": ["pc"],
            "uintValues": [0x0880_4008u32],
            "floatValues": ["0.0"],
        }],
    });
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "cpu.stepping",
            json!({"event":"cpu.stepping","pc":0x0880_4004u32,"ticks":0}),
        ),
        (
            "cpu.stepping",
            json!({"event":"cpu.stepping","pc":0x0880_4008u32,"ticks":0}),
        ),
        ("cpu.getAllRegs", regs),
    ]));
    let resp = bridge.handle_request(Request::new(1, "step_instructions", json!({"count": 2})));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["count"], 2);
    // No pre-pause cpu.stepping call — straight into two cpu.stepInto calls.
    assert_eq!(bridge.ws.calls[0].0, "cpu.status");
    assert_eq!(bridge.ws.calls[1].0, "cpu.stepInto");
    assert_eq!(bridge.ws.calls[2].0, "cpu.stepInto");
}

// --- poll_events ---

fn gpr_only_pc(pc: u32) -> Value {
    json!({
        "event": "cpu.getAllRegs",
        "categories": [{
            "id": 0, "name": "GPR",
            "registerNames": ["pc"],
            "uintValues": [pc],
            "floatValues": ["0.0"],
        }],
    })
}

#[test]
fn poll_events_ignores_non_stepping_spontaneous_events_and_counts_them_as_dropped() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    bridge
        .ws
        .push_event(json!({"event":"input.analog","stick":"left","x":0.0,"y":0.0}));
    bridge.ws.push_event(json!({"event":"cpu.resume"}));
    let resp = bridge.handle_request(Request::new(1, "poll_events", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["events"], json!([]));
    // Both discarded spontaneous events must be reported as dropped, not a hardcoded 0.
    assert_eq!(result["dropped"], 2);
}

#[test]
fn poll_events_reports_zero_dropped_when_only_stops_arrive() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[("cpu.getAllRegs", gpr_only_pc(0x123))]));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x123,"ticks":5}));
    let resp = bridge.handle_request(Request::new(1, "poll_events", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 1);
    assert_eq!(result["dropped"], 0);
}

#[test]
fn poll_events_reports_generic_stop_when_pc_matches_no_breakpoint() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[("cpu.getAllRegs", gpr_only_pc(0x123))]));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x123,"ticks":5}));
    let resp = bridge.handle_request(Request::new(1, "poll_events", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let events = resp.result.unwrap()["events"].clone();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["type"], "stop");
    assert_eq!(events[0]["pc"], 0x123);
    assert_eq!(events[0]["regs"]["cpu.pc"], 0x123);
    assert!(events[0].get("breakpoint_id").is_none());
}

#[test]
fn poll_events_classifies_exec_breakpoint_hit_by_pc_match() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        ("cpu.getAllRegs", gpr_only_pc(0x0880_4004)),
    ]));
    bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"address": 0x0880_4004u32}),
    ));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x0880_4004u32,"ticks":42}));
    let resp = bridge.handle_request(Request::new(2, "poll_events", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let events = resp.result.unwrap()["events"].clone();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["type"], "breakpoint_hit");
    assert_eq!(events[0]["kind"], "exec");
    assert_eq!(events[0]["address"], 0x0880_4004u32);
    assert_eq!(events[0]["breakpoint_id"], 1);
    assert_eq!(events[0]["id"], 1);
    assert_eq!(events[0]["regs"]["cpu.pc"], 0x0880_4004u32);
}

#[test]
fn poll_events_classifies_memory_breakpoint_hit_via_hits_delta() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        // add reads back the live counter to seed last_hits — fresh memcheck, no hits yet.
        (
            "memory.breakpoint.list",
            json!({
                "event": "memory.breakpoint.list",
                "breakpoints": [
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0},
                ],
            }),
        ),
        ("cpu.getAllRegs", gpr_only_pc(0x0880_9000)),
        (
            "memory.breakpoint.list",
            json!({
                "event": "memory.breakpoint.list",
                "breakpoints": [
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1},
                ],
            }),
        ),
    ]));
    bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    // The stop's pc is the writing instruction's address (0x08809000), not the watched address
    // — exec pc-matching cannot attribute this, so the hits-delta cross-check must.
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x0880_9000u32,"ticks":7}));
    let resp = bridge.handle_request(Request::new(2, "poll_events", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let events = resp.result.unwrap()["events"].clone();
    assert_eq!(events[0]["type"], "breakpoint_hit");
    assert_eq!(events[0]["kind"], "write");
    assert_eq!(events[0]["address"], 0x0880_0100u32);
    assert_eq!(events[0]["breakpoint_id"], 1);

    // A second poll with an unchanged hits count must not re-report the same hit.
    bridge
        .ws
        .replies
        .push_back(("cpu.getAllRegs".to_string(), gpr_only_pc(0x0880_9010)));
    bridge.ws.replies.push_back((
        "memory.breakpoint.list".to_string(),
        json!({
            "event": "memory.breakpoint.list",
            "breakpoints": [
                {"address": 0x0880_0100u32, "size": 4, "hits": 1},
            ],
        }),
    ));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x0880_9010u32,"ticks":9}));
    let resp2 = bridge.handle_request(Request::new(3, "poll_events", json!({})));
    let events2 = resp2.result.unwrap()["events"].clone();
    assert_eq!(events2[0]["type"], "stop", "hits unchanged — not a new hit");
}

#[test]
fn set_breakpoint_duplicate_address_inherits_hit_count_so_no_false_hit() {
    // PPSSPP reuses the existing memcheck and PRESERVES numHits on a re-add. A second
    // breakpoint at an already-hit address/size seeded last_hits=0 would make the very next
    // unrelated stop look like a fresh hit on it. The bridge seeds each add from PPSSPP's live
    // counter (`memory.breakpoint.list`), which for a duplicate returns the preserved hit count.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        // bp1 seed — fresh memcheck, no hits yet.
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
        ),
        ("cpu.getAllRegs", gpr_only_pc(0x0880_9000)),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
        ),
        // bp2 add at the same address/size — seed reads the PRESERVED live count (1).
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
        ),
        ("cpu.getAllRegs", gpr_only_pc(0x0880_9010)),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
        ),
    ]));
    // bp1, then a real hit takes the shared counter to 1 (attributed to bp1).
    bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x0880_9000u32,"ticks":7}));
    let hit = bridge.handle_request(Request::new(2, "poll_events", json!({})));
    assert_eq!(hit.result.unwrap()["events"][0]["breakpoint_id"], 1);
    // bp2 at the SAME address/size — must inherit the current hit count (1), not reset to 0.
    let add2 = bridge.handle_request(Request::new(
        3,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    assert_eq!(add2.result.unwrap()["id"], 2);
    // An unrelated stop with the counter unchanged at 1 must be a generic stop, not a false hit.
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x0880_9010u32,"ticks":9}));
    let unrelated = bridge.handle_request(Request::new(4, "poll_events", json!({})));
    let ev = unrelated.result.unwrap()["events"][0].clone();
    assert_eq!(
        ev["type"], "stop",
        "an unrelated stop must not be misattributed to the duplicate breakpoint"
    );
    assert!(
        ev.get("breakpoint_id").is_none(),
        "no breakpoint should be credited for an unchanged hit count"
    );
}

#[test]
fn set_breakpoint_after_reclear_seeds_from_live_counter_so_first_hit_not_missed() {
    // Seed logic: a sole breakpoint on a range is hit (shared memcheck
    // numHits→1), then cleared — clearing the LAST bridge id on the range removes the memcheck,
    // so PPSSPP resets its numHits. Re-adding creates a FRESH memcheck at numHits=0. Seeding the
    // re-add from PPSSPP's live counter (0) makes the first real hit (0→1) satisfy
    // `hits > last_hits`; a stale non-zero seed would miss it. (The duplicate-while-live case is
    // covered by set_breakpoint_duplicate_address_inherits_hit_count; the clear no longer nukes a
    // memcheck a surviving duplicate needs — see clearing_one_duplicate_memory_breakpoint_*.)
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        // bp1 add + seed (fresh, hits=0).
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
        ),
        // First hit poll → shared counter goes to 1 (attributed to bp1).
        ("cpu.getAllRegs", gpr_only_pc(0x0880_9000)),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
        ),
        // clear bp1 → sole id on the range, so the shared memcheck is removed (numHits reset).
        (
            "memory.breakpoint.remove",
            json!({"event":"memory.breakpoint.remove"}),
        ),
        // bp2 add + seed — PPSSPP recreated the memcheck, so its live counter is back to 0.
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
        ),
        // The first REAL hit after the reclear → counter 0→1; must be attributed to bp2.
        ("cpu.getAllRegs", gpr_only_pc(0x0880_9020)),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
        ),
    ]));
    bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x0880_9000u32,"ticks":7}));
    let first = bridge.handle_request(Request::new(2, "poll_events", json!({})));
    assert_eq!(first.result.unwrap()["events"][0]["breakpoint_id"], 1);
    // Clear bp1 — sole id on the range, so the memcheck (and its numHits) is torn down.
    let cleared = bridge.handle_request(Request::new(3, "clear_breakpoint", json!({"id": 1})));
    assert!(cleared.ok, "{:?}", cleared.error);
    // Re-add at the same address/size — a fresh memcheck at numHits=0.
    let re_add = bridge.handle_request(Request::new(
        4,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    assert_eq!(re_add.result.unwrap()["id"], 2);
    // The first real hit on the re-added breakpoint must NOT be missed.
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x0880_9020u32,"ticks":11}));
    let real = bridge.handle_request(Request::new(5, "poll_events", json!({})));
    let ev = real.result.unwrap()["events"][0].clone();
    assert_eq!(
        ev["type"], "breakpoint_hit",
        "first real hit after reclear was missed"
    );
    assert_eq!(ev["breakpoint_id"], 2);
    assert_eq!(ev["kind"], "write");
    assert_eq!(ev["address"], 0x0880_0100u32);
}

#[test]
fn clearing_one_duplicate_memory_breakpoint_keeps_the_shared_memcheck_for_the_survivor() {
    // PPSSPP keeps ONE memcheck per (address, size); bp1 and bp2 both watch it.
    // Clearing bp1 must NOT tear the shared memcheck down while bp2 still lives — otherwise bp2
    // would stay in list_breakpoints but never stop again. So the first clear sends no
    // memory.breakpoint.remove (a survivor remains) and a later access still attributes a hit to
    // bp2; clearing the LAST duplicate finally removes the memcheck.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        // bp1 add + seed (fresh, 0).
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
        ),
        // bp2 add + seed (duplicate, still 0).
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
        ),
        // clear bp1: survivor bp2 remains → NO memory.breakpoint.remove is queued here.
        // a write then hits (counter 0→1) — the survivor bp2 must still be credited.
        ("cpu.getAllRegs", gpr_only_pc(0x0880_9000)),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 1}]}),
        ),
        // clear bp2: now the last id on the range → the memcheck is removed.
        (
            "memory.breakpoint.remove",
            json!({"event":"memory.breakpoint.remove"}),
        ),
    ]));
    bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "read", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    bridge.handle_request(Request::new(
        2,
        "set_breakpoint",
        json!({"kind": "read", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    // Clear bp1 — bp2 survives, so the shared memcheck must stay (no remove call).
    let cleared = bridge.handle_request(Request::new(3, "clear_breakpoint", json!({"id": 1})));
    assert!(cleared.ok, "{:?}", cleared.error);
    assert!(
        !bridge
            .ws
            .calls
            .iter()
            .any(|(e, _)| e == "memory.breakpoint.remove"),
        "clearing a duplicate must not remove the memcheck the survivor still needs: {:?}",
        bridge.ws.calls
    );
    // bp2 still stops: a write hit is attributed to it, not lost.
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x0880_9000u32,"ticks":7}));
    let hit = bridge.handle_request(Request::new(4, "poll_events", json!({})));
    let ev = hit.result.unwrap()["events"][0].clone();
    assert_eq!(
        ev["type"], "breakpoint_hit",
        "the survivor stopped working after its duplicate was cleared"
    );
    assert_eq!(ev["breakpoint_id"], 2);
    // Clearing the last duplicate finally removes the shared memcheck.
    let cleared2 = bridge.handle_request(Request::new(5, "clear_breakpoint", json!({"id": 2})));
    assert!(cleared2.ok, "{:?}", cleared2.error);
    assert!(
        bridge
            .ws
            .calls
            .iter()
            .any(|(e, p)| e == "memory.breakpoint.remove"
                && p["address"] == 0x0880_0100u32
                && p["size"] == 4),
        "clearing the last duplicate must remove the memcheck: {:?}",
        bridge.ws.calls
    );
}

#[test]
fn set_breakpoint_rejects_read_and_write_on_the_same_range() {
    // A read and a write breakpoint on the SAME (address, size) collapse into one
    // PPSSPP memcheck with one shared hit counter, so a hit could not be told apart between the
    // two bridge ids. Refuse the ambiguous pair rather than advertise a disambiguation PPSSPP
    // cannot provide. (A different size on the same address is a distinct memcheck — allowed.)
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "memory.breakpoint.add",
            json!({"event":"memory.breakpoint.add"}),
        ),
        (
            "memory.breakpoint.list",
            json!({"event":"memory.breakpoint.list","breakpoints":[
                    {"address": 0x0880_0100u32, "size": 4, "hits": 0}]}),
        ),
    ]));
    let read = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "read", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    assert!(read.ok, "{:?}", read.error);
    // A write on the exact same range must be refused before any WS round trip (only the two
    // add/seed calls above are queued — a WS call here would panic on an unexpected event).
    let write = bridge.handle_request(Request::new(
        2,
        "set_breakpoint",
        json!({"kind": "write", "memory_type": "main", "start": 0x100, "length": 4}),
    ));
    assert!(!write.ok);
    assert_eq!(write.error.unwrap().kind, "bad_params");
}

#[test]
fn poll_events_breakpoint_id_filter_holds_back_non_matching_events() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        ("cpu.getAllRegs", gpr_only_pc(0x100)),
        ("cpu.getAllRegs", gpr_only_pc(0x200)),
    ]));
    bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x100})));
    bridge.handle_request(Request::new(2, "set_breakpoint", json!({"address": 0x200})));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x100,"ticks":1}));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x200,"ticks":2}));

    let filtered =
        bridge.handle_request(Request::new(3, "poll_events", json!({"breakpoint_id": 2})));
    let events = filtered.result.unwrap()["events"].clone();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["breakpoint_id"], 2);

    // The id=1 hit was held back, not dropped — an unfiltered poll must still see it.
    let unfiltered = bridge.handle_request(Request::new(4, "poll_events", json!({})));
    let events = unfiltered.result.unwrap()["events"].clone();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["breakpoint_id"], 1);
}

#[test]
fn poll_events_malformed_filter_errors_without_losing_buffered_hits() {
    // A malformed breakpoint_id must be rejected BEFORE the transport is drained — otherwise the
    // failed request destructively consumes an already-buffered breakpoint-hit event and loses
    // it forever. Only one cpu.getAllRegs reply is queued (for the later valid poll's enrich):
    // the malformed poll must not consume anything from the transport.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("cpu.breakpoint.add", json!({"event":"cpu.breakpoint.add"})),
        ("cpu.getAllRegs", gpr_only_pc(0x100)),
    ]));
    bridge.handle_request(Request::new(1, "set_breakpoint", json!({"address": 0x100})));
    bridge
        .ws
        .push_event(json!({"event":"cpu.stepping","pc":0x100,"ticks":1}));

    let bad = bridge.handle_request(Request::new(
        2,
        "poll_events",
        json!({"breakpoint_id": "not-a-number"}),
    ));
    assert!(!bad.ok);
    assert_eq!(bad.error.unwrap().kind, "bad_params");

    // The buffered hit survived the failed poll — a subsequent valid poll still surfaces it.
    let good = bridge.handle_request(Request::new(3, "poll_events", json!({})));
    let events = good.result.unwrap()["events"].clone();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["breakpoint_id"], 1);
}

// --- screenshot ---

#[test]
fn screenshot_decodes_data_uri_into_uniform_png_base64() {
    let png_bytes = [
        0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0xde, 0xad,
    ];
    let b64 = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    let uri = format!("data:image/png;base64,{b64}");
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "emucap.screenshot",
            json!({"event":"emucap.screenshot","width":480,"height":272,"uri":uri}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(1, "screenshot", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["png_base64"], b64);
    assert_eq!(result["width"], 480);
    assert_eq!(result["height"], 272);
}

#[test]
fn screenshot_defaults_dimensions_when_reply_omits_them() {
    let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
    let uri = format!("data:image/png;base64,{b64}");
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "emucap.screenshot",
            json!({"event":"emucap.screenshot","uri":uri}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(1, "screenshot", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["width"], 480);
    assert_eq!(result["height"], 272);
}

#[test]
fn screenshot_rejects_reply_missing_uri_field() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        ("emucap.screenshot", json!({"event":"emucap.screenshot"})),
    ]));
    let resp = bridge.handle_request(Request::new(1, "screenshot", json!({})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "emulator_error");
}

#[test]
fn screenshot_rejects_while_cpu_halted() {
    // emucap.screenshot drives GE stepping, which only progresses while the CPU is running —
    // a halted core must fail fast (bad_params) instead of riding PPSSPP's own ~5s wait to an
    // emulator_error. The FakeWs has no "emucap.screenshot" reply queued, so this test would
    // fail loudly (unexpected fake WS call) if the proactive guard were missing.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.status",
        json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
    )]));
    let resp = bridge.handle_request(Request::new(1, "screenshot", json!({})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert_eq!(
        bridge.ws.calls.len(),
        1,
        "must not call emucap.screenshot while the CPU is halted"
    );
}

// --- set_input / press_buttons ---

#[test]
fn set_input_sends_full_button_map_with_requested_true_rest_false() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "input.buttons.send",
        json!({"event":"input.buttons.send"}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"buttons": ["a", "up"]}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let (event, params) = &bridge.ws.calls[0];
    assert_eq!(event, "input.buttons.send");
    let buttons = &params["buttons"];
    assert_eq!(buttons["cross"], true);
    assert_eq!(buttons["up"], true);
    assert_eq!(buttons["circle"], false);
    assert_eq!(buttons["down"], false);
    assert_eq!(buttons["triangle"], false);
    assert_eq!(buttons["square"], false);
    assert_eq!(buttons["ltrigger"], false);
    assert_eq!(buttons["rtrigger"], false);
    assert_eq!(buttons["start"], false);
    assert_eq!(buttons["select"], false);
    assert_eq!(buttons["left"], false);
    assert_eq!(buttons["right"], false);
}

#[test]
fn set_input_empty_list_releases_every_tracked_button() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "input.buttons.send",
        json!({"event":"input.buttons.send"}),
    )]));
    let resp = bridge.handle_request(Request::new(1, "set_input", json!({"buttons": []})));
    assert!(resp.ok, "{:?}", resp.error);
    let params = &bridge.ws.calls[0].1;
    for psp_name in [
        "cross", "circle", "triangle", "square", "ltrigger", "rtrigger", "start", "select", "up",
        "down", "left", "right",
    ] {
        assert_eq!(
            params["buttons"][psp_name], false,
            "{psp_name} must be released"
        );
    }
}

#[test]
fn set_input_rejects_unknown_button() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"buttons": ["nonsense"]}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(bridge.ws.calls.is_empty());
}

#[test]
fn input_methods_reject_nonzero_port_before_ws_mutation() {
    for (method, params) in [
        ("set_input", json!({"port": 1, "buttons": ["a"]})),
        (
            "press_buttons",
            json!({"port": 1, "buttons": ["a"], "frames": 1}),
        ),
    ] {
        let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
        let resp = bridge.handle_request(Request::new(1, method, params));
        assert!(!resp.ok, "{method} must reject port 1");
        assert_eq!(resp.error.unwrap().kind, "bad_params");
        assert!(
            bridge.ws.calls.is_empty(),
            "{method} mutated PPSSPP before rejecting the port"
        );
    }
}

#[test]
fn status_reports_bridge_owned_persistent_input_after_set() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("input.buttons.send", json!({"event":"input.buttons.send"})),
        ("version", json!({"event":"version","version":"test"})),
        (
            "game.status",
            json!({"event":"game.status","game":{},"paused":false}),
        ),
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
    ]));
    let set = bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"buttons": ["a", "up"]}),
    ));
    assert!(set.ok, "{:?}", set.error);

    let status = bridge.handle_request(Request::new(2, "status", json!({})));
    assert!(status.ok, "{:?}", status.error);
    let input = &status.result.unwrap()["input_override"];
    assert_eq!(input["observable"], true);
    assert_eq!(input["authority"], "bridge_local");
    assert_eq!(input["engaged"], true);
    assert_eq!(input["buttons"], json!(["a", "up"]));
}

#[test]
fn status_reports_native_input_after_empty_set() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("input.buttons.send", json!({"event":"input.buttons.send"})),
        ("version", json!({"event":"version","version":"test"})),
        (
            "game.status",
            json!({"event":"game.status","game":{},"paused":false}),
        ),
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
    ]));
    let set = bridge.handle_request(Request::new(1, "set_input", json!({"buttons": []})));
    assert!(set.ok, "{:?}", set.error);

    let status = bridge.handle_request(Request::new(2, "status", json!({})));
    assert!(status.ok, "{:?}", status.error);
    let input = &status.result.unwrap()["input_override"];
    assert_eq!(input["observable"], true);
    assert_eq!(input["authority"], "bridge_local");
    assert_eq!(input["engaged"], false);
    assert_eq!(input["mode"], "native");
    assert_eq!(input["buttons"], json!([]));
}

#[test]
fn press_buttons_maps_uniform_name_to_psp_and_sends_duration() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "input.buttons.press",
            json!({"event":"input.buttons.press","ticket":"emucap-1"}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": 3}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let (event, params) = &bridge.ws.calls[1];
    assert_eq!(event, "input.buttons.press");
    assert_eq!(params["button"], "cross");
    assert_eq!(params["duration"], 3);
    // The request is ticket-tagged so its delayed release ack can be correlated.
    assert_eq!(params["ticket"], "emucap-1");
}

#[test]
fn press_buttons_rejects_combo_before_any_ws_mutation() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["up", "a"], "frames": 2}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(bridge.ws.calls.is_empty());
}

#[test]
fn press_buttons_defaults_frames_to_one() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "input.buttons.press",
            json!({"event":"input.buttons.press","ticket":"emucap-1"}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["start"]}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(bridge.ws.calls[1].1["duration"], 1);
}

#[test]
fn press_buttons_timeout_releases_inputs_and_surfaces_error() {
    // An exec breakpoint halts the CPU mid-press. The pre-check passed while
    // running, then frames stopped, so PPSSPP's timed release ack never fires and the ticketed
    // read times out (WouldBlock) with the button still held. The bridge must release every
    // input (empty input.buttons.send) and return a clear timeout error, not leave it stuck.
    let mut ws = FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        ("input.buttons.send", json!({"event":"input.buttons.send"})),
    ]);
    ws.timeout_events.insert("input.buttons.press".into());
    let mut bridge = PpssppBridge::new(ws);
    let resp = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": 240}),
    ));
    assert!(!resp.ok, "a timed-out press must not report success");
    assert!(
        resp.error.unwrap().message.contains("timed out"),
        "error should explain the mid-press timeout"
    );
    // calls: cpu.status, the (timed-out) ticketed press, then the release.
    let events: Vec<&str> = bridge.ws.calls.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(
        events,
        ["cpu.status", "input.buttons.press", "input.buttons.send"]
    );
    // The recovery release drives every button false so nothing stays held.
    let release = &bridge.ws.calls[2].1["buttons"];
    assert_eq!(release["cross"], false);
    assert_eq!(release["up"], false);
}

#[test]
fn press_buttons_ignores_a_stale_off_ticket_ack() {
    // A late ack from an earlier interrupted press (a different ticket) must not satisfy this
    // press — the bridge waits for its own ticket and queues the stale ack as ignored.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "input.buttons.press",
            json!({"event":"input.buttons.press","ticket":"stale-old"}),
        ),
        (
            "input.buttons.press",
            json!({"event":"input.buttons.press","ticket":"emucap-1"}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": 1}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    // The stale off-ticket ack was skipped (queued), not misattributed to this press.
    let drained = bridge.ws.drain_events();
    assert!(
        drained
            .iter()
            .any(|e| e.get("ticket") == Some(&json!("stale-old"))),
        "the stale ack should have been queued/ignored, not consumed as this press's reply"
    );
}

#[test]
fn press_buttons_rejects_while_cpu_halted() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "cpu.status",
        json!({"event":"cpu.status","stepping":true,"paused":false,"pc":0,"ticks":0}),
    )]));
    let resp = bridge.handle_request(Request::new(1, "press_buttons", json!({"buttons": ["a"]})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}

#[test]
fn press_buttons_rejects_frames_over_cap() {
    // A large hold (e.g. 10s ~= 600 frames at 60fps) would block call() past the bridge
    // binary's own 8s WS read timeout, reproducing the stale-reply misattribution race — must
    // be rejected up front, before any WS round trip (FakeWs has no replies queued at all).
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": MAX_PRESS_FRAMES + 1}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(
        bridge.ws.calls.is_empty(),
        "must reject before calling PPSSPP"
    );
}

#[test]
fn press_buttons_accepts_frames_at_the_cap() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false,"pc":0,"ticks":0}),
        ),
        (
            "input.buttons.press",
            json!({"event":"input.buttons.press","ticket":"emucap-1"}),
        ),
    ]));
    let resp = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": MAX_PRESS_FRAMES}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(bridge.ws.calls[1].1["duration"], MAX_PRESS_FRAMES);
}

#[test]
fn press_buttons_rejects_unknown_button() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["nonsense"]}),
    ));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(bridge.ws.calls.is_empty());
}

#[test]
fn press_buttons_requires_at_least_one_button() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(1, "press_buttons", json!({"buttons": []})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
    assert!(bridge.ws.calls.is_empty());
}

// --- save_state / load_state / reset ---

#[test]
fn save_state_calls_savestate_save_with_path() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "savestate.save",
        json!({"event":"savestate.save","path":"/tmp/x.ppst"}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "save_state",
        json!({"path": "/tmp/x.ppst"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["status"], "completed");
    assert_eq!(bridge.ws.calls[0].0, "savestate.save");
    assert_eq!(bridge.ws.calls[0].1["path"], "/tmp/x.ppst");
}

#[test]
fn save_state_requires_path() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(1, "save_state", json!({})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}

#[test]
fn call_with_timeout_below_a_slow_reply_budget_times_out_like_the_old_default() {
    // Reproduces the desync directly at the transport: the emucap fork's savestate handler can
    // take up to 15s to reply, so an 8s read budget times out mid-save (a bridge_error) while
    // PPSSPP is still working — but the dedicated savestate budget (>15s) tolerates it.
    let mut ws = FakeWs::with(&[(
        "savestate.save",
        json!({"event":"savestate.save","path":"/tmp/x.ppst","message":"Saved State"}),
    )]);
    ws.slow_replies
        .insert("savestate.save".into(), Duration::from_secs(15));
    // The old 8s default would have surfaced a spurious failure on a save that succeeds.
    assert!(ws
        .call_with_timeout(
            "savestate.save",
            json!({"path":"/tmp/x.ppst"}),
            Duration::from_secs(8)
        )
        .is_err());
    // The dedicated budget outlasts the fork's 15s wait, so the reply is consumed cleanly.
    assert!(ws
        .call_with_timeout(
            "savestate.save",
            json!({"path":"/tmp/x.ppst"}),
            SAVESTATE_READ_TIMEOUT
        )
        .is_ok());
}

#[test]
fn save_state_threads_a_read_budget_above_the_forks_15s_wait() {
    // A save that takes ~15s must not time out on the bridge side. The savestate call is given
    // a budget past the fork's 15s wait; with the old 8s default this save would spuriously
    // fail and leave PPSSPP's late reply to desync the next request.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "savestate.save",
        json!({"event":"savestate.save","path":"/tmp/x.ppst","message":"Saved State"}),
    )]));
    bridge
        .ws
        .slow_replies
        .insert("savestate.save".into(), Duration::from_secs(15));
    let resp = bridge.handle_request(Request::new(
        1,
        "save_state",
        json!({"path": "/tmp/x.ppst"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["status"], "completed");
    let (event, budget) = bridge.ws.call_timeouts.last().unwrap();
    assert_eq!(event, "savestate.save");
    assert!(
        *budget > Duration::from_secs(15),
        "savestate budget {budget:?} must outlast the fork's 15s wait"
    );
}

#[test]
fn load_state_threads_a_read_budget_above_the_forks_15s_wait() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "savestate.load",
        json!({"event":"savestate.load","path":"/tmp/x.ppst","message":"Loaded State"}),
    )]));
    bridge
        .ws
        .slow_replies
        .insert("savestate.load".into(), Duration::from_secs(15));
    let resp = bridge.handle_request(Request::new(
        1,
        "load_state",
        json!({"path": "/tmp/x.ppst"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let (event, budget) = bridge.ws.call_timeouts.last().unwrap();
    assert_eq!(event, "savestate.load");
    assert!(*budget > Duration::from_secs(15));
}

#[test]
fn load_state_calls_savestate_load_with_path() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "savestate.load",
        json!({"event":"savestate.load","path":"/tmp/x.ppst"}),
    )]));
    let resp = bridge.handle_request(Request::new(
        1,
        "load_state",
        json!({"path": "/tmp/x.ppst"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["status"], "completed");
    assert_eq!(bridge.ws.calls[0].0, "savestate.load");
    assert_eq!(bridge.ws.calls[0].1["path"], "/tmp/x.ppst");
}

#[test]
fn load_state_requires_path() {
    let mut bridge = PpssppBridge::new(FakeWs::with(&[]));
    let resp = bridge.handle_request(Request::new(1, "load_state", json!({})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}

#[test]
fn reset_calls_game_reset_with_reboot_budget_and_reports_post_reset_pc() {
    // Headless path: the fork blocks game.reset until the reboot completed and left the core
    // halted at the fresh boot entry, so the halt poll reads stepping on the first check and the
    // bridge reports a confirmed completion with the boot-entry pc.
    let mut bridge = PpssppBridge::new(FakeWs::with(&[
        ("game.reset", json!({"event":"game.reset"})),
        (
            "cpu.status",
            json!({"event":"cpu.status","stepping":true,"paused":false}),
        ),
        ("cpu.getAllRegs", gpr_only_pc(0x0880_4128)),
    ]));
    let resp = bridge.handle_request(Request::new(1, "reset", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["status"], "completed");
    // `status` is the single source of truth for whether the core is halted — no redundant
    // `stopped` boolean derivable from it.
    assert!(result.get("stopped").is_none());
    assert_eq!(result["post_reset_pc"], 0x0880_4128u32);
    assert_eq!(bridge.ws.calls[0].0, "game.reset");
    // game.reset must ride the extended reboot budget, not the default fail-fast read — an 8s
    // read would time out mid-reboot and desync the channel (like save_state).
    assert_eq!(
        bridge.ws.call_timeouts,
        vec![("game.reset".to_string(), RESET_READ_TIMEOUT)]
    );
}

#[test]
fn reset_display_session_reports_async_reboot_not_false_completed_while_running() {
    // display:true GUI session: the fork does NOT block game.reset (only the headless build
    // does), so the ack returns while the reboot is still queued on the GUI pump and the core
    // keeps running. The halt poll never reads stepping, so the bridge must report the async
    // reboot truthfully — NOT a false "completed" with the stale, still-in-game pc.
    let mut replies: Vec<(&str, Value)> = vec![("game.reset", json!({"event":"game.reset"}))];
    for _ in 0..RESET_HALT_POLLS {
        replies.push((
            "cpu.status",
            json!({"event":"cpu.status","stepping":false,"paused":false}),
        ));
    }
    let mut bridge = PpssppBridge::new(FakeWs::with(&replies));
    let resp = bridge.handle_request(Request::new(1, "reset", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_ne!(
        result["status"], "completed",
        "must not claim completed while the GUI reboot is still in flight"
    );
    assert_eq!(result["status"], "rebooting");
    // No redundant `stopped` boolean — `status` alone distinguishes rebooting (running) from
    // completed (halted).
    assert!(result.get("stopped").is_none());
    // No boot-entry pc is claimed while the core is still running the pre-reset game — the live
    // pc would be a stale, misleading value, not reset evidence.
    assert!(result.get("post_reset_pc").is_none());
}

// --- dump_memory ---

#[test]
fn dump_memory_writes_bin_and_regions_under_requested_directory() {
    // `main` (user RAM) streams in `MAX_READ_LEN` chunks; a fixed 0xAB byte per chunk lets the
    // test assert the whole region was concatenated in order. The bridge writes only the region
    // .bins + regions.json — state.json is the MCP host's job (src/live/tools.rs).
    let full_chunk_b64 =
        base64::engine::general_purpose::STANDARD.encode(vec![0xABu8; MAX_READ_LEN]);
    let mut replies: Vec<(&str, Value)> = Vec::new();
    let mut offset = 0u64;
    while offset < PSP_MAIN_RAM_SIZE {
        let chunk = MAX_READ_LEN.min((PSP_MAIN_RAM_SIZE - offset) as usize);
        // The region size is an exact multiple of MAX_READ_LEN, so every chunk is full-size.
        assert_eq!(chunk, MAX_READ_LEN);
        replies.push((
            "memory.read",
            json!({"event": "memory.read", "base64": full_chunk_b64.clone()}),
        ));
        offset += chunk as u64;
    }
    let mut bridge = PpssppBridge::new(FakeWs::with(&replies));
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dump");
    let resp = bridge.handle_request(Request::new(
        12,
        "dump_memory",
        json!({"path": out.to_str().unwrap()}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["regions"], 1);
    assert_eq!(result["path"], out.display().to_string());

    // main.bin: the whole 24 MiB region, every chunk concatenated in order.
    let bin = std::fs::read(out.join("main.bin")).unwrap();
    assert_eq!(bin.len() as u64, PSP_MAIN_RAM_SIZE);
    assert!(bin.iter().all(|&b| b == 0xAB));

    // regions.json: the canonical RegionMeta shape the cross-ROM diff loader consumes
    // (`src/analysis/dump.rs`).
    let regions: Value =
        serde_json::from_slice(&std::fs::read(out.join("regions.json")).unwrap()).unwrap();
    let arr = regions.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "main");
    assert_eq!(arr[0]["memory_type"], "main");
    assert_eq!(arr[0]["base_address"], PSP_MAIN_RAM_BASE);
    assert_eq!(arr[0]["size"], PSP_MAIN_RAM_SIZE);
}

#[test]
fn dump_memory_short_read_fails_without_writing_a_mismatched_bin() {
    // A `memory.read` reply that decodes to fewer bytes than requested is a short read: the dump
    // must fail rather than publish a `main.bin` smaller than the `PSP_MAIN_RAM_SIZE` its
    // `regions.json` advertises, and it must not leave any partial artifact behind.
    let short_b64 =
        base64::engine::general_purpose::STANDARD.encode(vec![0xABu8; MAX_READ_LEN - 1]);
    let mut bridge = PpssppBridge::new(FakeWs::with(&[(
        "memory.read",
        json!({"event": "memory.read", "base64": short_b64}),
    )]));
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dump");
    let resp = bridge.handle_request(Request::new(
        12,
        "dump_memory",
        json!({"path": out.to_str().unwrap()}),
    ));
    assert!(!resp.ok, "a short read must fail the dump");
    assert!(
        resp.error.unwrap().message.contains("short read"),
        "the error should name the short read"
    );
    assert!(
        !out.join("main.bin").exists(),
        "a short read must not leave a truncated main.bin"
    );
    assert!(!out.join("regions.json").exists());
}

#[test]
fn dump_memory_midstream_failure_preserves_the_prior_dump() {
    // A re-dump that fails part way through must not clobber a prior good dump with a truncated
    // `main.bin` beside a stale `regions.json`/`state.json`.
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dump");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(
        out.join("main.bin"),
        vec![0xCDu8; PSP_MAIN_RAM_SIZE as usize],
    )
    .unwrap();
    std::fs::write(out.join("regions.json"), b"[{\"name\":\"main\"}]").unwrap();
    // state.json is normally written by the host after the bridge returns; a prior one must
    // survive a failed re-dump too.
    std::fs::write(out.join("state.json"), b"{\"cpu.pc\":1}").unwrap();

    // New dump: first chunk reads fine, the second reply arrives with no base64 field → the read
    // errors mid-stream (models a dropped/garbled reply part way through the region).
    let full_chunk_b64 =
        base64::engine::general_purpose::STANDARD.encode(vec![0xABu8; MAX_READ_LEN]);
    let replies: Vec<(&str, Value)> = vec![
        (
            "memory.read",
            json!({"event": "memory.read", "base64": full_chunk_b64}),
        ),
        ("memory.read", json!({"event": "memory.read"})),
    ];
    let mut bridge = PpssppBridge::new(FakeWs::with(&replies));
    let resp = bridge.handle_request(Request::new(
        13,
        "dump_memory",
        json!({"path": out.to_str().unwrap()}),
    ));
    assert!(!resp.ok, "a mid-stream read failure must fail the dump");

    // The prior good dump is intact — byte-for-byte, metadata and all.
    let bin = std::fs::read(out.join("main.bin")).unwrap();
    assert_eq!(bin.len() as u64, PSP_MAIN_RAM_SIZE);
    assert!(
        bin.iter().all(|&b| b == 0xCD),
        "the prior main.bin must be preserved, not overwritten by a truncated new one"
    );
    assert_eq!(
        std::fs::read(out.join("regions.json")).unwrap(),
        b"[{\"name\":\"main\"}]"
    );
    assert!(
        out.join("state.json").exists(),
        "the prior state.json must survive a failed re-dump"
    );
    // The bridge writes region files directly into the requested dir (the host owns the atomic
    // dir swap), so a mid-stream failure must leave no `.partial` region temp behind in it.
    let leftovers: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".partial"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "partial region temp must be cleaned up on failure"
    );
}

// --- get_rom_info ---

#[test]
fn get_rom_info_reports_sha1_size_and_game_status() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "emucap-ppsspp-bridge-test-{}-reports-sha1.iso",
        std::process::id()
    ));
    std::fs::write(&path, b"hello psp").expect("write temp content");
    let mut bridge = PpssppBridge::with_content(
        FakeWs::with(&[(
            "game.status",
            json!({"event":"game.status","game":{"id":"ULJS00097","title":"Tales of Destiny 2"},"paused":false}),
        )]),
        Some(path.clone()),
    );
    let resp = bridge.handle_request(Request::new(1, "get_rom_info", json!({})));
    std::fs::remove_file(&path).ok();
    assert!(resp.ok, "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["system"], "psp");
    assert_eq!(result["adapter"], "ppsspp-rust-ws");
    // sha1("hello psp"), verified independently via `shasum -a1`.
    assert_eq!(result["sha1"], "51ce64b9e8869767e47fe87d9f13f5c626292273");
    assert_eq!(result["size"], 9);
    assert_eq!(result["game"]["id"], "ULJS00097");
    assert_eq!(result["game"]["title"], "Tales of Destiny 2");
}

#[test]
fn get_rom_info_requires_content_env() {
    let mut bridge = PpssppBridge::with_content(FakeWs::with(&[]), None);
    let resp = bridge.handle_request(Request::new(1, "get_rom_info", json!({})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}

#[test]
fn get_rom_info_rejects_missing_content_file() {
    let mut bridge = PpssppBridge::with_content(
        FakeWs::with(&[]),
        Some(std::path::PathBuf::from(
            "/nonexistent/emucap-ppsspp-test.iso",
        )),
    );
    let resp = bridge.handle_request(Request::new(1, "get_rom_info", json!({})));
    assert!(!resp.ok);
    assert_eq!(resp.error.unwrap().kind, "bad_params");
}
