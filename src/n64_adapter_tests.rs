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
    for method in [
        "step_instructions",
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
