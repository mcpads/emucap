use std::sync::{Arc, Mutex};

use serde_json::json;
use tempfile::TempDir;

use super::*;

struct FakeControl {
    commands: Arc<Mutex<Vec<String>>>,
    frame: u64,
    backend_frame: u64,
    pc: u64,
    paused: bool,
    breaked: bool,
    terminal: bool,
    keymatrix: [u8; 12],
    joystick_owners: [Option<u8>; 2],
    native_breakpoints: Vec<String>,
    frame_probe: Option<[String; 4]>,
    frame_probe_inventory_override: Option<String>,
    next_breakpoint: u64,
    next_watchpoint: u64,
    debugger_drain: String,
    inventory_override: Option<String>,
    fail_once: Option<String>,
    media_target: PathBuf,
    machine: String,
    machine_type: String,
}

impl FakeControl {
    fn new(
        commands: Arc<Mutex<Vec<String>>>,
        fail_once: Option<&str>,
        media_target: PathBuf,
    ) -> Self {
        Self {
            commands,
            frame: 10,
            backend_frame: 10,
            pc: 0x4000,
            paused: true,
            breaked: false,
            terminal: false,
            keymatrix: [0xff; 12],
            joystick_owners: [None; 2],
            native_breakpoints: Vec::new(),
            frame_probe: None,
            frame_probe_inventory_override: None,
            next_breakpoint: 1,
            next_watchpoint: 1,
            debugger_drain: hex::encode("0\n"),
            inventory_override: None,
            fail_once: fail_once.map(str::to_owned),
            media_target,
            machine: "C-BIOS_MSX2+".into(),
            machine_type: "MSX2+".into(),
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
            "machine_info config_name" => self.machine.clone(),
            "machine_info type" => self.machine_type.clone(),
            "binary encode hex [encoding convertto utf-8 [dict get [machine_info media carta] target]]" => {
                hex::encode(self.media_target.to_string_lossy().as_bytes())
            }
            "openmsx_update enable setting"
            | "set throttle off"
            | "set power on"
            | "set renderer SDLGL-PP"
            | "set renderer none"
            | "set mute on"
            | "debug step" => {
                if command == "debug step" {
                    self.pc = self.pc.wrapping_add(1);
                    self.breaked = true;
                    self.paused = false;
                }
                String::new()
            }
            "debug break" => {
                self.breaked = true;
                String::new()
            }
            "debug cont" => {
                self.breaked = false;
                self.paused = false;
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
            "format \"%s|%s\" [set pause] [debug breaked]" => format!(
                "{}|{}",
                if self.paused { "true" } else { "false" },
                if self.breaked { 1 } else { 0 }
            ),
            "debug size memory" => "65536".into(),
            "debug size VRAM" => "131072".into(),
            "debug size {Main RAM}" => "524288".into(),
            "debug size emucap_joystick_override" => "2".into(),
            "machine_info VDP_frame_count" => self.backend_frame.to_string(),
            "::emucap::frame_seq" => self.frame.to_string(),
            "::emucap::cancel_frame" => String::new(),
            "::emucap::frame_debug" => format!(
                "{} {{}} {} {} {{}}",
                self.frame,
                self.paused,
                u8::from(self.breaked)
            ),
            "debug probe list" => "emucap.vdp_frame_boundary".into(),
            "::emucap::frame_probe_inventory" => self
                .frame_probe_inventory_override
                .clone()
                .unwrap_or_else(|| {
                    hex::encode(
                        self.frame_probe
                            .as_ref()
                            .map(|fields| format!("{}\n", fields.join("|")))
                            .unwrap_or_default(),
                    )
                }),
            "debug probe set_bp {emucap.vdp_frame_boundary} {} {::emucap::frame_tick}" => {
                let id = "pp#1".to_string();
                self.frame_probe = Some([
                    id.clone(),
                    "emucap.vdp_frame_boundary".into(),
                    String::new(),
                    "::emucap::frame_tick".into(),
                ]);
                id
            }
            "debug breaked" => {
                if self.breaked {
                    "1".into()
                } else {
                    "0".into()
                }
            }
            "reg PC" => self.pc.to_string(),
            command if command.starts_with("reg ") => "0".into(),
            command if command.starts_with("namespace eval ::emucap ") => String::new(),
            command if command.starts_with("::emucap::set_spec ") => String::new(),
            command if command.starts_with("::emucap::unset_spec ") => String::new(),
            "::emucap::inventory" => self
                .inventory_override
                .clone()
                .unwrap_or_else(|| hex::encode(self.native_breakpoints.join("\n"))),
            "::emucap::drain" => {
                let drained = self.debugger_drain.clone();
                self.debugger_drain = hex::encode("0\n");
                drained
            }
            command if command.starts_with("debug breakpoint create ") => {
                let address = fake_decimal_after(command, "-address ");
                let public_id = fake_decimal_after(command, "::emucap::hit ");
                let native_id = format!("bp#{}", self.next_breakpoint);
                self.next_breakpoint += 1;
                self.native_breakpoints
                    .push(format!("{native_id}|x|{address}|{address}|{public_id}"));
                native_id
            }
            command if command.starts_with("debug watchpoint create ") => {
                let public_id = fake_decimal_after(command, "::emucap::hit ");
                let kind = if command.contains("-type read_mem") {
                    "r"
                } else {
                    "w"
                };
                let address = command
                    .split("-address {")
                    .nth(1)
                    .and_then(|tail| tail.split('}').next())
                    .unwrap();
                let mut parts = address.split_whitespace();
                let start = parts.next().unwrap();
                let end = parts.next().unwrap_or(start);
                let native_id = format!("wp#{}", self.next_watchpoint);
                self.next_watchpoint += 1;
                self.native_breakpoints
                    .push(format!("{native_id}|{kind}|{start}|{end}|{public_id}"));
                native_id
            }
            command
                if command.starts_with("debug breakpoint remove ")
                    || command.starts_with("debug watchpoint remove ") =>
            {
                let native_id = command.split_whitespace().last().unwrap();
                self.native_breakpoints
                    .retain(|entry| !entry.starts_with(&format!("{native_id}|")));
                String::new()
            }
            command if command.starts_with("set emucap_dasm ") => {
                let address = fake_decimal_after(command, "debug disasm ");
                format!("{}|3e,01", hex::encode(format!("LD A,#1 ; {address:04X}")))
            }
            command if command.starts_with("debug read keymatrix ") => {
                let row = command
                    .split_whitespace()
                    .last()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                self.keymatrix[row].to_string()
            }
            command if command.starts_with("debug read emucap_joystick_override ") => {
                let index = command
                    .split_whitespace()
                    .last()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                self.joystick_owners[index]
                    .map(|mask| 0x80 | mask)
                    .unwrap_or(0)
                    .to_string()
            }
            command if command.starts_with("debug read joystickports ") => {
                let index = command
                    .split_whitespace()
                    .last()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                self.joystick_owners[index].unwrap_or(0x3f).to_string()
            }
            command if command.starts_with("debug write emucap_joystick_override ") => {
                let parts = command.split_whitespace().collect::<Vec<_>>();
                let index = parts[3].parse::<usize>().unwrap();
                let encoded = parts[4].parse::<u8>().unwrap();
                self.joystick_owners[index] = (encoded != 0xff).then_some(encoded);
                String::new()
            }
            command if command.contains("restore_machine $emucap_path") => {
                self.joystick_owners = [None; 2];
                self.frame_probe = None;
                String::new()
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
        self.backend_frame = self.backend_frame.checked_add(count).unwrap_or(1);
        self.paused = true;
        self.breaked = false;
        Ok(())
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn child_pid(&self) -> u32 {
        4242
    }
}

fn fake_decimal_after(command: &str, marker: &str) -> u64 {
    command
        .split(marker)
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .unwrap()
        .trim_matches(|ch: char| !ch.is_ascii_digit())
        .parse()
        .unwrap()
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
    let prepared = crate::launch::openmsx::prepare_session(
        crate::launch::openmsx::OpenMsxProfile::CbiosMsx2p,
        &rom,
        temp.path(),
        "bridge-fixture",
        None,
    )
    .unwrap();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let control = FakeControl::new(
        Arc::clone(&commands),
        fail_once,
        prepared.session.media.mounted_path.clone(),
    );
    let bridge = OpenMsxBridge::new(control, &prepared.session, temp.path(), display).unwrap();
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
    assert_eq!(visible_hello["build"], crate::build_identity::BUILD_HASH);
    assert_eq!(visible_hello["host_api"], 3);
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
    assert!(!commands.lock().unwrap()[before..]
        .iter()
        .any(|command| command.starts_with("binary encode hex [debug read_block ")));

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
fn exact_frame_step_uses_adapter_sequence_across_backend_counter_reset() {
    let (mut bridge, _, _) = fixture(false);
    let before = result(bridge.handle_request(Request::new(1, "status", json!({}))))["frame"]
        .as_u64()
        .unwrap();
    bridge.control.backend_frame = u64::MAX;
    let stepped =
        result(bridge.handle_request(Request::new(2, "step", json!({"unit":"frames", "count":2}))));
    bridge.control.backend_frame = 1;
    assert_eq!(stepped["frame_before"], before);
    assert_eq!(stepped["frame"], before + 2);
    assert_eq!(stepped["count"], 2);
}

#[test]
fn frame_monitor_uses_one_exact_native_vsync_probe_and_fails_closed_on_drift() {
    let (mut bridge, commands, _temp) = fixture(false);
    let initial = commands.lock().unwrap().clone();
    assert!(initial.iter().any(|command| command == "debug probe list"));
    assert_eq!(
        initial
            .iter()
            .filter(|command| {
                command.as_str()
                    == "debug probe set_bp {emucap.vdp_frame_boundary} {} {::emucap::frame_tick}"
            })
            .count(),
        1
    );

    bridge.control.frame_probe_inventory_override =
        Some(hex::encode("pp#1|emucap.vdp_frame_boundary||debug break\n"));
    let reset = bridge.handle_request(Request::new(1, "reset", json!({})));
    assert!(!reset.ok);
    let message = reset.error.unwrap().message;
    assert!(
        message.contains("native frame monitor identity changed"),
        "{message}"
    );
    assert!(bridge.backend_terminal());
}

#[test]
fn load_state_rebinds_the_private_frame_monitor_only_after_observing_zero() {
    let (mut bridge, commands, temp) = fixture(false);
    let state_path = temp.path().join("frame-monitor-restore.oms");
    fs::write(&state_path, b"fixture").unwrap();
    let loaded =
        result(bridge.handle_request(Request::new(1, "load_state", json!({"path":state_path}))));
    assert_eq!(loaded["state"], "frozen");
    assert_eq!(
        commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| {
                command.as_str()
                    == "debug probe set_bp {emucap.vdp_frame_boundary} {} {::emucap::frame_tick}"
            })
            .count(),
        2
    );
    assert_eq!(
        bridge.control.frame_probe.as_ref().unwrap()[3],
        "::emucap::frame_tick"
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
fn joystick_ports_have_independent_readback_checked_owners() {
    let (mut bridge, _, _) = fixture(false);
    let first = result(bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"port":1, "buttons":["up", "fire1"]}),
    )));
    assert_eq!(first["device"], "joystick");
    assert_eq!(first["active_low_mask"], 0x2e);

    let second = result(bridge.handle_request(Request::new(
        2,
        "set_input",
        json!({"port":2, "buttons":["b"]}),
    )));
    assert_eq!(second["active_low_mask"], 0x1f);

    let status = result(bridge.handle_request(Request::new(3, "status", json!({}))));
    assert_eq!(status["joystick_ports"][0]["engaged"], true);
    assert_eq!(status["joystick_ports"][0]["guest_value"], 0x2e);
    assert_eq!(status["joystick_ports"][1]["guest_value"], 0x1f);

    let released = result(bridge.handle_request(Request::new(
        4,
        "set_input",
        json!({"port":1, "buttons":[]}),
    )));
    assert_eq!(released["mode"], "native");
    let status = result(bridge.handle_request(Request::new(5, "status", json!({}))));
    assert_eq!(status["joystick_ports"][0]["engaged"], false);
    assert_eq!(status["joystick_ports"][0]["guest_value"], 0x3f);
    assert_eq!(status["joystick_ports"][1]["engaged"], true);
}

#[test]
fn joystick_pulse_restores_persistent_owner_and_running_state() {
    let (mut bridge, commands, _) = fixture(false);
    result(bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"port":1, "buttons":["a"]}),
    )));
    result(bridge.handle_request(Request::new(2, "resume", json!({}))));
    let pulse = result(bridge.handle_request(Request::new(
        3,
        "press_buttons",
        json!({"port":1, "buttons":["right"], "frames":3}),
    )));
    assert_eq!(pulse["state"], "running");
    assert_eq!(pulse["restored_active_low_mask"], 0x2f);
    let status = result(bridge.handle_request(Request::new(4, "status", json!({}))));
    assert_eq!(status["state"], "running");
    assert_eq!(status["joystick_ports"][0]["guest_value"], 0x2f);

    let commands = commands.lock().unwrap();
    assert!(commands
        .iter()
        .any(|command| command == "debug write emucap_joystick_override 0 39"));
    assert!(commands
        .iter()
        .any(|command| command == "debug write emucap_joystick_override 0 47"));
}

#[test]
fn joystick_rejects_invalid_ports_and_opposite_directions_before_mutation() {
    let (mut bridge, commands, _) = fixture(false);
    let invalid = bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"port":3, "buttons":["a"]}),
    ));
    assert!(!invalid.ok);
    assert_eq!(invalid.error.unwrap().kind, "bad_params");

    let before = commands.lock().unwrap().len();
    let opposite = bridge.handle_request(Request::new(
        2,
        "set_input",
        json!({"port":1, "buttons":["up", "down"]}),
    ));
    assert!(!opposite.ok);
    assert_eq!(opposite.error.unwrap().kind, "bad_params");
    assert_eq!(commands.lock().unwrap().len(), before);

    result(bridge.handle_request(Request::new(
        3,
        "set_input",
        json!({"port":1, "buttons":["up"]}),
    )));
    let conflicting_pulse = bridge.handle_request(Request::new(
        4,
        "press_buttons",
        json!({"port":1, "buttons":["down"], "frames":2}),
    ));
    assert!(!conflicting_pulse.ok);
    assert_eq!(conflicting_pulse.error.unwrap().kind, "bad_params");
    let status = result(bridge.handle_request(Request::new(5, "status", json!({}))));
    assert_eq!(status["joystick_ports"][0]["guest_value"], 0x3e);
}

#[test]
fn joystick_write_failure_rolls_back_to_native() {
    let (mut bridge, _, _) =
        fixture_with_failure(false, Some("debug write emucap_joystick_override 0 46"));
    let set = bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"port":1, "buttons":["up", "a"]}),
    ));
    assert!(!set.ok);
    assert_eq!(set.error.unwrap().kind, "emulator_error");
    let status = result(bridge.handle_request(Request::new(2, "status", json!({}))));
    assert_eq!(status["joystick_ports"][0]["engaged"], false);
    assert_eq!(status["joystick_ports"][0]["guest_value"], 0x3f);
}

#[test]
fn load_state_reapplies_nonserialized_joystick_owners() {
    let (mut bridge, commands, temp) = fixture(false);
    result(bridge.handle_request(Request::new(
        1,
        "set_input",
        json!({"port":1, "buttons":["up", "a"]}),
    )));
    result(bridge.handle_request(Request::new(
        2,
        "set_input",
        json!({"port":2, "buttons":["b"]}),
    )));
    let state_path = temp.path().join("restore.oms");
    std::fs::write(&state_path, b"fixture").unwrap();
    let loaded =
        result(bridge.handle_request(Request::new(3, "load_state", json!({"path":state_path}))));
    assert_eq!(loaded["state"], "frozen");

    let status = result(bridge.handle_request(Request::new(4, "status", json!({}))));
    assert_eq!(status["joystick_ports"][0]["guest_value"], 0x2e);
    assert_eq!(status["joystick_ports"][1]["guest_value"], 0x1f);
    let commands = commands.lock().unwrap();
    let restore = commands
        .iter()
        .position(|command| command.contains("restore_machine $emucap_path"))
        .unwrap();
    assert!(commands[restore + 1..]
        .iter()
        .any(|command| command == "debug write emucap_joystick_override 0 46"));
    assert!(commands[restore + 1..]
        .iter()
        .any(|command| command == "debug write emucap_joystick_override 1 31"));
}

#[test]
fn foreign_savestate_machine_identity_terminates_the_generation() {
    let (mut bridge, _, temp) = fixture(false);
    let state_path = temp.path().join("foreign.oms");
    std::fs::write(&state_path, b"fixture").unwrap();
    bridge.control.machine = "Foreign_MSX".into();
    bridge.control.machine_type = "MSX".into();

    let loaded = bridge.handle_request(Request::new(1, "load_state", json!({"path":state_path})));
    assert!(!loaded.ok);
    assert_eq!(loaded.error.unwrap().kind, "emulator_error");
    assert!(bridge.backend_terminal());
}

#[test]
fn foreign_savestate_media_identity_terminates_the_generation() {
    let (mut bridge, _, temp) = fixture(false);
    let state_path = temp.path().join("foreign-media.oms");
    let foreign_rom = temp.path().join("foreign.rom");
    std::fs::write(&state_path, b"fixture").unwrap();
    std::fs::write(&foreign_rom, b"foreign-rom").unwrap();
    bridge.control.media_target = foreign_rom;

    let loaded = bridge.handle_request(Request::new(1, "load_state", json!({"path":state_path})));
    assert!(!loaded.ok);
    assert_eq!(loaded.error.unwrap().kind, "emulator_error");
    assert!(bridge.backend_terminal());
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
    assert!(commands.iter().any(|command| command == "debug cont"));
    assert_eq!(
        commands.last().unwrap(),
        "format \"%s|%s\" [set pause] [debug breaked]"
    );
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
            .filter(|command| command.as_str() == "debug cont")
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
fn debugger_capabilities_are_structured_and_contract_validated() {
    let (mut bridge, _, _) = fixture(false);
    let hello = result(bridge.handle_request(Request::new(1, "hello", json!({}))));
    for method in [
        "set_breakpoint",
        "clear_breakpoint",
        "list_breakpoints",
        "clear_all_breakpoints",
        "poll_events",
        "disassemble",
    ] {
        assert!(hello["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == method));
    }
    assert_eq!(hello["breakpoint_kinds"][0]["kind"], "exec");
    assert_eq!(hello["breakpoint_kinds"][0]["range_mode"], "exact");
    assert_eq!(hello["breakpoint_kinds"][1]["kind"], "read");
    assert_eq!(
        hello["contracts"]["active_exceptions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|value| *value == "openmsx.breakpoint.pausing-subset")
            .count(),
        1
    );
    assert_eq!(
        crate::contracts::validate_advertisement(
            &crate::contracts::advertisement_from_hello(&hello),
            Some("openmsx-rust-xml"),
            Some("msx"),
            &hello["methods"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
        )
        .state,
        "validated"
    );
}

#[test]
fn breakpoint_subset_and_overlap_are_rejected_before_native_mutation() {
    let (mut bridge, commands, _) = fixture(false);
    for params in [
        json!({"kind":"exec", "start":0x4000, "end":0x4001}),
        json!({"kind":"read", "memory_type":"ram", "start":0, "end":1}),
        json!({"kind":"write", "start":0, "pause_on_hit":false}),
        json!({"kind":"exec", "start":0, "condition":"A == 1"}),
        json!({"kind":"read", "start":0, "snapshot":["memory:65535:2"]}),
    ] {
        let response = bridge.handle_request(Request::new(1, "set_breakpoint", params));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "bad_params");
    }
    assert!(!commands.lock().unwrap().iter().any(|command| {
        command.starts_with("debug breakpoint create ")
            || command.starts_with("debug watchpoint create ")
    }));

    result(bridge.handle_request(Request::new(
        2,
        "set_breakpoint",
        json!({"kind":"read", "memory_type":"memory", "start":0x1000, "end":0x10ff}),
    )));
    let native_before = commands.lock().unwrap().len();
    let overlap = bridge.handle_request(Request::new(
        3,
        "set_breakpoint",
        json!({"kind":"read", "memory_type":"memory", "start":0x1080, "end":0x1100}),
    ));
    assert!(!overlap.ok);
    assert_eq!(overlap.error.unwrap().kind, "bad_params");
    assert_eq!(commands.lock().unwrap().len(), native_before);
}

#[test]
fn breakpoint_arm_list_clear_preserves_public_and_native_identity() {
    let (mut bridge, commands, _) = fixture(false);
    let armed = result(bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({
            "kind":"exec",
            "start":0x4000,
            "snapshot":["memory:0x10:2", "vram:0x20:1"]
        }),
    )));
    assert_eq!(armed["id"], 1);
    assert_eq!(armed["native_id"], "bp#1");
    assert_eq!(armed["arm_state"], "armed");

    let listed = result(bridge.handle_request(Request::new(2, "list_breakpoints", json!({}))));
    assert_eq!(listed["breakpoints"][0]["id"], 1);
    assert_eq!(listed["breakpoints"][0]["native_id"], "bp#1");
    assert_eq!(listed["breakpoints"][0]["kind"], "exec");

    let cleared =
        result(bridge.handle_request(Request::new(3, "clear_breakpoint", json!({"id":1}))));
    assert_eq!(cleared["cleared"], 1);
    let listed = result(bridge.handle_request(Request::new(4, "list_breakpoints", json!({}))));
    assert!(listed["breakpoints"].as_array().unwrap().is_empty());

    let commands = commands.lock().unwrap();
    assert!(commands
        .iter()
        .any(|command| command.starts_with("::emucap::set_spec 1 ")));
    assert!(commands
        .iter()
        .any(|command| command == "debug breakpoint remove bp#1"));
    assert!(commands
        .iter()
        .any(|command| command == "::emucap::unset_spec 1"));
}

#[test]
fn breakpoint_event_is_atomic_and_drained_once() {
    let (mut bridge, _, _) = fixture(false);
    result(bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind":"exec", "start":0x4000, "snapshot":["memory:0x10:2"]}),
    )));
    bridge.control.breaked = true;
    bridge.control.paused = true;
    bridge.control.debugger_drain = hex::encode(concat!(
        "0\n",
        "1|1|x|16384|-|-|1,2,3,4,5,6,7,8,9,10,16384,65534,11,12,1,1|",
        "m,16,2,aabb|"
    ));

    let first = result(bridge.handle_request(Request::new(2, "poll_events", json!({}))));
    assert_eq!(first["dropped"], 0);
    assert_eq!(first["events"].as_array().unwrap().len(), 1);
    assert_eq!(first["events"][0]["breakpoint_id"], 1);
    assert_eq!(first["events"][0]["pc"], 0x4000);
    assert_eq!(first["events"][0]["regs"]["SP"], 65534);
    assert_eq!(first["events"][0]["snapshot"][0]["memory_type"], "memory");
    assert_eq!(first["events"][0]["snapshot"][0]["hex"], "aabb");
    assert_eq!(first["events"][0]["evidence_complete"], true);

    let second = result(bridge.handle_request(Request::new(3, "poll_events", json!({}))));
    assert!(second["events"].as_array().unwrap().is_empty());
}

#[test]
fn temporal_request_does_not_claim_a_preexisting_hit() {
    let (mut bridge, commands, _) = fixture(false);
    result(bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind":"exec", "start":0x4000}),
    )));
    bridge.control.breaked = true;
    bridge.control.paused = true;
    bridge.control.debugger_drain = hex::encode(concat!(
        "0\n",
        "1|1|x|16384|-|-|1,2,3,4,5,6,7,8,9,10,16384,65534,11,12,1,1||"
    ));
    let continues_before = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|command| command.as_str() == "debug cont")
        .count();

    let step = bridge.handle_request(Request::new(2, "step", json!({"unit":"frames", "count":1})));
    assert!(!step.ok);
    assert_eq!(step.error.unwrap().kind, "bad_state");
    assert_eq!(
        commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| command.as_str() == "debug cont")
            .count(),
        continues_before
    );
    let events = result(bridge.handle_request(Request::new(3, "poll_events", json!({}))));
    assert_eq!(events["events"][0]["hit_seq"], 1);
}

#[test]
fn load_state_preserves_exact_native_breakpoint_set_or_fails_generation() {
    let (mut bridge, _, temp) = fixture(false);
    result(bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind":"write", "memory_type":"memory", "start":0x2000, "end":0x20ff}),
    )));
    let state_path = temp.path().join("debugger-restore.oms");
    fs::write(&state_path, b"fixture").unwrap();
    let loaded =
        result(bridge.handle_request(Request::new(2, "load_state", json!({"path":state_path}))));
    assert_eq!(loaded["state"], "frozen");
    let listed = result(bridge.handle_request(Request::new(3, "list_breakpoints", json!({}))));
    assert_eq!(listed["breakpoints"][0]["native_id"], "wp#1");

    bridge.control.inventory_override = Some(hex::encode("wp#999|w|8192|8447|1"));
    let failed = bridge.handle_request(Request::new(4, "reset", json!({})));
    assert!(!failed.ok);
    assert_eq!(failed.error.unwrap().kind, "emulator_error");
    assert!(bridge.backend_terminal());
}

#[test]
fn disassemble_uses_instruction_bytes_as_the_length_authority() {
    let (mut bridge, _, _) = fixture(false);
    let value = result(bridge.handle_request(Request::new(
        1,
        "disassemble",
        json!({"address":0x4000, "count":2}),
    )));
    assert_eq!(value["cpu"], "z80");
    assert_eq!(value["instructions"][0]["addr"], 0x4000);
    assert_eq!(value["instructions"][0]["bytes"], "3e01");
    assert_eq!(value["instructions"][1]["addr"], 0x4002);
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
