use std::collections::VecDeque;

use serde_json::json;

use super::*;
use crate::gdb_rsp::{GdbResult, GdbTransport};
use crate::live::protocol::Request;

#[derive(Default)]
struct FakeGdb {
    replies: VecDeque<String>,
    sent: Vec<String>,
}

impl FakeGdb {
    fn with(replies: &[&str]) -> Self {
        Self {
            replies: replies.iter().map(|v| (*v).into()).collect(),
            sent: Vec::new(),
        }
    }
}

impl GdbTransport for FakeGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        self.sent.push(payload.into());
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
fn rejects_aes_until_its_media_contract_is_proven() {
    let result = NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_aes");
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
    assert_eq!(value["contracts"]["catalog"], crate::contracts::CATALOG_ID);
    assert_eq!(
        value["contracts"]["active_exceptions"],
        json!(ACTIVE_EXCEPTIONS)
    );
    let advertisement = crate::contracts::advertisement_from_hello(&value);
    let methods = METHODS
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
fn state_read_requires_freeze() {
    let mut bridge =
        NeoGeoBridge::new(FakeGdb::default(), GdbBridgeEnv::default(), "neogeo_mvs").unwrap();
    let response = bridge.handle_request(request(1, "get_state", json!({})));
    assert_eq!(response.error.unwrap().kind, "bad_state");
    assert!(bridge.gdb.sent.is_empty());
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
