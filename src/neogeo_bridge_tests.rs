use std::collections::VecDeque;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::gdb_rsp::{GdbResult, GdbTransport};
use crate::live::protocol::Request;

#[derive(Default)]
struct FakeGdb {
    replies: VecDeque<String>,
    sent: Vec<String>,
    timeout: Duration,
    timeout_changes: Vec<Duration>,
    write_state_fixture: bool,
}

impl FakeGdb {
    fn with(replies: &[&str]) -> Self {
        Self {
            replies: replies.iter().map(|v| (*v).into()).collect(),
            sent: Vec::new(),
            timeout: Duration::from_secs(5),
            timeout_changes: Vec::new(),
            write_state_fixture: false,
        }
    }

    fn with_state_save(replies: &[&str]) -> Self {
        let mut gdb = Self::with(replies);
        gdb.write_state_fixture = true;
        gdb
    }
}

impl GdbTransport for FakeGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        self.sent.push(payload.into());
        if self.write_state_fixture {
            if let Some(encoded) = payload.strip_prefix("qEmucap,savesync,") {
                if let Ok(bytes) = hex::decode(encoded) {
                    if let Ok(path) = String::from_utf8(bytes) {
                        std::fs::write(path, b"MAMESAVE-fixture").unwrap();
                    }
                }
            }
        }
        Ok(self.replies.pop_front().unwrap_or_default())
    }

    fn send_no_reply(&mut self, payload: &str) -> GdbResult<()> {
        self.sent.push(payload.into());
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        self.sent.push("interrupt".into());
        Ok(self.replies.pop_front().unwrap_or_else(|| "S05".into()))
    }

    fn get_timeout(&self) -> GdbResult<Duration> {
        Ok(self.timeout)
    }

    fn set_timeout(&mut self, timeout: Duration) -> GdbResult<()> {
        self.timeout = timeout;
        self.timeout_changes.push(timeout);
        Ok(())
    }
}

fn request(id: u64, method: &str, params: Value) -> Request {
    Request::new(id, method, params)
}

#[test]
fn rejects_ambiguous_neogeo_system() {
    let result = NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo");
    assert!(matches!(result, Err(BridgeError::BadParams(_))));
}

#[test]
fn hello_advertises_only_proven_initial_surface() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    let response = bridge.handle_request(request(1, "hello", json!({})));
    let value = response.result.unwrap();
    assert_eq!(value["system"], "neogeo_mvs");
    assert_eq!(value["memory_types"], json!(["ram"]));
    assert_eq!(value["breakpoint_kinds"], json!([]));
    assert_eq!(
        value["execution_limits"]["frame"]["max_count"],
        max_sync_frame_count()
    );
    assert_eq!(value["contracts"]["catalog"], crate::contracts::CATALOG_ID);
    assert_eq!(
        value["contracts"]["active_exceptions"],
        json!(MVS_ACTIVE_EXCEPTIONS)
    );
    let advertisement = crate::contracts::advertisement_from_hello(&value);
    let methods = MVS_METHODS
        .iter()
        .map(|method| (*method).to_string())
        .collect::<Vec<_>>();
    let status = crate::contracts::validate_advertisement(
        &advertisement,
        Some("mame-neogeo-rust-gdb"),
        Some("neogeo_mvs"),
        &methods,
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
}

#[test]
fn aes_hello_advertises_state_files_and_console_inputs() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_aes").unwrap();
    let response = bridge.handle_request(request(1, "hello", json!({})));
    let value = response.result.unwrap();
    assert_eq!(value["system"], "neogeo_aes");
    assert_eq!(value["region_sizes"]["ram"], MVS_RAM_SIZE);
    assert_eq!(value["capability_notes"]["initial_scope"], "aes");
    assert_eq!(
        value["capability_notes"]["state_restore"]["supported"],
        true
    );
    assert!(
        value["capability_notes"]["state_restore"]["screenshot_after_load"]
            .as_str()
            .is_some_and(|note| note.contains("one frozen frame"))
    );
    let methods = value["methods"].as_array().unwrap();
    assert!(methods.iter().any(|method| method == "save_state"));
    assert!(methods.iter().any(|method| method == "load_state"));
    assert!(value["input_buttons"]["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|button| button == "select"));
    assert!(!value["input_buttons"]["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|button| button == "coin"));
    let advertisement = crate::contracts::advertisement_from_hello(&value);
    let methods = MVS_METHODS
        .iter()
        .map(|method| (*method).to_string())
        .collect::<Vec<_>>();
    let status = crate::contracts::validate_advertisement(
        &advertisement,
        Some("mame-neogeo-rust-gdb"),
        Some("neogeo_aes"),
        &methods,
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
}

#[test]
fn cd_hello_advertises_the_cd_profile_without_state_files() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_cd").unwrap();
    let response = bridge.handle_request(request(1, "hello", json!({})));
    let value = response.result.unwrap();
    assert_eq!(value["system"], "neogeo_cd");
    assert_eq!(value["region_sizes"]["ram"], CD_RAM_SIZE);
    assert_eq!(value["capability_notes"]["initial_scope"], "cdz");
    assert_eq!(
        value["capability_notes"]["state_restore"]["supported"],
        false
    );
    let methods = value["methods"].as_array().unwrap();
    assert!(!methods.iter().any(|method| method == "save_state"));
    assert!(!methods.iter().any(|method| method == "load_state"));
    assert!(value["input_buttons"]["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|button| button == "select"));
    assert!(!value["input_buttons"]["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|button| button == "coin"));
    let advertisement = crate::contracts::advertisement_from_hello(&value);
    let methods = CD_METHODS
        .iter()
        .map(|method| (*method).to_string())
        .collect::<Vec<_>>();
    let status = crate::contracts::validate_advertisement(
        &advertisement,
        Some("mame-neogeo-rust-gdb"),
        Some("neogeo_cd"),
        &methods,
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
}

#[test]
fn memory_access_requires_freeze_and_checks_cross_boundary() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    let running = bridge.handle_request(request(
        1,
        "read_memory",
        json!({"memory_type":"ram", "address":0, "length":1}),
    ));
    assert_eq!(running.error.unwrap().kind, "bad_state");

    let write_running = bridge.handle_request(request(
        2,
        "write_memory",
        json!({"memory_type":"ram", "address":0, "hex":"00"}),
    ));
    assert_eq!(write_running.error.unwrap().kind, "bad_state");

    bridge.frozen = true;
    let boundary = bridge.handle_request(request(
        3,
        "read_memory",
        json!({"memory_type":"ram", "address":0xffff, "length":2}),
    ));
    assert_eq!(boundary.error.unwrap().kind, "bad_params");

    let oversized = bridge.handle_request(request(
        4,
        "read_memory",
        json!({"memory_type":"ram", "address":0, "length":0x4001}),
    ));
    assert_eq!(oversized.error.unwrap().kind, "bad_params");
    assert!(bridge.gdb.sent.is_empty());
}

#[test]
fn aes_uses_mvs_ram_with_console_only_input() {
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with(&["aa"]),
        GdbBridgeEnv::default(),
        "neogeo_aes",
    )
    .unwrap();
    bridge.frozen = true;
    let last_byte = bridge.handle_request(request(
        1,
        "read_memory",
        json!({"memory_type":"ram", "address":0xffff, "length":1}),
    ));
    assert!(last_byte.ok, "{:?}", last_byte.error);
    assert_eq!(bridge.gdb.sent, vec!["m10ffff,1"]);

    let crossing = bridge.handle_request(request(
        2,
        "read_memory",
        json!({"memory_type":"ram", "address":0xffff, "length":2}),
    ));
    assert_eq!(crossing.error.unwrap().kind, "bad_params");
    let coin = bridge.handle_request(request(3, "set_input", json!({"buttons":["coin"]})));
    assert_eq!(coin.error.unwrap().kind, "bad_params");
    assert_eq!(bridge.gdb.sent, vec!["m10ffff,1"]);
}

#[test]
fn cd_ram_uses_the_two_megabyte_zero_based_profile() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::with(&["aa"]), GdbBridgeEnv::default(), "neogeo_cd").unwrap();
    bridge.frozen = true;
    let last_byte = bridge.handle_request(request(
        1,
        "read_memory",
        json!({"memory_type":"ram", "address":0x1f_ffff, "length":1}),
    ));
    assert!(last_byte.ok, "{:?}", last_byte.error);
    assert_eq!(bridge.gdb.sent, vec!["m1fffff,1"]);

    let crossing = bridge.handle_request(request(
        2,
        "read_memory",
        json!({"memory_type":"ram", "address":0x1f_ffff, "length":2}),
    ));
    assert_eq!(crossing.error.unwrap().kind, "bad_params");
    assert_eq!(bridge.gdb.sent, vec!["m1fffff,1"]);
}

#[test]
fn cd_rejects_state_files_and_mvs_only_input_before_backend_mutation() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_cd").unwrap();
    let save = bridge.handle_request(request(
        1,
        "save_state",
        json!({"path":"/tmp/not-supported.sta"}),
    ));
    assert_eq!(save.error.unwrap().kind, "unknown_method");
    let coin = bridge.handle_request(request(2, "set_input", json!({"buttons":["coin"]})));
    assert_eq!(coin.error.unwrap().kind, "bad_params");
    assert!(bridge.gdb.sent.is_empty());

    bridge.gdb.replies.push_back("OK".into());
    let select = bridge.handle_request(request(3, "set_input", json!({"buttons":["select"]})));
    assert!(select.ok, "{:?}", select.error);
    assert_eq!(bridge.gdb.sent, vec!["qEmucap,setinput,73656c656374"]);
}

#[test]
fn cd_rom_info_hashes_the_complete_cue_graph() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    let track = dir.path().join("track01.bin");
    std::fs::write(&track, b"track-one").unwrap();
    std::fs::write(&cue, "FILE \"track01.bin\" BINARY\n").unwrap();
    let env = GdbBridgeEnv {
        content: Some(cue.clone()),
        ..GdbBridgeEnv::default()
    };
    let mut bridge = NeoGeoBridge::new(FakeGdb::default(), env, "neogeo_cd").unwrap();
    let before = bridge
        .handle_request(request(1, "get_rom_info", json!({})))
        .result
        .unwrap();
    assert_eq!(before["identity"]["kind"], "cue_graph_v1");
    assert_eq!(before["identity"]["files"].as_array().unwrap().len(), 2);

    std::fs::write(&track, b"track-two").unwrap();
    let after = bridge
        .handle_request(request(2, "get_rom_info", json!({})))
        .result
        .unwrap();
    assert_ne!(before["sha1"], after["sha1"]);
}

#[test]
fn state_read_requires_freeze() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    let response = bridge.handle_request(request(1, "get_state", json!({})));
    assert_eq!(response.error.unwrap().kind, "bad_state");
    assert!(bridge.gdb.sent.is_empty());
}

#[test]
fn native_state_methods_require_freeze_before_backend_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sta");
    std::fs::write(&path, b"MAMESAVE-fixture").unwrap();
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    let save = bridge.handle_request(request(
        1,
        "save_state",
        json!({"path":path.display().to_string()}),
    ));
    assert_eq!(save.error.unwrap().kind, "bad_state");
    let load = bridge.handle_request(request(
        2,
        "load_state",
        json!({"path":path.display().to_string()}),
    ));
    assert_eq!(load.error.unwrap().kind, "bad_state");
    assert!(bridge.gdb.sent.is_empty());
}

#[test]
fn native_save_waits_for_backend_completion_and_publishes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state with spaces.sta");
    std::fs::write(&path, b"prior-state").unwrap();
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with_state_save(&["OK", "42"]),
        GdbBridgeEnv::default(),
        "neogeo_mvs",
    )
    .unwrap();
    bridge.frozen = true;
    let response = bridge.handle_request(request(
        1,
        "save_state",
        json!({"path":path.display().to_string()}),
    ));
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.result.unwrap()["state"], "frozen");
    assert_eq!(std::fs::read(&path).unwrap(), b"MAMESAVE-fixture");
    assert_eq!(
        bridge.gdb.timeout_changes,
        vec![STATE_OPERATION_TIMEOUT, Duration::from_secs(5)]
    );
    assert!(bridge.gdb.sent[0].starts_with("qEmucap,savesync,"));
}

#[test]
fn native_load_waits_for_post_load_completion_and_stays_frozen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sta");
    std::fs::write(&path, b"MAMESAVE-fixture").unwrap();
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with(&["OK", "7"]),
        GdbBridgeEnv::default(),
        "neogeo_mvs",
    )
    .unwrap();
    bridge.frozen = true;
    let response = bridge.handle_request(request(
        1,
        "load_state",
        json!({"path":path.display().to_string()}),
    ));
    assert!(response.ok, "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["state"], "frozen");
    assert!(bridge.frozen);
    assert_eq!(
        bridge.gdb.timeout_changes,
        vec![STATE_OPERATION_TIMEOUT, Duration::from_secs(5)]
    );
    assert!(bridge.gdb.sent[0].starts_with("qEmucap,loadsync,"));
}

#[test]
fn parses_m68000_register_packet_as_plugin_little_endian_words() {
    let mut bytes = Vec::new();
    for value in 0..REG_NAMES.len() as u32 {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with(&[&hex::encode(bytes), "42"]),
        GdbBridgeEnv::default(),
        "neogeo_mvs",
    )
    .unwrap();
    bridge.frozen = true;
    let response = bridge.handle_request(request(1, "get_state", json!({})));
    let value = response.result.unwrap();
    assert_eq!(value["M68K"]["d0"], 0);
    assert_eq!(value["M68K"]["pc"], 17);
    assert_eq!(value["frame"], 42);
}

#[test]
fn input_rejects_unknown_button_before_backend_mutation() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    let response = bridge.handle_request(request(1, "set_input", json!({"buttons":["menu"]})));
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert!(bridge.gdb.sent.is_empty());
}

#[test]
fn input_constraints_reject_before_backend_mutation() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    let port = bridge.handle_request(request(1, "set_input", json!({"port":1, "buttons":["a"]})));
    assert_eq!(port.error.unwrap().kind, "bad_params");

    let pulse = bridge.handle_request(request(
        2,
        "press_buttons",
        json!({"buttons":["a"], "frames":121}),
    ));
    assert_eq!(pulse.error.unwrap().kind, "bad_params");
    assert!(bridge.gdb.sent.is_empty());
}

#[test]
fn empty_input_explicitly_returns_native_control() {
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with(&["OK"]),
        GdbBridgeEnv::default(),
        "neogeo_mvs",
    )
    .unwrap();
    let response = bridge.handle_request(request(1, "set_input", json!({"buttons":[]})));
    assert!(response.ok);
    assert_eq!(response.result.unwrap()["mode"], "native");
    assert_eq!(bridge.gdb.sent, vec!["qEmucap,setinput,"]);
}

#[test]
fn secondary_cpu_requests_are_rejected_before_backend_mutation() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    let response = bridge.handle_request(request(
        1,
        "step_instructions",
        json!({"cpu":"z80", "count":1}),
    ));
    assert_eq!(response.error.unwrap().kind, "bad_params");

    let pause = bridge.handle_request(request(2, "pause", json!({"cpu":"z80"})));
    assert_eq!(pause.error.unwrap().kind, "bad_params");

    let resume = bridge.handle_request(request(3, "resume", json!({"cpu":"z80"})));
    assert_eq!(resume.error.unwrap().kind, "bad_params");
    assert!(bridge.gdb.sent.is_empty());
}

#[test]
fn one_instruction_step_stays_frozen() {
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with(&["S05"]),
        GdbBridgeEnv::default(),
        "neogeo_mvs",
    )
    .unwrap();
    bridge.frozen = true;
    let response = bridge.handle_request(request(
        1,
        "step",
        json!({"unit":"instructions", "count":1}),
    ));
    assert!(response.ok);
    assert_eq!(bridge.gdb.sent, vec!["s"]);
    assert!(bridge.frozen);
}

#[test]
fn long_frame_advance_gets_a_bounded_timeout_and_restores_it() {
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with(&["10", "OK", "410"]),
        GdbBridgeEnv::default(),
        "neogeo_mvs",
    )
    .unwrap();
    let response = bridge.handle_request(request(1, "run_frames", json!({"frames":400})));
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.result.unwrap()["frame"], 410);
    assert_eq!(
        bridge.gdb.timeout_changes,
        vec![Duration::from_secs(205), Duration::from_secs(5)]
    );
    assert_eq!(bridge.gdb.timeout, Duration::from_secs(5));
}

#[test]
fn run_frames_accepts_a_terminal_overshoot_and_reports_it() {
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with(&["10", "OK", "72"]),
        GdbBridgeEnv::default(),
        "neogeo_mvs",
    )
    .unwrap();
    let response = bridge.handle_request(request(1, "run_frames", json!({"frames":60})));
    assert!(response.ok, "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["count"], 60);
    assert_eq!(result["frames_observed_min"], 60);
    assert_eq!(result["frame_counter_delta"], 62);
    assert_eq!(result["frame_counter_continuous"], true);
    assert_eq!(result["state"], "running");
}

#[test]
fn frame_advance_accepts_a_screen_counter_reset_after_terminal_ack() {
    let mut bridge = NeoGeoBridge::new(
        FakeGdb::with(&["100", "OK", "4"]),
        GdbBridgeEnv::default(),
        "neogeo_mvs",
    )
    .unwrap();
    let response = bridge.handle_request(request(1, "run_frames", json!({"frames":60})));
    assert!(response.ok, "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["frames_observed_min"], 60);
    assert_eq!(result["frame_counter_delta"], Value::Null);
    assert_eq!(result["frame_counter_continuous"], false);
}

#[test]
fn oversized_frame_advance_is_rejected_before_backend_mutation() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    bridge.frozen = true;
    let response = bridge.handle_request(request(
        1,
        "step",
        json!({"unit":"frames", "count":max_sync_frame_count() + 1}),
    ));
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert!(bridge.gdb.sent.is_empty());
    assert!(bridge.gdb.timeout_changes.is_empty());
}
