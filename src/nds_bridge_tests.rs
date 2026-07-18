use super::*;
use crate::pc98_bridge::BridgeError;
use std::collections::VecDeque;

#[derive(Default)]
struct FakeGdb {
    replies: VecDeque<(String, String)>,
    calls: Vec<String>,
    nonblocking: VecDeque<String>,
    /// When set, `interrupt()` returns an error — models a core whose pause (SIGINT) fails.
    fail_interrupt: bool,
    /// When set, `recv_nonblocking()` returns an error — models a drain-stops socket failure.
    fail_nonblocking: bool,
}

impl FakeGdb {
    fn with(replies: &[(&str, &str)]) -> Self {
        Self {
            replies: replies
                .iter()
                .map(|(a, b)| ((*a).into(), (*b).into()))
                .collect(),
            ..Default::default()
        }
    }

    fn from_pairs(replies: Vec<(String, String)>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            ..Default::default()
        }
    }
}

impl GdbTransport for FakeGdb {
    fn send(&mut self, payload: &str) -> Result<String, BridgeError> {
        self.calls.push(payload.into());
        let Some((expected, reply)) = self.replies.pop_front() else {
            return Err(BridgeError::Emulator(format!(
                "unexpected fake GDB call: {payload}"
            )));
        };
        assert_eq!(payload, expected);
        Ok(reply)
    }

    fn send_no_reply(&mut self, payload: &str) -> Result<(), BridgeError> {
        self.calls.push(payload.into());
        Ok(())
    }

    fn interrupt(&mut self) -> Result<String, BridgeError> {
        if self.fail_interrupt {
            return Err(BridgeError::Emulator("fake interrupt failure".into()));
        }
        // A real interrupt reads the next packet off the socket: a pending stop is consumed here
        // (the loss the pause fix drains first). Otherwise the stub answers our SIGINT (S02).
        Ok(self.nonblocking.pop_front().unwrap_or_else(|| "S02".into()))
    }

    fn recv_nonblocking(&mut self) -> Result<Option<String>, BridgeError> {
        if self.fail_nonblocking {
            return Err(BridgeError::Emulator("fake nonblocking failure".into()));
        }
        Ok(self.nonblocking.pop_front())
    }
}

/// Build a 168-byte DeSmuME ARM `g` packet with the given r-registers and CPSR.
fn arm_regs_hex(regs: &[(usize, u32)], cpsr: u32) -> String {
    let mut bytes = vec![0u8; 168];
    for i in 0..16 {
        let value = regs
            .iter()
            .find(|(idx, _)| *idx == i)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[164..168].copy_from_slice(&cpsr.to_le_bytes());
    hex::encode(bytes)
}

fn bridge_arm9_only(replies: &[(&str, &str)]) -> NdsBridge<FakeGdb> {
    NdsBridge::new(FakeGdb::with(replies), None, BridgeEnv::default())
}

#[test]
fn looks_like_stray_stop_distinguishes_stops_from_base64() {
    // 실 stop 패킷: 스샷/디스어셈 base64 응답으로 오독되면 padding 에러를 내던 것들.
    assert!(looks_like_stray_stop("S05")); // "S"+2hex(정확히 3자) — base64로 디코드 시 길이 3 → padding 에러
    assert!(looks_like_stray_stop("S00"));
    assert!(looks_like_stray_stop("T05thread:1;0d:0000;")); // T-stop은 ';'/':'를 포함(base64엔 없는 문자)
    assert!(looks_like_stray_stop("T0b20:0102;"));
    // 실 base64 응답(길고 [A-Za-z0-9+/=]만): S/T로 시작해도 stop으로 오분류하면 안 된다.
    assert!(!looks_like_stray_stop("SGVsbG8=")); // "S..."로 시작하는 base64
    assert!(!looks_like_stray_stop("TWFuIGlzIGRpc3Rpbmd1aXNoZWQ=")); // "T..."지만 ';'/':' 없음
    assert!(!looks_like_stray_stop("iVBORw0KGgoAAAANSUhEUg==")); // 일반 PNG base64
    assert!(!looks_like_stray_stop("S0")); // 짧지만 "S"+2hex 형식 아님
}

#[test]
fn hello_advertises_only_tier1_truths() {
    let mut bridge = NdsBridge::new(
        FakeGdb::with(&[("?", "S05")]),
        Some(FakeGdb::with(&[("?", "S05")])),
        BridgeEnv {
            name: Some("nds".into()),
            ..Default::default()
        },
    );
    let response = bridge.handle_request(Request::new(1, "hello", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["adapter"], "desmume-nds-rust-gdb");
    assert_eq!(result["system"], "nds");
    assert_eq!(result["memory_types"], json!(["main", "arm9", "arm7"]));
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
        "read_memory",
        "get_state",
        "set_breakpoint",
        "poll_events",
        "step_instructions",
        "screenshot",
        "set_input",
        "press_buttons",
        "save_state",
        "load_state",
        "disassemble",
        "call_stack",
        "reset",
    ] {
        assert!(methods.iter().any(|m| m == wanted), "missing {wanted}");
    }
    for forbidden in ["run_frames", "probe", "set_trace", "watch_register"] {
        assert!(
            !methods.iter().any(|m| m == forbidden),
            "should not advertise {forbidden}"
        );
    }

    let caps = &result["capability_notes"];
    assert_eq!(caps["screenshot"], true);
    assert_eq!(caps["input"], true);
    assert_eq!(caps["frame_step"], false);
    assert_eq!(caps["breakpoints"], true);
    assert_eq!(caps["state_restore"], true);
    assert_eq!(caps["disassemble"], true);
    assert_eq!(caps["call_stack"], true);
    assert_eq!(caps["step_units"], json!(["instructions"]));

    let contracts = &result["contracts"];
    assert_eq!(contracts["catalog"], crate::contracts::CATALOG_ID);
    assert_eq!(
        contracts["active_exceptions"],
        json!([
            "nds.execution.frame-step-absent",
            "nds.call-stack.best-effort",
            "nds.input-hold.port-zero-only",
            "nds.input-pulse.constraints",
            "nds.input-touch.constraints"
        ])
    );
    assert!(contracts.get("authority").is_none());
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
}

#[test]
fn hello_omits_arm7_memory_type_when_arm7_absent() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response = bridge.handle_request(Request::new(1, "hello", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["memory_types"], json!(["main", "arm9"]));
    assert_eq!(result["cpus"], json!(["arm9"]));
}

#[test]
fn read_memory_maps_main_region_to_absolute_arm9_address() {
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("m2000000,4", "deadbeef")]);
    let response = bridge.handle_request(Request::new(
        2,
        "read_memory",
        json!({"memory_type": "main", "address": 0, "length": 4}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["hex"], "deadbeef");
    assert_eq!(result["cpu"], "arm9");
}

#[test]
fn write_memory_sends_m_packet_on_routed_cpu() {
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("M2000000,2:aabb", "OK")]);
    let response = bridge.handle_request(Request::new(
        3,
        "write_memory",
        json!({"memory_type": "main", "address": "0x0", "hex": "aabb"}),
    ));
    assert_eq!(response.result.unwrap()["written"], 2);
}

#[test]
fn get_state_decodes_arm_register_packet_little_endian() {
    let regs = arm_regs_hex(
        &[
            (0, 0x0000_0011),
            (13, 0x0380_0000),
            (14, 0x0200_1000),
            (15, 0x0200_0800),
        ],
        0x6000_00DF,
    );
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("g", &regs)]);
    let response = bridge.handle_request(Request::new(4, "get_state", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["cpu"], "arm9");
    let state = &result["state"];
    assert_eq!(state["cpu.r0"], 0x11);
    assert_eq!(state["cpu.r15"], 0x0200_0800);
    assert_eq!(state["cpu.pc"], 0x0200_0800);
    assert_eq!(state["cpu.sp"], 0x0380_0000);
    assert_eq!(state["cpu.lr"], 0x0200_1000);
    assert_eq!(state["cpu.cpsr"], 0x6000_00DF);
}

#[test]
fn set_breakpoint_sends_z0_and_tracks_id() {
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("Z0,2000100,4", "OK")]);
    let set = bridge.handle_request(Request::new(
        5,
        "set_breakpoint",
        json!({"memory_type": "main", "address": 0x100}),
    ));
    let set = set.result.unwrap();
    assert_eq!(set["id"], 1);
    assert_eq!(set["address"], 0x0200_0100);
    assert_eq!(set["cpu"], "arm9");

    let list = bridge.handle_request(Request::new(6, "list_breakpoints", json!({})));
    assert_eq!(
        list.result.unwrap()["breakpoints"],
        json!([{
            "id": 1,
            "cpu": "arm9",
            "kind": "exec",
            "address": 0x0200_0100,
            "hardware": false,
        }])
    );
}

#[test]
fn arm7_memory_type_routes_to_arm7_connection() {
    // ARM9 only handles the handshake; the read must land on the ARM7 stub.
    let arm9 = FakeGdb::with(&[("?", "S05")]);
    let arm7 = FakeGdb::with(&[("?", "S05"), ("m3800000,4", "cafef00d")]);
    let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
    let response = bridge.handle_request(Request::new(
        7,
        "read_memory",
        json!({"memory_type": "arm7", "address": 0x0380_0000, "length": 4}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["hex"], "cafef00d");
    assert_eq!(result["cpu"], "arm7");
    // ARM9 stub saw only the handshake.
    assert_eq!(bridge.arm9.gdb.calls, vec!["?".to_string()]);
}

#[test]
fn arm7_memory_type_errors_when_arm7_not_attached() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response = bridge.handle_request(Request::new(
        8,
        "read_memory",
        json!({"memory_type": "arm7", "address": 0, "length": 4}),
    ));
    assert!(!response.ok);
    assert!(response.error.unwrap().message.contains("ARM7"));
}

#[test]
fn step_instructions_single_steps_then_reports_pc() {
    let regs = arm_regs_hex(&[(15, 0x0200_0004)], 0);
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("s", "S05"), ("g", &regs)]);
    let response = bridge.handle_request(Request::new(9, "step_instructions", json!({"count": 1})));
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["unit"], "instructions");
    assert_eq!(result["count"], 1);
    assert_eq!(result["pc"], 0x0200_0004);
    assert_eq!(
        bridge
            .arm9
            .gdb
            .calls
            .iter()
            .filter(|c| c.as_str() == "s")
            .count(),
        1
    );
}

#[test]
fn step_instructions_rejects_over_sync_cap_before_backend_calls() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    bridge.arm9.gdb.calls.clear();
    let response = bridge.handle_request(Request::new(
        9,
        "step_instructions",
        json!({"count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT + 1}),
    ));

    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert!(bridge.arm9.gdb.calls.is_empty());
}

#[test]
fn step_method_treats_frames_with_instructions_unit_as_instruction_count() {
    // Older hosts may send `{frames:N, unit:"instructions"}` to wire `step`.
    // That must run as an instruction step, not be rejected as a frame step.
    let regs = arm_regs_hex(&[(15, 0x0200_0008)], 0);
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("s", "S05"), ("g", &regs)]);
    let response = bridge.handle_request(Request::new(
        9,
        "step",
        json!({"frames": 1, "unit": "instructions"}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["count"], 1);
    assert_eq!(
        bridge
            .arm9
            .gdb
            .calls
            .iter()
            .filter(|c| c.as_str() == "s")
            .count(),
        1
    );
}

#[test]
fn step_method_rejects_bare_frames_as_unsupported_frame_step() {
    // A bare {frames:N} (the frame-step tool, no unit) has no NDS meaning → reject, do not
    // silently run N instructions.
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response = bridge.handle_request(Request::new(9, "step", json!({"frames": 60})));
    assert!(!response.ok);
    assert!(response
        .error
        .unwrap()
        .message
        .contains("프레임 step 미지원"));
}

#[test]
fn is_interrupt_stop_matches_sigint_only() {
    assert!(is_interrupt_stop("S02")); // SIGINT = our pause
    assert!(is_interrupt_stop("T02thread:1;")); // T-form SIGINT
    assert!(!is_interrupt_stop("S05")); // SIGTRAP = breakpoint, reportable
    assert!(!is_interrupt_stop("T05thread:1;"));
    assert!(!is_interrupt_stop("OK"));
}

#[test]
fn note_stop_drops_sigint_keeps_sigtrap() {
    // with_frozen pauses on every data command; those SIGINT (S02) stops must not flood the
    // poll_events queue and bury a real breakpoint hit (S05).
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    bridge.arm9.note_stop("S02".into());
    assert!(
        bridge.arm9.events.is_empty(),
        "SIGINT (S02) must not enter the event queue"
    );
    bridge.arm9.note_stop("S05".into());
    assert_eq!(
        bridge.arm9.events.len(),
        1,
        "SIGTRAP (S05) breakpoint stop must be reported"
    );
}

#[test]
fn reset_while_running_halts_core_without_resuming() {
    // reset's contract is state:"frozen" — it must leave the core actually halted. If ARM9 is
    // running, reset must NOT resume it (send_cmd's with_frozen would) while still claiming
    // frozen; that mismatch sends the next command to a running stub and desyncs. Assert no `c`
    // (resume) is emitted and the reported state matches reality.
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,reset", "OK")]);
    bridge.arm9.frozen = false; // simulate a running core (e.g. HITL resume-both)
    let response = bridge.handle_request(Request::new(9, "reset", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["state"], "frozen");
    assert!(
        bridge.arm9.frozen,
        "ARM9 must actually be halted after reset"
    );
    assert!(
        !bridge.arm9.gdb.calls.iter().any(|c| c == "c"),
        "reset must not resume the core; calls = {:?}",
        bridge.arm9.gdb.calls
    );
}

#[test]
fn reset_from_frozen_completes_without_resuming() {
    // The normal path (already frozen) must still work and never emit a resume.
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,reset", "OK")]);
    let response = bridge.handle_request(Request::new(9, "reset", json!({})));
    assert_eq!(response.result.unwrap()["state"], "frozen");
    assert!(bridge.arm9.frozen);
    assert!(!bridge.arm9.gdb.calls.iter().any(|c| c == "c"));
}

#[test]
fn read_memory_rejects_out_of_range_main_offset() {
    // main is 4 MB; without the bound, route() wraps a past-the-end offset into unrelated DS bus
    // space via absolute addressing. Reject instead.
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let r = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "main", "address": 0x0040_0000, "length": 4}),
    ));
    assert!(!r.ok);
    assert_eq!(r.error.unwrap().kind, "bad_params");
}

#[test]
fn read_memory_rejects_length_over_cap() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let r = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "arm9", "address": 0, "length": 0x30_0000}),
    ));
    assert!(!r.ok);
    assert_eq!(r.error.unwrap().kind, "bad_params");
}

#[test]
fn read_memory_accepts_in_range_main() {
    // main+0 for 4 bytes maps to the ARM9 bus at 0x0200_0000 and reaches the stub.
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("m2000000,4", "aabbccdd")]);
    let r = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "main", "address": 0, "length": 4}),
    ));
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(r.result.unwrap()["hex"], "aabbccdd");
}

#[test]
fn pending_breakpoint_stop_survives_a_data_command() {
    // Scope: a breakpoint hits while the bridge still believes the core is running; the data
    // command's with_frozen pause must not swallow the pending S05, so poll_events still reports
    // it. Register/state correctness at the stop is out of scope here — the fake stub's `g` reply
    // is static and can't model the core advancing past the breakpoint; the live core owns
    // that transition.
    let regs = arm_regs_hex(&[(15, 0x0200_0000)], 0);
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("g", &regs)]);
    bridge.arm9.frozen = false; // bridge believes the core is running
    bridge.arm9.gdb.nonblocking.push_back("S05".into()); // a breakpoint hit is pending
    let _ = bridge.handle_request(Request::new(1, "get_state", json!({})));
    let events = bridge
        .handle_request(Request::new(2, "poll_events", json!({})))
        .result
        .unwrap();
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["signal"] == "05"),
        "pending breakpoint hit was lost: {events:?}"
    );
}

#[test]
fn poll_events_bad_filter_does_not_drop_buffered_hit() {
    // A malformed `breakpoint_id` filter must be rejected BEFORE poll_events drains the stop
    // sockets or `mem::take`s the queues: otherwise the `?` early-return drops every just-drained
    // and previously-held event forever. Here a breakpoint hit is already buffered; a bad filter
    // must error without consuming it, and the hit must surface on the next valid poll.
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    bridge.events.push(json!({
        "type": "breakpoint_hit",
        "signal": "05",
        "id": 7,
    }));
    let bad = bridge.handle_request(Request::new(
        1,
        "poll_events",
        json!({"breakpoint_id": "abc"}),
    ));
    assert!(!bad.ok, "malformed breakpoint_id must be rejected");
    assert_eq!(bad.error.unwrap().kind, "bad_params");
    let good = bridge
        .handle_request(Request::new(2, "poll_events", json!({})))
        .result
        .unwrap();
    assert!(
        good["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == 7 && e["signal"] == "05"),
        "buffered breakpoint hit was lost by the bad-filter poll: {good:?}"
    );
}

#[test]
fn pending_breakpoint_stop_leaves_core_halted_after_data_command() {
    // A real exec-breakpoint stop (S05) is pending on the socket while the bridge still believes
    // the core is running. A data command's with_frozen pause drains that stop (preserving the
    // event) but must NOT auto-resume past it: the bridge only caused the pause when it injected
    // an interrupt, not when a real stop was drained. So the core stays halted, no `c` is sent,
    // and enrichment reads the true stopped PC (0x0200_0000) — matching the exec breakpoint.
    let regs = arm_regs_hex(&[(15, 0x0200_0000)], 0);
    let mut bridge = bridge_arm9_only(&[
        ("?", "S05"),
        ("Z0,2000000,4", "OK"),
        ("g", &regs),
        ("g", &regs), // enrich_event re-reads regs at poll time
    ]);
    let set = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"cpu": "arm9", "address": "0x2000000", "kind": "exec"}),
    ));
    let bp_id = set.result.unwrap()["id"].as_u64().unwrap();
    bridge.arm9.frozen = false; // bridge believes the core is running
    bridge.arm9.gdb.nonblocking.push_back("S05".into()); // a breakpoint hit is pending

    let _ = bridge.handle_request(Request::new(2, "get_state", json!({})));
    assert!(
        bridge.arm9.frozen,
        "core must stay halted at the breakpoint, not be resumed past it"
    );
    assert!(
        !bridge.arm9.gdb.calls.iter().any(|c| c == "c"),
        "no continue may be sent after draining a real breakpoint stop: {:?}",
        bridge.arm9.gdb.calls
    );

    let events = bridge
        .handle_request(Request::new(3, "poll_events", json!({})))
        .result
        .unwrap();
    let arr = events["events"].as_array().unwrap();
    let hit = arr
        .iter()
        .find(|e| e["signal"] == "05")
        .expect("pending breakpoint hit was lost");
    // State stays consistent: the halted PC still matches the breakpoint, so it is attributed.
    assert_eq!(hit["type"], "breakpoint_hit");
    assert_eq!(hit["breakpoint_id"], bp_id);
    assert_eq!(hit["address"], 0x0200_0000);
}

#[test]
fn data_command_resumes_running_core_when_no_stop_was_pending() {
    // Non-regression for the pause-fix: when the bridge itself injects the pause (no real stop
    // is pending), with_frozen must still resume the core afterwards so it keeps running.
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("m2000000,4", "aabbccdd")]);
    bridge.arm9.frozen = false; // running, nothing pending
    let r = bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type": "main", "address": 0, "length": 4}),
    ));
    assert!(r.ok, "{:?}", r.error);
    assert!(
        !bridge.arm9.frozen,
        "core the bridge paused itself must be resumed back to running"
    );
    assert!(
        bridge.arm9.gdb.calls.iter().any(|c| c == "c"),
        "a bridge-injected pause must be undone with a continue: {:?}",
        bridge.arm9.gdb.calls
    );
}

#[test]
fn unsupported_method_returns_unsupported_error_kind() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    for method in ["run_frames", "probe", "set_trace", "watch_register"] {
        let response = bridge.handle_request(Request::new(10, method, json!({})));
        assert!(!response.ok, "{method} should fail");
        let error = response.error.unwrap();
        assert_eq!(error.kind, "unsupported", "{method} kind");
        assert!(error.message.contains("unsupported on nds"), "{method} msg");
    }
}

#[test]
fn screenshot_sends_query_and_returns_png_base64() {
    let png = b"\x89PNG\r\n\x1a\nDESMUME-TEST-BYTES";
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("qEmucap,ss", b64.as_str())]);
    let response = bridge.handle_request(Request::new(1, "screenshot", json!({})));
    assert!(response.ok, "screenshot failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["png_base64"], b64);
    assert_eq!(result["format"], "png");
    assert_eq!(result["width"], 256);
    assert_eq!(result["height"], 384);
    assert!(bridge.arm9.gdb.calls.iter().any(|c| c == "qEmucap,ss"));
}

#[test]
fn screenshot_rejects_non_png_reply() {
    let b64 = base64::engine::general_purpose::STANDARD.encode(b"not a png");
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("qEmucap,ss", b64.as_str())]);
    let response = bridge.handle_request(Request::new(1, "screenshot", json!({})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "emulator_error");
}

#[test]
fn set_input_sends_mask_for_a_and_b() {
    // a=bit0, b=bit1 -> 0b11 = 0x3
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,input:3", "OK")]);
    let response =
        bridge.handle_request(Request::new(1, "set_input", json!({"buttons": ["a", "b"]})));
    assert!(response.ok, "set_input failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["buttons"], json!(["a", "b"]));
    assert_eq!(result["cpu"], "arm9");
}

#[test]
fn set_input_maps_shoulder_and_dpad_bits() {
    // left=bit5 (0x20), r shoulder=bit8 (0x100), start=bit3 (0x8) -> 0x128
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,input:128", "OK")]);
    let response = bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"buttons": ["left", "r", "start"]}),
    ));
    assert!(response.ok, "set_input failed: {:?}", response.error);
}

#[test]
fn set_input_empty_releases_with_zero_mask() {
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,input:0", "OK")]);
    let response = bridge.handle_request(Request::new(1, "set_input", json!({"buttons": []})));
    assert!(response.ok, "release failed: {:?}", response.error);
}

#[test]
fn zero_remaining_override_is_reported_as_native_input() {
    assert_eq!(
        override_status_json(Some(0)),
        json!({
            "observable": true,
            "engaged": false,
            "mode": "native",
            "remaining_frames": 0,
        })
    );
}

#[test]
fn input_methods_reject_nonzero_port_before_gdb_mutation() {
    for (method, params) in [
        ("set_input", json!({"port": 1, "buttons": ["a"]})),
        (
            "press_buttons",
            json!({"port": 1, "buttons": ["a"], "frames": 1}),
        ),
        ("touch", json!({"port": 1, "x": 10, "y": 20})),
    ] {
        let mut bridge = bridge_arm9_only(&[("?", "S05")]);
        let response = bridge.handle_request(Request::new(1, method, params));
        assert!(!response.ok, "{method} must reject port 1");
        assert_eq!(response.error.unwrap().kind, "bad_params");
        assert_eq!(
            bridge.arm9.gdb.calls,
            ["?"],
            "{method} mutated the emulator before rejecting the port"
        );
    }
}

#[test]
fn press_buttons_encodes_mask_and_frames() {
    // a=bit0 -> mask 1, frames 3 -> "QEmucap,input:1,3"
    let mut bridge = bridge_arm9_only(&[
        ("?", "S05"),
        ("QEmucap,input:1,3", "OK"),
        ("qEmucap,inputstatus", "2"),
        ("qEmucap,inputstatus", "0"),
    ]);
    let response = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": 3}),
    ));
    assert!(response.ok, "press failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["frames"], 3);
    assert_eq!(result["frames_elapsed"], 3);
    assert_eq!(result["buttons"], json!(["a"]));
    assert_eq!(result["override_engaged"], false);
    assert!(!bridge.arm9.frozen, "frozen press must atomically resume");
}

#[test]
fn press_buttons_requires_a_button() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response = bridge.handle_request(Request::new(1, "press_buttons", json!({"buttons": []})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
}

#[test]
fn unknown_button_is_rejected() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response =
        bridge.handle_request(Request::new(1, "set_input", json!({"buttons": ["turbo"]})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
}

#[test]
fn touch_sends_hex_coords() {
    // x=128 (0x80), y=96 (0x60), no frames -> hold "QEmucap,touch:80,60"
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,touch:80,60", "OK")]);
    let response = bridge.handle_request(Request::new(1, "touch", json!({"x": 128, "y": 96})));
    assert!(response.ok, "touch failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["x"], 128);
    assert_eq!(result["y"], 96);
}

#[test]
fn touch_with_frames_is_a_tap() {
    // x=10 (0xa), y=20 (0x14), frames=5 -> "QEmucap,touch:a,14,5"
    let mut bridge = bridge_arm9_only(&[
        ("?", "S05"),
        ("QEmucap,touch:a,14,5", "OK"),
        ("qEmucap,touchstatus", "0"),
    ]);
    let response = bridge.handle_request(Request::new(
        1,
        "touch",
        json!({"x": 10, "y": 20, "frames": 5}),
    ));
    assert!(response.ok, "touch failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["frames"], 5);
    assert_eq!(result["override_engaged"], false);
}

#[test]
fn timed_input_interruption_releases_override_before_reply() {
    let mut bridge = bridge_arm9_only(&[
        ("?", "S05"),
        ("QEmucap,input:1,3", "OK"),
        ("qEmucap,inputstatus", "2"),
        ("QEmucap,input:0", "OK"),
    ]);
    // The request starts frozen, arms input, then resumes. A real stop waiting at the first
    // terminal-status poll must halt the core and force a release before interrupted returns.
    bridge.arm9.gdb.nonblocking.push_back("S05".into());
    let response = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": 3}),
    ));
    assert!(
        response.ok,
        "interruption should be a terminal result: {:?}",
        response.error
    );
    let result = response.result.unwrap();
    assert_eq!(result["status"], "interrupted");
    assert_eq!(result["reason"], "breakpoint");
    assert_eq!(result["frames_elapsed"], 1);
    assert_eq!(result["override_engaged"], false);
    assert!(bridge.arm9.frozen);
    assert!(bridge
        .arm9
        .gdb
        .calls
        .iter()
        .any(|call| call == "QEmucap,input:0"));
}

#[test]
fn timed_input_release_and_stop_same_poll_reports_interrupted() {
    let mut bridge = bridge_arm9_only(&[
        ("?", "S05"),
        ("QEmucap,input:1,3", "OK"),
        ("qEmucap,inputstatus", "0"),
        ("QEmucap,input:0", "OK"),
    ]);
    bridge.arm9.gdb.nonblocking.push_back("S05".into());

    let response = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": 3}),
    ));

    assert!(response.ok, "same-frame stop is a terminal interruption");
    let result = response.result.unwrap();
    assert_eq!(result["status"], "interrupted");
    assert_eq!(result["frames_elapsed"], 3);
    assert_eq!(result["state"], "frozen");
    assert_eq!(result["override_engaged"], false);
    assert!(bridge.arm9.frozen);
}

#[test]
fn timed_input_over_sync_bound_is_rejected_before_arming() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response = bridge.handle_request(Request::new(
        1,
        "press_buttons",
        json!({"buttons": ["a"], "frames": MAX_SYNC_TIMED_INPUT_FRAMES + 1}),
    ));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert!(!bridge
        .arm9
        .gdb
        .calls
        .iter()
        .any(|call| call.starts_with("QEmucap,input:")));
}

#[test]
fn touch_release_lifts() {
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,touch:release", "OK")]);
    let response = bridge.handle_request(Request::new(1, "touch", json!({"release": true})));
    assert!(response.ok, "touch release failed: {:?}", response.error);
    assert_eq!(response.result.unwrap()["released"], true);
}

#[test]
fn touch_out_of_range_is_rejected() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response = bridge.handle_request(Request::new(1, "touch", json!({"x": 300, "y": 96})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
}

#[test]
fn touch_requires_coords() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response = bridge.handle_request(Request::new(1, "touch", json!({"y": 96})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
}

#[test]
fn unknown_method_uses_unknown_method_kind() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let response = bridge.handle_request(Request::new(11, "not_a_method", json!({})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "unknown_method");
}

#[test]
fn write_memory_accepts_desmume_empty_reply() {
    // DeSmuME performs the write but answers `M` with an empty packet, not "OK".
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("M2100000,4:deadbeef", "")]);
    let response = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "main", "address": 0x100000, "hex": "deadbeef"}),
    ));
    assert!(
        response.ok,
        "empty M reply is success: {:?}",
        response.error
    );
    assert_eq!(response.result.unwrap()["written"], json!(4));
}

#[test]
fn write_memory_rejects_error_reply() {
    // A real error code (bad address) is still an error, not silently accepted.
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("M2100000,4:deadbeef", "E02")]);
    let response = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "main", "address": 0x100000, "hex": "deadbeef"}),
    ));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "emulator_error");
}

#[test]
fn resume_defaults_to_arm9_only() {
    // ARM9-primary: continuing both cores is racy in DeSmuME's lockstep (the un-broken core
    // drags the broken one past its breakpoint), so a bare resume continues only ARM9.
    let mut bridge = NdsBridge::new(
        FakeGdb::with(&[("?", "S05")]),
        Some(FakeGdb::with(&[("?", "S05")])),
        BridgeEnv::default(),
    );
    let response = bridge.handle_request(Request::new(1, "resume", json!({})));
    assert!(response.ok);
    let cpus = response.result.unwrap()["cpus"].clone();
    assert_eq!(cpus.get("arm9").and_then(|v| v.as_str()), Some("running"));
    assert!(
        cpus.get("arm7").is_none(),
        "arm7 must not resume by default"
    );
}

#[test]
fn resume_both_opts_into_dual_continue() {
    let mut bridge = NdsBridge::new(
        FakeGdb::with(&[("?", "S05")]),
        Some(FakeGdb::with(&[("?", "S05")])),
        BridgeEnv::default(),
    );
    let response = bridge.handle_request(Request::new(1, "resume", json!({"cpu": "both"})));
    assert!(response.ok);
    let cpus = response.result.unwrap()["cpus"].clone();
    assert_eq!(cpus.get("arm9").and_then(|v| v.as_str()), Some("running"));
    assert_eq!(cpus.get("arm7").and_then(|v| v.as_str()), Some("running"));
}

#[test]
fn save_state_sends_hex_encoded_savestate_command() {
    let path = "/tmp/s.dsv";
    let cmd = format!("QEmucap,savestate:{}", hex::encode(path));
    let mut bridge = bridge_arm9_only(&[("?", "S05"), (cmd.as_str(), "OK")]);
    let response = bridge.handle_request(Request::new(1, "save_state", json!({ "path": path })));
    assert!(response.ok, "save_state failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["path"], path);
    assert_eq!(result["status"], "completed");
    assert!(bridge.arm9.gdb.calls.iter().any(|c| c == &cmd));
}

#[test]
fn reset_sends_reset_command_and_reports_frozen() {
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("QEmucap,reset", "OK")]);
    let response = bridge.handle_request(Request::new(1, "reset", json!({})));
    assert!(response.ok, "reset failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["state"], "frozen");
    assert!(bridge.arm9.gdb.calls.iter().any(|c| c == "QEmucap,reset"));
}

#[test]
fn load_state_sends_hex_encoded_loadstate_command() {
    let path = "/tmp/s.dsv";
    let cmd = format!("QEmucap,loadstate:{}", hex::encode(path));
    let mut bridge = bridge_arm9_only(&[("?", "S05"), (cmd.as_str(), "OK")]);
    let response = bridge.handle_request(Request::new(1, "load_state", json!({ "path": path })));
    assert!(response.ok, "load_state failed: {:?}", response.error);
    assert_eq!(response.result.unwrap()["status"], "completed");
}

#[test]
fn save_state_surfaces_emulator_error_on_e01() {
    let path = "/bad/s.dsv";
    let cmd = format!("QEmucap,savestate:{}", hex::encode(path));
    let mut bridge = bridge_arm9_only(&[("?", "S05"), (cmd.as_str(), "E01")]);
    let response = bridge.handle_request(Request::new(1, "save_state", json!({ "path": path })));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "emulator_error");
}

#[test]
fn disassemble_sends_query_and_parses_little_endian_bytes() {
    // Fork emits "<addr>|<opcode-value-hex>|<text>" per line, base64-encoded.
    let block = "2000000|e3a00001|mov r0, #1\n2000004|e12fff1e|bx lr\n";
    let b64 = base64::engine::general_purpose::STANDARD.encode(block);
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("qEmucap,disasm:2000000,2", b64.as_str())]);
    let response = bridge.handle_request(Request::new(
        1,
        "disassemble",
        json!({ "address": 0x0200_0000u64, "count": 2 }),
    ));
    assert!(response.ok, "disassemble failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["cpu"], "arm9");
    let insns = result["instructions"].as_array().unwrap();
    assert_eq!(insns.len(), 2);
    assert_eq!(insns[0]["addr"], 0x0200_0000u64);
    assert_eq!(insns[0]["text"], "mov r0, #1");
    // e3a00001 in memory is little-endian: 01 00 a0 e3.
    assert_eq!(insns[0]["bytes"], "0100a0e3");
    assert_eq!(insns[1]["addr"], 0x0200_0004u64);
    assert_eq!(insns[1]["text"], "bx lr");
    assert_eq!(insns[1]["bytes"], "1eff2fe1");
}

#[test]
fn disassemble_passes_thumb_mode_to_fork() {
    let block = "2000000|2001|movs r0, #1\n";
    let b64 = base64::engine::general_purpose::STANDARD.encode(block);
    let mut bridge =
        bridge_arm9_only(&[("?", "S05"), ("qEmucap,disasm:2000000,1,t", b64.as_str())]);
    let response = bridge.handle_request(Request::new(
        1,
        "disassemble",
        json!({ "address": 0x0200_0000u64, "count": 1, "mode": "thumb" }),
    ));
    assert!(
        response.ok,
        "disassemble thumb failed: {:?}",
        response.error
    );
    let result = response.result.unwrap();
    assert_eq!(result["mode"], "thumb");
    // 2001 in memory little-endian: 01 20.
    assert_eq!(result["instructions"][0]["bytes"], "0120");
}

#[test]
fn call_stack_walks_lr_then_fp_chain_over_g_and_m() {
    // pc/lr in main RAM (code region); sp/fp in WRAM. One valid frame-pointer frame,
    // then a saved lr outside code space terminates the walk.
    let regs = arm_regs_hex(
        &[
            (11, 0x0300_0100), // fp
            (13, 0x0300_0000), // sp (stack top)
            (14, 0x0200_0200), // lr
            (15, 0x0200_0100), // pc
        ],
        0,
    );
    let mut bridge = bridge_arm9_only(&[
        ("?", "S05"),
        ("g", &regs),
        // iter 1: [fp-4]=saved lr=0x02000300, [fp-12]=saved fp=0x03000200
        ("m30000fc,4", "00030002"),
        ("m30000f4,4", "00020003"),
        // iter 2: [fp-4]=saved lr=0 (out of code region -> stop), [fp-12]=0
        ("m30001fc,4", "00000000"),
        ("m30001f4,4", "00000000"),
    ]);
    let response = bridge.handle_request(Request::new(1, "call_stack", json!({})));
    assert!(response.ok, "call_stack failed: {:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["cpu"], "arm9");
    assert_eq!(result["method"], "lr+fp-walk (best-effort)");
    let frames = result["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 3, "pc + lr + one fp-walk frame");
    assert_eq!(frames[0]["pc"], 0x0200_0100u64);
    assert_eq!(frames[0]["kind"], "pc");
    assert_eq!(frames[1]["pc"], 0x0200_0200u64);
    assert_eq!(frames[1]["kind"], "lr");
    assert_eq!(frames[2]["pc"], 0x0200_0300u64);
    assert_eq!(frames[2]["kind"], "fp-walk");
}

#[test]
fn call_stack_without_frame_pointer_returns_pc_and_lr_only() {
    // r11 = 0 (no frame pointer) -> walk contributes nothing; only pc + lr frames.
    let regs = arm_regs_hex(&[(14, 0x0200_0200), (15, 0x0200_0100)], 0);
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("g", &regs)]);
    let response = bridge.handle_request(Request::new(1, "call_stack", json!({})));
    assert!(response.ok, "call_stack failed: {:?}", response.error);
    let frames = response.result.unwrap()["frames"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["kind"], "pc");
    assert_eq!(frames[1]["kind"], "lr");
}

#[test]
fn opcode_hex_to_le_bytes_reverses_byte_order() {
    assert_eq!(opcode_hex_to_le_bytes("e3a00001"), "0100a0e3");
    assert_eq!(opcode_hex_to_le_bytes("2001"), "0120");
}

#[test]
fn find_pattern_scans_main_region_with_match_limit() {
    // main+0 maps to the ARM9 bus at 0x0200_0000. "aa00" occurs at rel offsets 0,2,4,6;
    // max_matches=2 keeps [0,2] and marks the scan truncated.
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("m2000000,8", "aa00aa00aa00aa00")]);
    let response = bridge.handle_request(Request::new(
        7,
        "find_pattern",
        json!({"memory_type":"main","start":0,"length":8,"hex":"aa00","max_matches":2}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["matches"], json!([0, 2]));
    assert_eq!(result["count"], 2);
    assert_eq!(result["truncated_matches"], true);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["cpu"], "arm9");
}

#[test]
fn find_pattern_absent_returns_no_matches() {
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("m2000000,8", "1122334455667788")]);
    let response = bridge.handle_request(Request::new(
        8,
        "find_pattern",
        json!({"memory_type":"main","start":0,"length":8,"hex":"aa00"}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["matches"], json!([]));
    assert_eq!(result["count"], 0);
    assert_eq!(result["truncated"], false);
}

#[test]
fn find_pattern_offsets_are_region_relative_to_start() {
    // start=4 within main reads from 0x0200_0004; the pattern sits at buffer offset 2, so the
    // reported match is start(4)+2 = 6 — a region-relative offset, matching the pc98 shape.
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("m2000004,4", "0000aa00")]);
    let response = bridge.handle_request(Request::new(
        9,
        "find_pattern",
        json!({"memory_type":"main","start":4,"length":4,"hex":"aa00"}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["matches"], json!([6]));
    assert_eq!(result["count"], 1);
}

#[test]
fn dump_memory_writes_regions_under_requested_directory() {
    // Feed a full zero read for every dumpable region, then assert the .bin sizes, the
    // regions.json manifest keys, and the returned region count.
    let mut replies = vec![("?".to_string(), "S05".to_string())];
    let dump_regions: Vec<NdsRegion> = MEMORY_REGIONS
        .iter()
        .copied()
        .filter(|r| r.dumpable && r.cpu == CpuId::Arm9)
        .collect();
    for region in &dump_regions {
        let mut offset = 0usize;
        while offset < region.size as usize {
            let chunk = MAX_READ_CHUNK.min(region.size as usize - offset);
            replies.push((
                format!("m{:x},{:x}", region.base as usize + offset, chunk),
                "00".repeat(chunk),
            ));
            offset += chunk;
        }
    }
    let mut bridge = bridge_arm9_only_pairs(replies);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dump");
    let response = bridge.handle_request(Request::new(
        12,
        "dump_memory",
        json!({"path": out.to_str().unwrap()}),
    ));
    assert!(response.ok, "dump failed: {:?}", response.error);
    assert_eq!(response.result.unwrap()["regions"], dump_regions.len());

    let regions: Value =
        serde_json::from_slice(&std::fs::read(out.join("regions.json")).unwrap()).unwrap();
    let regions = regions.as_array().unwrap();
    assert_eq!(regions.len(), dump_regions.len());
    let main_meta = regions.iter().find(|r| r["name"] == "main").unwrap();
    assert_eq!(main_meta["memory_type"], "main");
    assert_eq!(main_meta["base_address"], 0x0200_0000u64);
    assert_eq!(main_meta["size"], 0x0040_0000u64);
    assert_eq!(
        std::fs::metadata(out.join("main.bin")).unwrap().len(),
        memory_region("main").unwrap().size
    );
}

#[test]
fn dump_memory_short_read_fails_without_partial_bin() {
    // A stub read that returns fewer bytes than requested must abort the dump cleanly: no
    // partial main.bin, no leftover temp, and no regions.json — the length check catches it
    // before anything is placed on disk.
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("m2000000,2000", "00")]); // 1 byte, want MAX_READ_CHUNK
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dump");
    let response = bridge.handle_request(Request::new(
        13,
        "dump_memory",
        json!({"path": out.to_str().unwrap()}),
    ));
    assert!(!response.ok, "short read must fail the dump");
    assert_eq!(response.error.unwrap().kind, "emulator_error");
    assert!(
        !out.join("main.bin").exists(),
        "a short read must not leave a partial main.bin"
    );
    assert!(
        !out.join(".main.bin.partial").exists(),
        "the temp file must not be left behind"
    );
    assert!(
        !out.join("regions.json").exists(),
        "regions.json must not be written when a region read fails"
    );
}

#[test]
fn shared_read_freezes_running_arm7_then_restores_it() {
    // `main` is shared Main RAM both cores write. ARM7 is an independent core that HITL resumes
    // alongside ARM9, so a bulk read (find_pattern/dump_memory) must freeze ARM7 too — else a
    // running ARM7 mutates `main` mid-read and tears the snapshot. A running ARM7 must be paused
    // for the read and restored to running after (proven by the resume `c` it receives).
    let arm9 = FakeGdb::with(&[("?", "S05"), ("m2000000,8", "1122334455667788")]);
    let arm7 = FakeGdb::with(&[("?", "S05")]);
    let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
    bridge.arm9.frozen = false; // HITL both-running
    bridge.arm7.as_mut().unwrap().frozen = false;
    let resp = bridge.handle_request(Request::new(
        1,
        "find_pattern",
        json!({"memory_type": "main", "start": 0, "length": 8, "hex": "aa"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let a7 = bridge.arm7.as_ref().unwrap();
    assert!(
        a7.gdb.calls.iter().any(|c| c == "c"),
        "a running ARM7 must be frozen for the shared read and resumed after: {:?}",
        a7.gdb.calls
    );
    assert!(
        !a7.frozen,
        "ARM7 must be restored to running after the read"
    );
    assert!(
        !bridge.arm9.frozen,
        "ARM9 must be restored to running after the read"
    );
}

#[test]
fn shared_read_leaves_already_frozen_arm7_frozen() {
    // If ARM7 is already halted, the bulk read must not spuriously resume it (that would drift a
    // core the agent deliberately paused). Only ARM9 is running here.
    let arm9 = FakeGdb::with(&[("?", "S05"), ("m2000000,8", "1122334455667788")]);
    let arm7 = FakeGdb::with(&[("?", "S05")]); // stays frozen after the handshake
    let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
    bridge.arm9.frozen = false;
    let resp = bridge.handle_request(Request::new(
        2,
        "find_pattern",
        json!({"memory_type": "main", "start": 0, "length": 8, "hex": "aa"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let a7 = bridge.arm7.as_ref().unwrap();
    assert!(a7.frozen, "an already-frozen ARM7 must stay frozen");
    assert!(
        !a7.gdb.calls.iter().any(|c| c == "c"),
        "an already-frozen ARM7 must not be resumed by a shared read: {:?}",
        a7.gdb.calls
    );
}

#[test]
fn step_on_running_core_stays_halted_and_labeled_frozen() {
    // Stepping a running core must end with the core actually halted AND labeled frozen —
    // consistently. The old path let send_cmd's with_frozen auto-resume ("c") after each `s`,
    // re-running the core while step set frozen=true, so the next command hit a running stub and
    // desynced. Assert: no `c` (resume) is emitted and the core is labeled frozen.
    let regs = arm_regs_hex(&[(15, 0x0200_0004)], 0);
    let mut bridge = bridge_arm9_only(&[("?", "S05"), ("s", "S05"), ("g", &regs)]);
    bridge.arm9.frozen = false; // running (e.g. HITL resume-both)
    let response = bridge.handle_request(Request::new(9, "step_instructions", json!({"count": 1})));
    assert!(response.ok, "{:?}", response.error);
    assert!(
        bridge.arm9.frozen,
        "a stepped core must be labeled frozen (matching its real halted state)"
    );
    assert!(
        !bridge.arm9.gdb.calls.iter().any(|c| c == "c"),
        "stepping must not resume (re-run) the core: {:?}",
        bridge.arm9.gdb.calls
    );
    assert_eq!(
        bridge
            .arm9
            .gdb
            .calls
            .iter()
            .filter(|c| c.as_str() == "s")
            .count(),
        1
    );
}

#[test]
fn write_memory_chunks_large_write_into_buffer_sized_packets() {
    // A write larger than the stub's input buffer must be split into MAX_WRITE_CHUNK packets, not
    // sent as one oversized `M` packet that DeSmuME silently drops (lost write + stall). Here a
    // write just over one chunk produces exactly two `M` packets, each within the buffer.
    let size = MAX_WRITE_CHUNK + 0x10;
    let hexstr = "ab".repeat(size);
    let hex1 = "ab".repeat(MAX_WRITE_CHUNK);
    let hex2 = "ab".repeat(0x10);
    let addr2 = 0x0200_0000usize + MAX_WRITE_CHUNK;
    let mut bridge = bridge_arm9_only_pairs(vec![
        ("?".into(), "S05".into()),
        (
            format!("M2000000,{:x}:{hex1}", MAX_WRITE_CHUNK),
            "OK".into(),
        ),
        (format!("M{addr2:x},10:{hex2}"), "OK".into()),
    ]);
    let response = bridge.handle_request(Request::new(
        3,
        "write_memory",
        json!({"memory_type": "main", "address": "0x0", "hex": hexstr}),
    ));
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.result.unwrap()["written"], size);
    let m_calls: Vec<&String> = bridge
        .arm9
        .gdb
        .calls
        .iter()
        .filter(|c| c.starts_with('M'))
        .collect();
    assert_eq!(
        m_calls.len(),
        2,
        "an over-chunk write must be split into 2 M packets, got {}",
        m_calls.len()
    );
    // Every emitted packet (payload + $..#cc framing) must fit the stub's input buffer.
    for c in &m_calls {
        assert!(
            c.len() + 4 <= GDBSTUB_BUFMAX,
            "M packet ({} bytes) must fit the stub input buffer",
            c.len()
        );
    }
}

#[test]
fn shared_main_write_leaves_running_arm7_untouched() {
    // A `main` (shared Main RAM) write freezes ONLY the routed ARM9, never the sibling ARM7.
    // Freezing both cores would guard a running ARM7 against a partially-applied multi-packet
    // write, but the only interrupt available is 0x03 + a `?` query whose retransmits burst SIGINT
    // echoes: pausing ARM7 on every write desyncs later reads into multi-second stalls and can
    // leave ARM7 pinned "frozen" after a resume. A correct running debugger state beats a
    // theoretical tearing guard, so a HITL-resumed ARM7 keeps running: no interrupt, no `c`.
    let size = MAX_WRITE_CHUNK + 0x10;
    let hexstr = "ab".repeat(size);
    let hex1 = "ab".repeat(MAX_WRITE_CHUNK);
    let hex2 = "ab".repeat(0x10);
    let addr2 = 0x0200_0000usize + MAX_WRITE_CHUNK;
    let arm9 = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        (
            format!("M2000000,{:x}:{hex1}", MAX_WRITE_CHUNK),
            "OK".into(),
        ),
        (format!("M{addr2:x},10:{hex2}"), "OK".into()),
    ]);
    let arm7 = FakeGdb::with(&[("?", "S05")]);
    let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
    bridge.arm9.frozen = false; // HITL both-running
    bridge.arm7.as_mut().unwrap().frozen = false;
    let response = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "main", "address": "0x0", "hex": hexstr}),
    ));
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.result.unwrap()["written"], size);

    // ARM7 is never touched by the write: it keeps running and sees nothing past the construction
    // handshake `?` — no interrupt, no resume `c`.
    let a7 = bridge.arm7.as_ref().unwrap();
    assert!(
        !a7.frozen,
        "a running ARM7 must stay running across a shared-Main write"
    );
    assert_eq!(
        a7.gdb.calls,
        vec!["?".to_string()],
        "a shared-Main write must not send ARM7 anything past the handshake: {:?}",
        a7.gdb.calls
    );

    // The write still lands as 2 M packets on the routed ARM9, which is restored to running.
    let m_calls: Vec<&String> = bridge
        .arm9
        .gdb
        .calls
        .iter()
        .filter(|c| c.starts_with('M'))
        .collect();
    assert_eq!(
        m_calls.len(),
        2,
        "the chunked write must reach ARM9 as 2 M packets: {m_calls:?}"
    );
    assert!(
        !bridge.arm9.frozen,
        "ARM9 must be restored to running after the write"
    );
    assert!(
        bridge.arm9.gdb.calls.iter().any(|c| c == "c"),
        "ARM9 frozen for the write must be resumed after: {:?}",
        bridge.arm9.gdb.calls
    );
}

#[test]
fn shared_read_does_not_phantom_freeze_arm7_when_stale_sigint_drains_after_resume() {
    // A shared bulk READ (find_pattern/dump) still runs under with_all_cores_frozen, which pauses
    // then resumes a running ARM7 (`c`). But a SIGINT (S02) — a residual async interrupt echo —
    // then surfaces on ARM7's socket, and the NEXT drain_stops (status/poll) reads it. It must be
    // dropped WITHOUT flipping the genuinely-running, already-resumed ARM7 back to "frozen".
    // note_stop keys `frozen` off reportable stops only (S05), never our SIGINT (S02); the pause/
    // resume bookkeeping owns frozen explicitly. Before that fix, this stale S02 pinned ARM7
    // "frozen" (pc pinned) even though the core was running — the exact live shared-write symptom.
    let arm9 = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        // find_pattern reads the 8-byte window as one m-chunk on ARM9.
        ("m2000000,8".into(), "1122334455667788".into()),
    ]);
    let arm7 = FakeGdb::with(&[("?", "S05")]);
    let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
    bridge.arm9.frozen = false; // HITL both-running (resume cpu="both")
    bridge.arm7.as_mut().unwrap().frozen = false;

    let r = bridge.handle_request(Request::new(
        1,
        "find_pattern",
        json!({"memory_type": "main", "start": 0, "length": 8, "hex": "55"}),
    ));
    assert!(r.ok, "{:?}", r.error);
    // The shared read paused+resumed ARM7 (proven by the `c`), leaving it running.
    let a7 = bridge.arm7.as_ref().unwrap();
    assert!(
        a7.gdb.calls.iter().any(|c| c == "c"),
        "shared read must resume ARM7: {:?}",
        a7.gdb.calls
    );
    assert!(
        !a7.frozen,
        "ARM7 must be running right after the shared read"
    );

    // A stale SIGINT now surfaces on ARM7's socket and is drained by the next status. It must not
    // re-freeze the resumed core (note_stop drops S02 and never sets frozen from it).
    bridge
        .arm7
        .as_mut()
        .unwrap()
        .gdb
        .nonblocking
        .push_back("S02".into());
    let st = bridge
        .handle_request(Request::new(2, "status", json!({})))
        .result
        .unwrap();
    assert_eq!(
        st["cpus"]["arm7"]["state"], "running",
        "a stale SIGINT must not phantom-freeze a resumed ARM7: {st}"
    );
    assert_eq!(st["cpus"]["arm9"]["state"], "running", "{st}");
    assert_eq!(st["state"], "running", "{st}");
}

#[test]
fn nonshared_write_does_not_freeze_running_arm7() {
    // A per-core write (memory_type=arm9) must NOT pause the sibling ARM7 — freezing every core
    // on every write would needlessly halt a HITL-resumed ARM7. Only the routed core is frozen
    // for the write and resumed after; ARM7 is left untouched.
    let arm9 = FakeGdb::with(&[("?", "S05"), ("M2000000,4:deadbeef", "OK")]);
    let arm7 = FakeGdb::with(&[("?", "S05")]);
    let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
    bridge.arm9.frozen = false; // both running
    bridge.arm7.as_mut().unwrap().frozen = false;
    let response = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "arm9", "address": 0x0200_0000u64, "hex": "deadbeef"}),
    ));
    assert!(response.ok, "{:?}", response.error);
    let a7 = bridge.arm7.as_ref().unwrap();
    assert!(!a7.frozen, "ARM7 stays running for a non-shared write");
    assert_eq!(
        a7.gdb.calls,
        vec!["?".to_string()],
        "a non-shared write must not touch ARM7 (no pause/resume): {:?}",
        a7.gdb.calls
    );
}

#[test]
fn write_memory_rejects_length_over_cap() {
    let mut bridge = bridge_arm9_only(&[("?", "S05")]);
    let hexstr = "00".repeat(MAX_WRITE_LEN + 1);
    let r = bridge.handle_request(Request::new(
        1,
        "write_memory",
        json!({"memory_type": "arm9", "address": 0, "hex": hexstr}),
    ));
    assert!(!r.ok);
    assert_eq!(r.error.unwrap().kind, "bad_params");
}

#[test]
fn shared_read_arm7_pause_failure_resumes_arm9() {
    // with_all_cores_frozen pauses ARM9 first, then ARM7. If ARM7's pause errors, the helper must
    // roll back the ARM9 pause it injected — otherwise a failed find_pattern/dump_memory leaves
    // ARM9 wrongly frozen. Assert ARM9 ends running (a resume `c` was sent) and the error surfaces.
    let arm9 = FakeGdb::with(&[("?", "S05")]);
    let mut arm7 = FakeGdb::with(&[("?", "S05")]);
    arm7.fail_interrupt = true; // ARM7's pause (SIGINT) will error
    let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
    bridge.arm9.frozen = false; // both running (HITL)
    bridge.arm7.as_mut().unwrap().frozen = false;
    let resp = bridge.handle_request(Request::new(
        1,
        "find_pattern",
        json!({"memory_type": "main", "start": 0, "length": 8, "hex": "aa"}),
    ));
    assert!(!resp.ok, "an ARM7 pause failure must propagate as an error");
    assert_eq!(resp.error.unwrap().kind, "emulator_error");
    assert!(
        !bridge.arm9.frozen,
        "ARM9 paused by the helper must be resumed after the ARM7 pause fails"
    );
    assert!(
        bridge.arm9.gdb.calls.iter().any(|c| c == "c"),
        "a rollback resume (continue) must be sent to ARM9: {:?}",
        bridge.arm9.gdb.calls
    );
}

#[test]
fn poll_events_preserves_arm9_events_when_arm7_drain_errors() {
    // poll_events drains ARM9 then ARM7. If the ARM7 drain errors, the ARM9 hits already drained
    // must not be discarded — they stay queued and surface on the next poll. Regression for a
    // harvest-into-local between the drains that dropped ARM9 events on an ARM7 socket error.
    let regs = arm_regs_hex(&[(15, 0x0200_0000)], 0);
    let arm9 = FakeGdb::with(&[("?", "S05"), ("g", &regs)]);
    let arm7 = FakeGdb::with(&[("?", "S05")]);
    let mut bridge = NdsBridge::new(arm9, Some(arm7), BridgeEnv::default());
    bridge.arm9.gdb.nonblocking.push_back("S05".into()); // an ARM9 breakpoint hit is pending
    bridge.arm7.as_mut().unwrap().gdb.fail_nonblocking = true; // the ARM7 drain will error

    let first = bridge.handle_request(Request::new(1, "poll_events", json!({})));
    assert!(!first.ok, "an ARM7 drain error must surface");

    // The ARM9 hit was drained before the ARM7 error; it must not be lost.
    bridge.arm7.as_mut().unwrap().gdb.fail_nonblocking = false;
    let second = bridge.handle_request(Request::new(2, "poll_events", json!({})));
    assert!(second.ok, "{:?}", second.error);
    let events = second.result.unwrap();
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["signal"] == "05"),
        "the ARM9 breakpoint hit drained before the ARM7 error was lost: {events:?}"
    );
}

fn bridge_arm9_only_pairs(replies: Vec<(String, String)>) -> NdsBridge<FakeGdb> {
    NdsBridge::new(FakeGdb::from_pairs(replies), None, BridgeEnv::default())
}
