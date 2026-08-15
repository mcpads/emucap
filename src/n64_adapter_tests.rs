use serde_json::json;

use super::lifecycle::{readiness_for, AdapterReadiness};
use super::*;

fn frame_gate_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[test]
fn readiness_requires_the_first_rendered_frame_only_in_visible_mode() {
    assert_eq!(
        readiness_for(true, false, false, false, false),
        AdapterReadiness::WaitingForDebugger
    );
    assert_eq!(
        readiness_for(true, true, false, false, false),
        AdapterReadiness::WaitingForInitialRelease
    );
    assert_eq!(
        readiness_for(true, true, true, false, false),
        AdapterReadiness::WaitingForFirstRenderedFrame
    );
    assert_eq!(
        readiness_for(true, true, true, false, true),
        AdapterReadiness::Ready
    );
    assert_eq!(
        readiness_for(false, true, true, false, false),
        AdapterReadiness::Ready
    );
    assert_eq!(
        readiness_for(false, true, true, true, true),
        AdapterReadiness::Terminated
    );
}

#[test]
fn rdram_access_is_offset_based_and_fail_loud_at_the_boundary() {
    assert_eq!(
        rdram_address(&json!({"memory_type":"rdram", "address":0}), 1).unwrap(),
        RDRAM_BASE
    );
    assert!(matches!(
        rdram_address(&json!({"memory_type":"rdram", "address":RDRAM_SIZE - 1}), 2),
        Err(N64Error::BadParams(_))
    ));
}

#[test]
fn rdram_access_rejects_unknown_memory_types() {
    assert!(matches!(
        rdram_address(&json!({"memory_type":"rom", "address":0}), 1),
        Err(N64Error::BadParams(_))
    ));
}

#[test]
fn execution_cpu_is_explicitly_limited_to_r4300() {
    require_r4300(&json!({"cpu":"r4300"})).unwrap();
    require_r4300(&json!({})).unwrap();
    assert!(matches!(
        require_r4300(&json!({"cpu":"rsp"})),
        Err(N64Error::BadParams(_))
    ));
}

#[test]
fn numeric_parameters_accept_decimal_and_prefixed_hex() {
    assert_eq!(parse_num(&json!("0x20")), Some(32));
    assert_eq!(parse_num(&json!("$20")), Some(32));
    assert_eq!(parse_num(&json!(32)), Some(32));
}

#[test]
fn r4300_link_classifier_distinguishes_calls_from_plain_jumps() {
    assert!(debug::r4300_link_instruction(0x0c00_0000));
    assert!(debug::r4300_link_instruction(0x03e0_f809));
    assert!(debug::r4300_link_instruction(0x0410_0000));
    assert!(!debug::r4300_link_instruction(0x0800_0000));
    assert!(!debug::r4300_link_instruction(0x03e0_0008));
}

#[test]
fn r4300_rdram_aliases_share_one_bounded_stack_region() {
    assert_eq!(debug::r4300_rdram_offset(0x8000_0020), Some(0x20));
    assert_eq!(debug::r4300_rdram_offset(0xa000_0020), Some(0x20));
    assert_eq!(debug::r4300_rdram_offset(0x8080_0000), None);
    assert_eq!(debug::r4300_rdram_offset(0xa080_0000), None);
}

#[test]
fn r4300_stack_walk_keeps_only_validated_return_addresses() {
    let pc = 0x8000_1000;
    let ra = 0x8000_2008;
    let sp = 0x8001_0000;
    let words = std::collections::BTreeMap::from([
        (0x8000_2000, 0x0c00_0000),
        (0x8000_3000, 0x03e0_f809),
        (sp + 4, 0x8123_4568),
        (sp + 8, 0x8000_3008),
        (sp + 12, ra),
    ]);
    let frames = debug::walk_r4300_stack(pc, ra, sp, |address| {
        words.get(&address).copied().unwrap_or(0)
    });
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].pc, pc);
    assert_eq!(frames[0].kind, "pc");
    assert_eq!(frames[1].pc, ra);
    assert_eq!(frames[1].kind, "ra");
    assert_eq!(frames[2].pc, 0x8000_3008);
    assert_eq!(frames[2].stack_address, Some(sp + 8));
}

#[test]
fn initial_contract_advertisement_validates() {
    let exceptions = exceptions_for(true);
    let hello = json!({
        "contracts": crate::contracts::advertisement_value(&exceptions)
    });
    let advertisement = crate::contracts::advertisement_from_hello(&hello);
    let methods = methods_for(true)
        .iter()
        .map(|method| (*method).to_string())
        .collect::<Vec<_>>();
    let status = crate::contracts::validate_advertisement(
        &advertisement,
        Some("mupen64plus-native"),
        Some("n64"),
        &methods,
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert_eq!(status.authority["debug.call-stack"], json!("best_effort"));
    assert_eq!(
        status.constraints["debug.call-stack.cpus.allowed"],
        json!(["r4300"])
    );
    for method in [
        "step_instructions",
        "call_stack",
        "set_input",
        "press_buttons",
        "screenshot",
        "save_state",
        "load_state",
    ] {
        assert!(methods.iter().any(|advertised| advertised == method));
    }
    assert!(!exceptions.contains(&"n64.execution.frame-step-absent"));
}

#[test]
fn headless_contract_removes_frame_step_and_reports_the_exception() {
    let exceptions = exceptions_for(false);
    let advertisement = crate::contracts::advertisement_from_hello(&json!({
        "contracts": crate::contracts::advertisement_value(&exceptions)
    }));
    let methods = methods_for(false)
        .iter()
        .map(|method| (*method).to_string())
        .collect::<Vec<_>>();
    let status = crate::contracts::validate_advertisement(
        &advertisement,
        Some("mupen64plus-native"),
        Some("n64"),
        &methods,
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert_eq!(
        status.constraints["execution.step.units"],
        json!(["instructions"])
    );
    assert!(methods.iter().any(|method| method == "set_input"));
    assert!(methods.iter().any(|method| method == "call_stack"));
    for unavailable in [
        "step",
        "press_buttons",
        "screenshot",
        "save_state",
        "load_state",
    ] {
        assert!(!methods.iter().any(|method| method == unavailable));
    }
}

#[test]
fn frame_gate_rearms_before_releasing_the_previous_callback() {
    let _guard = frame_gate_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset_frame_gate();
    arm_frame_gate(FrameGateTrigger::NextFrame).unwrap();
    let callbacks = std::thread::spawn(|| {
        frame::frame_callback(0);
        frame::frame_callback(1);
    });

    assert_eq!(wait_frame_gate(Duration::from_secs(1)).unwrap(), 1);
    arm_frame_gate(FrameGateTrigger::NextFrame).unwrap();
    assert_eq!(release_frame_gate().unwrap(), 1);
    assert_eq!(wait_frame_gate(Duration::from_secs(1)).unwrap(), 2);
    assert_eq!(release_frame_gate().unwrap(), 2);

    callbacks.join().unwrap();
    reset_frame_gate();
}

#[test]
fn timed_out_frame_gate_returns_callback_ownership() {
    let _guard = frame_gate_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset_frame_gate();
    arm_frame_gate(FrameGateTrigger::NextFrame).unwrap();
    assert!(matches!(
        wait_frame_gate(Duration::from_millis(1)),
        Err(N64Error::Timeout(_))
    ));
    assert!(!frame_gate_is_blocked());

    let callback = std::thread::spawn(|| frame::frame_callback(0));
    callback.join().unwrap();
    assert!(!frame_gate_is_blocked());
    reset_frame_gate();
}

#[test]
fn screenshot_gate_waits_for_completion_before_freezing_a_frame() {
    let _guard = frame_gate_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset_frame_gate();
    SCREENSHOT_RESULT.store(-1, Ordering::Release);
    arm_frame_gate(FrameGateTrigger::ScreenshotCompleted).unwrap();

    let first = std::thread::spawn(|| frame::frame_callback(0));
    first.join().unwrap();
    assert!(!frame_gate_is_blocked());

    SCREENSHOT_RESULT.store(1, Ordering::Release);
    let completed = std::thread::spawn(|| frame::frame_callback(1));
    assert_eq!(wait_frame_gate(Duration::from_secs(1)).unwrap(), 2);
    assert_eq!(release_frame_gate().unwrap(), 2);

    completed.join().unwrap();
    SCREENSHOT_RESULT.store(-1, Ordering::Release);
    reset_frame_gate();
}

#[test]
fn screenshot_completion_may_cross_internal_frame_callbacks_but_step_may_not() {
    assert!(validate_observed_frame(FrameGateTrigger::NextFrame, true, 10, 11).is_ok());
    assert!(matches!(
        validate_observed_frame(FrameGateTrigger::NextFrame, true, 10, 13),
        Err(N64Error::BadState(_))
    ));
    assert!(validate_observed_frame(FrameGateTrigger::ScreenshotCompleted, true, 10, 13).is_ok());
    assert!(matches!(
        validate_observed_frame(FrameGateTrigger::ScreenshotCompleted, true, 10, 10),
        Err(N64Error::BadState(_))
    ));
}

#[test]
fn input_names_are_canonicalized_and_mapped_to_unique_scancodes() {
    let buttons = control::normalize_buttons(Some(&json!([
        "A",
        "shoulder-l",
        "analog_up",
        "dpad-right",
        "c_left"
    ])))
    .unwrap();
    assert_eq!(
        buttons,
        ["a", "c_left", "dpad_right", "l", "up"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    let scancodes = INPUT_BUTTONS
        .iter()
        .map(|button| control::input_key(button).unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(scancodes.len(), INPUT_BUTTONS.len());
    assert!(control::normalize_buttons(Some(&json!(["menu"]))).is_err());
}

#[test]
fn input_port_and_pulse_bound_are_fail_loud() {
    control::require_port_zero(&json!({})).unwrap();
    control::require_port_zero(&json!({"port":0})).unwrap();
    assert!(matches!(
        control::require_port_zero(&json!({"port":1})),
        Err(N64Error::BadParams(_))
    ));
    assert_eq!(MAX_INPUT_PULSE_FRAMES, 120);
    assert_eq!(
        MAX_STEP_COUNT,
        crate::live::temporal::MAX_SYNC_ADVANCE_COUNT
    );
}

#[test]
fn breakpoint_specs_separate_virtual_exec_from_physical_rdram_access() {
    let exec = debug::breakpoint_spec(&json!({
        "kind":"exec",
        "start":"0x80000100",
        "pause_on_hit":true,
    }))
    .unwrap();
    assert_eq!(exec.native.address, 0x8000_0100);
    assert_eq!(exec.native.endaddr, 0x8000_0100);
    assert_eq!(exec.native.flags, debug::BKP_ENABLED | debug::BKP_EXEC);

    let read = debug::breakpoint_spec(&json!({
        "kind":"read",
        "memory_type":"rdram",
        "start":0x20,
        "end":0x23,
        "pause_on_hit":true,
        "snapshot":["rdram:0x40:4"],
    }))
    .unwrap();
    assert_eq!(read.native.address, 0x20);
    assert_eq!(read.native.endaddr, 0x23);
    assert_eq!(read.native.flags, debug::BKP_ENABLED | debug::BKP_READ);
    assert_eq!(read.snapshots[0].offset, 0x40);
    assert_eq!(read.snapshots[0].length, 4);
}

#[test]
fn breakpoint_specs_reject_ambiguous_or_unobservable_requests() {
    for params in [
        json!({"kind":"exec", "start":0x100, "end":0x104}),
        json!({"kind":"access", "start":0x100}),
        json!({"kind":"read", "memory_type":"rom", "start":0}),
        json!({"kind":"write", "memory_type":"rdram", "start":RDRAM_SIZE}),
        json!({"kind":"exec", "start":0x100, "pause_on_hit":false}),
        json!({"kind":"exec", "start":0x100, "value":1}),
    ] {
        assert!(debug::breakpoint_spec(&params).is_err(), "{params}");
    }
}

#[test]
fn native_slot_identity_moves_only_after_a_lower_slot_is_removed() {
    assert_eq!(debug::slot_after_clear(2, 1), Some(1));
    assert_eq!(debug::slot_after_clear(1, 2), Some(1));
    assert_eq!(debug::slot_after_clear(1, 1), None);
}

#[test]
fn trigger_matching_requires_one_non_overlapping_public_range() {
    let exec = debug::breakpoint_spec(&json!({"kind":"exec", "start":0x80000100_u64})).unwrap();
    let same = debug::breakpoint_spec(&json!({"kind":"exec", "start":0x80000100_u64})).unwrap();
    let other = debug::breakpoint_spec(&json!({"kind":"exec", "start":0x80000104_u64})).unwrap();
    assert!(debug::ranges_overlap(&exec, &same));
    assert!(!debug::ranges_overlap(&exec, &other));
    assert!(debug::trigger_matches(
        &exec,
        debug::BKP_EXEC,
        0,
        0x8000_0100
    ));
    assert!(!debug::trigger_matches(
        &exec,
        debug::BKP_EXEC,
        0,
        0x8000_0104
    ));
}

#[test]
fn disassembly_uses_the_r4300_virtual_address_space_and_checks_its_boundary() {
    assert_eq!(
        debug::disassembly_range(&json!({"address":"0x80000100"}), 4).unwrap(),
        (0x8000_0100, 16)
    );
    assert!(
        debug::disassembly_range(&json!({"address":"0xfffffffc"}), 1).is_ok(),
        "the final aligned instruction is within the 32-bit address space"
    );
    assert!(debug::disassembly_range(&json!({"address":"0xfffffffc"}), 2).is_err());
    assert!(debug::disassembly_range(&json!({"address":0x1_0000_0000_u64}), 1).is_err());
}

#[test]
fn png_header_validation_reports_dimensions() {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend_from_slice(&320u32.to_be_bytes());
    png.extend_from_slice(&240u32.to_be_bytes());
    assert_eq!(control::png_dimensions(&png).unwrap(), (320, 240));
    assert!(control::png_dimensions(b"not png").is_err());
}

#[test]
fn state_partial_path_stays_beside_the_destination() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("slot.m64p");
    let partial = control::state_partial_sibling(&destination).unwrap();
    assert_eq!(partial.parent(), destination.parent());
    assert_ne!(partial, destination);
    assert!(partial
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains(".slot.m64p.partial."));
}

#[test]
fn native_operation_result_classifies_terminal_callback_values() {
    let result = AtomicI32::new(1);
    control::operation_result(&result, "test operation").unwrap();
    result.store(0, Ordering::Release);
    assert!(matches!(
        control::operation_result(&result, "test operation"),
        Err(N64Error::BadState(_))
    ));
    result.store(-1, Ordering::Release);
    assert!(matches!(
        control::operation_result(&result, "test operation"),
        Err(N64Error::BadState(_))
    ));
}

#[test]
fn native_operation_completion_can_arrive_from_the_host_worker() {
    static RESULT: AtomicI32 = AtomicI32::new(-1);
    RESULT.store(-1, Ordering::Release);
    let worker = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(2));
        RESULT.store(1, Ordering::Release);
    });
    control::wait_operation_result(&RESULT, "test host-worker completion").unwrap();
    worker.join().unwrap();
}
