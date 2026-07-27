use std::sync::{Arc, Mutex};

use serde_json::json;
use tempfile::TempDir;

use super::*;

struct FakeControl {
    commands: Arc<Mutex<Vec<String>>>,
    frame: u64,
    pc: u64,
    paused: bool,
    terminal: bool,
    keymatrix: [u8; 12],
    fail_once: Option<String>,
}

impl FakeControl {
    fn new(commands: Arc<Mutex<Vec<String>>>, fail_once: Option<&str>) -> Self {
        Self {
            commands,
            frame: 10,
            pc: 0x4000,
            paused: true,
            terminal: false,
            keymatrix: [0xff; 12],
            fail_once: fail_once.map(str::to_owned),
        }
    }
}

impl OpenMsxControl for FakeControl {
    fn command(&mut self, command: &str) -> BridgeResult<String> {
        self.commands.lock().unwrap().push(command.to_string());
        if self.fail_once.as_deref() == Some(command) {
            self.fail_once = None;
            return Err(OpenMsxBridgeError::Emulator(format!(
                "injected failure for {command}"
            )));
        }
        let result = match command {
            "openmsx_info version" => "openMSX 21.0".into(),
            "machine_info config_name" => "C-BIOS_MSX2+".into(),
            "machine_info type" => "MSX2+".into(),
            "openmsx_update enable setting"
            | "set throttle off"
            | "set power on"
            | "set renderer SDLGL-PP"
            | "set renderer none"
            | "set mute on"
            | "debug break"
            | "debug cont"
            | "debug step" => {
                if command == "debug step" {
                    self.pc = self.pc.wrapping_add(1);
                }
                String::new()
            }
            "set pause on" => {
                self.paused = true;
                "true".into()
            }
            "set pause off" => {
                self.paused = false;
                "false".into()
            }
            "set pause" => {
                if self.paused {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            "debug size memory" => "65536".into(),
            "debug size VRAM" => "131072".into(),
            "debug size {Main RAM}" => "524288".into(),
            "machine_info VDP_frame_count" => self.frame.to_string(),
            "debug breaked" => "1".into(),
            "reg PC" => self.pc.to_string(),
            command if command.starts_with("reg ") => "0".into(),
            command if command.starts_with("debug read keymatrix ") => {
                let row = command
                    .split_whitespace()
                    .last()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                self.keymatrix[row].to_string()
            }
            command if command.starts_with("binary encode hex [debug read_block ") => {
                let length = command
                    .trim_end_matches(']')
                    .split_whitespace()
                    .last()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                "00".repeat(length)
            }
            command if command.starts_with("keymatrixup ") => {
                let parts = command.split_whitespace().collect::<Vec<_>>();
                let row = parts[1].parse::<usize>().unwrap();
                let mask = parts[2].parse::<u8>().unwrap();
                self.keymatrix[row] |= mask;
                String::new()
            }
            command if command.starts_with("keymatrixdown ") => {
                let parts = command.split_whitespace().collect::<Vec<_>>();
                let row = parts[1].parse::<usize>().unwrap();
                let mask = parts[2].parse::<u8>().unwrap();
                self.keymatrix[row] &= !mask;
                String::new()
            }
            command
                if command.starts_with("debug write_block ") || command.starts_with("reset;") =>
            {
                String::new()
            }
            other => {
                return Err(OpenMsxBridgeError::Protocol(format!(
                    "unexpected fake command: {other}"
                )))
            }
        };
        Ok(result)
    }

    fn advance_frames(&mut self, count: u64) -> BridgeResult<()> {
        self.frame += count;
        self.paused = true;
        Ok(())
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn child_pid(&self) -> u32 {
        4242
    }
}

fn fixture(display: bool) -> (OpenMsxBridge<FakeControl>, Arc<Mutex<Vec<String>>>, TempDir) {
    fixture_with_failure(display, None)
}

fn fixture_with_failure(
    display: bool,
    fail_once: Option<&str>,
) -> (OpenMsxBridge<FakeControl>, Arc<Mutex<Vec<String>>>, TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let rom = temp.path().join("game.rom");
    fs::write(&rom, b"openmsx-test-rom").unwrap();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let control = FakeControl::new(Arc::clone(&commands), fail_once);
    let bridge = OpenMsxBridge::new(control, &rom, temp.path(), display).unwrap();
    (bridge, commands, temp)
}

fn result(response: Response) -> Value {
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}

#[test]
fn hello_advertises_only_the_display_proven_screenshot_surface() {
    let (mut visible, _, _) = fixture(true);
    let visible_hello = result(visible.handle_request(Request::new(1, "hello", json!({}))));
    assert!(visible_hello["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method == "screenshot"));
    assert_eq!(
        crate::contracts::validate_advertisement(
            &crate::contracts::advertisement_from_hello(&visible_hello),
            Some("openmsx-rust-xml"),
            Some("msx"),
            &visible_hello["methods"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
        )
        .state,
        "validated"
    );

    let (mut headless, _, _) = fixture(false);
    let headless_hello = result(headless.handle_request(Request::new(2, "hello", json!({}))));
    assert!(!headless_hello["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method == "screenshot"));
    assert!(!headless_hello["contracts"]["active_exceptions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "openmsx.screenshot.frozen-only"));
}

#[test]
fn memory_access_checks_region_end_before_contacting_openmsx() {
    let (mut bridge, commands, _) = fixture(false);
    let read = result(bridge.handle_request(Request::new(
        1,
        "read_memory",
        json!({"memory_type":"memory", "address":65535, "length":1}),
    )));
    assert_eq!(read["hex"], "00");

    let before = commands.lock().unwrap().len();
    let rejected = bridge.handle_request(Request::new(
        2,
        "read_memory",
        json!({"memory_type":"memory", "address":65535, "length":2}),
    ));
    assert!(!rejected.ok);
    assert_eq!(rejected.error.unwrap().kind, "bad_params");
    assert_eq!(commands.lock().unwrap().len(), before + 1);
    assert_eq!(commands.lock().unwrap().last().unwrap(), "set pause");

    let oversized = bridge.handle_request(Request::new(
        3,
        "read_memory",
        json!({"memory_type":"ram", "address":0, "length":16385}),
    ));
    assert!(!oversized.ok);
    assert!(oversized.error.unwrap().message.contains("exceeds 16384"));
}

#[test]
fn frame_and_instruction_steps_end_frozen_at_measured_boundaries() {
    let (mut bridge, commands, _) = fixture(false);
    let frame = result(bridge.handle_request(Request::new(
        1,
        "step",
        json!({"unit":"frames", "count":2, "cpu":"z80"}),
    )));
    assert_eq!(frame["frame_before"], 10);
    assert_eq!(frame["frame"], 12);
    assert_eq!(frame["state"], "frozen");

    let instruction = result(bridge.handle_request(Request::new(
        2,
        "step",
        json!({"unit":"instructions", "count":2}),
    )));
    assert_eq!(instruction["pc_before"], 0x4000);
    assert_eq!(instruction["pc"], 0x4002);
    assert_eq!(instruction["state"], "frozen");
    assert_eq!(
        commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| command.as_str() == "debug step")
            .count(),
        2
    );
}

#[test]
fn persistent_keyboard_input_releases_with_an_empty_set() {
    let (mut bridge, commands, _) = fixture(false);
    let held = result(bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"buttons":["space", "up"]}),
    )));
    assert_eq!(held["input_override"], true);
    assert!(commands
        .lock()
        .unwrap()
        .iter()
        .any(|command| command == "keymatrixdown 8 33"));
    let status = result(bridge.handle_request(Request::new(2, "status", json!({}))));
    assert_eq!(status["input_matrix"]["8"], 0xde);

    let released =
        result(bridge.handle_request(Request::new(3, "set_input", json!({"buttons":[]}))));
    assert_eq!(released["mode"], "native");
    assert_eq!(released["input_override"], false);
    let status = result(bridge.handle_request(Request::new(4, "status", json!({}))));
    assert_eq!(status["input_matrix"]["8"], 0xff);
}

#[test]
fn pulse_restores_existing_hold_and_original_running_state() {
    let (mut bridge, commands, _) = fixture(false);
    result(bridge.handle_request(Request::new(1, "set_input", json!({"buttons":["space"]}))));
    result(bridge.handle_request(Request::new(2, "resume", json!({}))));
    let pulse = result(bridge.handle_request(Request::new(
        3,
        "press_buttons",
        json!({"buttons":["up"], "frames":3}),
    )));
    assert_eq!(pulse["state"], "running");
    assert_eq!(pulse["input_override"], true);

    let commands = commands.lock().unwrap();
    assert!(commands
        .iter()
        .any(|command| command == "keymatrixdown 8 33"));
    assert_eq!(commands.last().unwrap(), "set pause off");
}

#[test]
fn pulse_setup_failure_restores_original_running_state() {
    let (mut bridge, commands, _) = fixture_with_failure(false, Some("keymatrixdown 8 1"));
    result(bridge.handle_request(Request::new(1, "resume", json!({}))));

    let pulse = bridge.handle_request(Request::new(
        2,
        "press_buttons",
        json!({"buttons":["space"], "frames":2}),
    ));
    assert!(!pulse.ok);
    assert_eq!(pulse.error.unwrap().kind, "emulator_error");

    let status = result(bridge.handle_request(Request::new(3, "status", json!({}))));
    assert_eq!(status["state"], "running");
    assert_eq!(status["input_override"], false);
    assert_eq!(
        commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| command.as_str() == "set pause off")
            .count(),
        2
    );
}

#[test]
fn resume_does_not_claim_running_when_debug_continue_fails() {
    let (mut bridge, commands, _) = fixture_with_failure(false, Some("debug cont"));
    let resumed = bridge.handle_request(Request::new(1, "resume", json!({})));
    assert!(!resumed.ok);
    assert_eq!(resumed.error.unwrap().kind, "emulator_error");
    assert!(!commands
        .lock()
        .unwrap()
        .iter()
        .any(|command| command == "set pause off"));

    let status = result(bridge.handle_request(Request::new(2, "status", json!({}))));
    assert_eq!(status["state"], "frozen");
}

#[test]
fn state_groups_and_cpu_selectors_fail_loudly() {
    let (mut bridge, _, _) = fixture(false);
    let groups = bridge.handle_request(Request::new(1, "get_state", json!({"groups":["video"]})));
    assert!(!groups.ok);
    assert_eq!(groups.error.unwrap().kind, "bad_params");

    let cpu = bridge.handle_request(Request::new(
        2,
        "step",
        json!({"unit":"instructions", "cpu":"r800"}),
    ));
    assert!(!cpu.ok);
    assert_eq!(cpu.error.unwrap().kind, "bad_params");
}

#[test]
fn headless_screenshot_is_not_callable() {
    let (mut bridge, _, _) = fixture(false);
    let response = bridge.handle_request(Request::new(1, "screenshot", json!({})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "unsupported");
}

#[test]
fn xml_text_round_trip_preserves_command_metacharacters() {
    let command = "set x {a&b<c>d}";
    assert_eq!(xml_unescape(&xml_escape(command)), command);
    assert_eq!(
        tag_text("<reply result=\"ok\">a&amp;b&#x0a;c</reply>", "reply"),
        Some("a&b\nc".into())
    );
}

#[test]
fn standard_keyboard_matrix_positions_are_combined_per_row() {
    assert_eq!(
        row_masks(["space", "up", "right"]),
        BTreeMap::from([(8, 0xa1)])
    );
    assert_eq!(button_position("a"), (2, 0x40));
    assert_eq!(button_position("ctrl"), (6, 0x02));
}
