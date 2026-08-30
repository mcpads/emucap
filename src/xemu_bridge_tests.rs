use super::*;
use std::cell::Cell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;

use crate::gdb_rsp::GdbResult;
use crate::qmp::QmpResult;

struct FakeQmp {
    replies: VecDeque<(String, QmpResult<Value>)>,
    calls: Vec<(String, Option<Value>)>,
    terminal: bool,
    require_stop_before_call: Option<(usize, Rc<Cell<bool>>)>,
}

impl FakeQmp {
    fn new(replies: Vec<(&str, Value)>) -> Self {
        Self {
            replies: replies
                .into_iter()
                .map(|(command, value)| (command.into(), Ok(value)))
                .collect(),
            calls: Vec::new(),
            terminal: false,
            require_stop_before_call: None,
        }
    }

    fn require_stop_before_call(mut self, index: usize, observed: Rc<Cell<bool>>) -> Self {
        self.require_stop_before_call = Some((index, observed));
        self
    }

    fn assert_drained(&self) {
        assert!(
            self.replies.is_empty(),
            "unconsumed QMP replies: {:?}",
            self.replies
                .iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        );
    }
}

impl QmpTransport for FakeQmp {
    fn execute(&mut self, command: &str, arguments: Option<Value>) -> QmpResult<Value> {
        if let Some((index, observed)) = &self.require_stop_before_call {
            assert!(
                self.calls.len() < *index || observed.get(),
                "QMP was queried before the terminal GDB stop was consumed"
            );
        }
        self.calls.push((command.into(), arguments));
        let (expected, response) = self
            .replies
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected QMP command: {command}"));
        assert_eq!(command, expected);
        response
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }
}

struct FakeGdb {
    replies: VecDeque<(String, GdbResult<String>)>,
    receive_replies: VecDeque<GdbResult<String>>,
    asynchronous: VecDeque<GdbResult<Option<String>>>,
    calls: Vec<String>,
    terminal: bool,
    stop_observed: Option<Rc<Cell<bool>>>,
}

impl FakeGdb {
    fn empty() -> Self {
        Self {
            replies: VecDeque::new(),
            receive_replies: VecDeque::new(),
            asynchronous: VecDeque::new(),
            calls: Vec::new(),
            terminal: false,
            stop_observed: None,
        }
    }

    fn with_replies(replies: Vec<(&str, &str)>) -> Self {
        let mut gdb = Self::empty();
        gdb.replies = replies
            .into_iter()
            .map(|(command, response)| (command.into(), Ok(response.into())))
            .collect();
        gdb
    }

    fn mark_stop_observed(mut self, observed: Rc<Cell<bool>>) -> Self {
        self.stop_observed = Some(observed);
        self
    }
}

impl GdbTransport for FakeGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        self.calls.push(payload.into());
        let (expected, response) = self
            .replies
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected GDB command: {payload}"));
        assert_eq!(payload, expected);
        response
    }

    fn send_no_reply(&mut self, payload: &str) -> GdbResult<()> {
        self.calls.push(payload.into());
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S02".into())
    }

    fn recv_reply(&mut self) -> GdbResult<String> {
        let reply = self
            .receive_replies
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected GDB recv_reply"));
        if reply.as_ref().is_ok_and(|packet| is_stop_packet(packet)) {
            if let Some(observed) = &self.stop_observed {
                observed.set(true);
            }
        }
        reply
    }

    fn recv_nonblocking(&mut self) -> GdbResult<Option<String>> {
        self.asynchronous.pop_front().unwrap_or(Ok(None))
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }
}

fn extension(frame: u64, active: bool, remaining: u64, input: bool) -> Value {
    json!({
        "api":REQUIRED_HOST_API,
        "frame-boundary":frame,
        "frame-step-active":active,
        "frame-step-remaining":remaining,
        "input-engaged":input,
    })
}

fn machine(running: bool) -> Value {
    json!({"running":running, "status":if running {"running"} else {"paused"}})
}

fn state_environment(root: &Path) -> XemuStateEnvironment {
    let hdd = root.join("hdd.qcow2");
    let eeprom = root.join("eeprom.bin");
    if !hdd.exists() {
        std::fs::write(&hdd, b"QFI\xfbstate-test").unwrap();
    }
    if !eeprom.exists() {
        std::fs::write(&eeprom, [0_u8; 256]).unwrap();
    }
    XemuStateEnvironment {
        hdd,
        eeprom,
        host_build: XemuHostBuildIdentity {
            upstream: "https://example.invalid/xemu".into(),
            tag: "test".into(),
            commit: "1".repeat(40),
            host_api: REQUIRED_HOST_API as u32,
            patchset_sha256: "2".repeat(64),
            binary_sha256: "3".repeat(64),
        },
    }
}

fn complete_machine_identity() -> XemuMachineIdentity {
    XemuMachineIdentity {
        mcpx_sha256: Some("4".repeat(64)),
        flash_sha256: Some("5".repeat(64)),
        hdd_template_sha256: Some("6".repeat(64)),
        eeprom_initial_sha256: Some("7".repeat(64)),
    }
}

fn state_block_layout(root: &Path) -> Value {
    json!([
        {
            "device":"ide0-hd0", "removable":false,
            "inserted":{
                "ro":false, "drv":"qcow2",
                "file":root.join("hdd.qcow2").display().to_string(),
                "node-name":"#block-state-hdd"
            }
        },
        {
            "device":"ide0-cd1", "removable":true,
            "inserted":{
                "ro":true, "drv":"raw",
                "file":root.join("game.xiso").display().to_string(),
                "node-name":"#block-state-disc"
            }
        }
    ])
}

fn concluded_job(id: &str, operation: &str) -> Value {
    json!([{
        "id":id, "type":format!("snapshot-{operation}"), "status":"concluded",
        "current-progress":1, "total-progress":1
    }])
}

fn failed_job(id: &str, operation: &str, error: &str) -> Value {
    json!([{
        "id":id, "type":format!("snapshot-{operation}"), "status":"concluded",
        "current-progress":0, "total-progress":1, "error":error
    }])
}

fn state_bridge(qmp: FakeQmp, gdb: FakeGdb, root: &Path) -> XemuBridge<FakeQmp, FakeGdb> {
    let environment = state_environment(root);
    let disc = root.join("game.xiso");
    if !disc.exists() {
        std::fs::write(&disc, b"state-test-xiso").unwrap();
    }
    XemuBridge::new(
        qmp,
        gdb,
        GdbBridgeEnv {
            name: Some("xbox-state-test".into()),
            session_token: Some("token".into()),
            launch_id: Some("launch-state-test".into()),
            content: Some(disc),
            build: Some("state-test-build".into()),
        },
        root.into(),
        true,
        complete_machine_identity(),
        environment,
    )
}

fn save_state_container(root: &Path, state_path: &Path) -> Value {
    let mut bridge = state_bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("query-block", state_block_layout(root)),
            ("snapshot-save", json!({})),
            ("query-jobs", concluded_job("emucap-save-1", "save")),
            ("job-dismiss", json!({})),
            ("query-status", machine(false)),
            ("xemu-emucap-status", extension(42, false, 0, false)),
        ]),
        FakeGdb::empty(),
        root,
    );
    let saved =
        result(bridge.handle_request(Request::new(100, "save_state", json!({"path":state_path}))));
    bridge.qmp.assert_drained();
    saved
}

fn load_state_replies(root: &Path, frame: u64) -> Vec<(&'static str, Value)> {
    vec![
        ("query-status", machine(false)),
        ("query-block", state_block_layout(root)),
        ("snapshot-save", json!({})),
        ("query-jobs", concluded_job("emucap-save-1", "save")),
        ("job-dismiss", json!({})),
        ("snapshot-load", json!({})),
        ("query-jobs", concluded_job("emucap-load-2", "load")),
        ("job-dismiss", json!({})),
        ("query-status", machine(false)),
        ("xemu-emucap-set-input", json!({})),
        ("xemu-emucap-status", extension(frame, false, 0, true)),
        ("xemu-emucap-set-input", json!({})),
        ("xemu-emucap-status", extension(frame, false, 0, false)),
        ("query-block", state_block_layout(root)),
        ("xemu-emucap-status", extension(frame, false, 0, false)),
        ("snapshot-delete", json!({})),
        ("query-jobs", concluded_job("emucap-delete-3", "delete")),
        ("job-dismiss", json!({})),
    ]
}

fn bridge(qmp: FakeQmp, gdb: FakeGdb, root: &Path) -> XemuBridge<FakeQmp, FakeGdb> {
    XemuBridge::new(
        qmp,
        gdb,
        GdbBridgeEnv {
            name: Some("xbox-test".into()),
            session_token: Some("token".into()),
            launch_id: Some("launch-test".into()),
            content: None,
            build: Some("build".into()),
        },
        root.into(),
        false,
        XemuMachineIdentity::default(),
        state_environment(root),
    )
}

fn result(response: Response) -> Value {
    assert!(response.ok, "request failed: {:?}", response.error);
    response.result.unwrap()
}

fn frame_step_gdb(stop: &str) -> FakeGdb {
    let registers = hex::encode([0_u8; 64]);
    let mut gdb = FakeGdb::with_replies(vec![("g", &registers)]);
    gdb.receive_replies.push_back(Ok(stop.into()));
    gdb
}

fn i386_register_packet(eip: u32) -> String {
    let mut registers = [0_u8; 64];
    registers[8 * 4..9 * 4].copy_from_slice(&eip.to_le_bytes());
    hex::encode(registers)
}

#[test]
fn hello_advertises_implemented_state_and_debug_methods() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("xemu-emucap-status", extension(0, false, 0, false)),
            ("query-status", machine(false)),
            ("query-status", machine(true)),
        ]),
        FakeGdb::with_replies(vec![("g", &hex::encode([0_u8; 64]))]),
        temp.path(),
    );
    let hello = result(bridge.handle_request(Request::new(1, "hello", json!({}))));
    let methods = hello["methods"].as_array().unwrap();
    assert!(methods.iter().any(|method| method == "step"));
    assert!(methods.iter().any(|method| method == "call_stack"));
    assert!(methods.iter().any(|method| method == "save_state"));
    assert!(methods.iter().any(|method| method == "load_state"));
    assert!(methods.iter().any(|method| method == "probe"));
    assert!(hello["capability_notes"].get("planned_methods").is_none());
    assert_eq!(
        hello["contracts"]["active_exceptions"],
        json!([
            "xemu.state-save.frozen-only",
            "xemu.state-load.frozen-only",
            "xemu.state-load.same-generation-only"
        ])
    );
    assert_eq!(hello["name"], "xbox-test");
    assert_eq!(hello["session_token"], "token");
    assert!(hello["host_features"]
        .as_array()
        .unwrap()
        .iter()
        .any(|feature| feature == "controlled_start"));
    assert_eq!(bridge.gdb.calls, ["g", "c"]);
    bridge.qmp.assert_drained();
}

#[test]
fn reconnect_hello_observes_running_machine_without_reapplying_launch_start() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("xemu-emucap-status", extension(0, false, 0, false)),
            ("query-status", machine(false)),
            ("query-status", machine(true)),
            ("xemu-emucap-status", extension(3, false, 0, false)),
            ("query-status", machine(true)),
        ]),
        FakeGdb::with_replies(vec![("g", &hex::encode([0_u8; 64]))]),
        temp.path(),
    );

    let first = result(bridge.handle_request(Request::new(10, "hello", json!({}))));
    let reconnect = result(bridge.handle_request(Request::new(11, "hello", json!({}))));

    assert_eq!(first["launch_start"]["controlled"], false);
    assert_eq!(reconnect["launch_start"]["controlled"], false);
    assert_eq!(bridge.gdb.calls, ["g", "c"]);
    bridge.qmp.assert_drained();
}

#[test]
fn controlled_hello_proves_frozen_qmp_and_gdb_readiness() {
    let temp = tempfile::tempdir().unwrap();
    let registers = hex::encode([0_u8; 64]);
    let identity = XemuMachineIdentity {
        mcpx_sha256: Some("1".repeat(64)),
        flash_sha256: Some("2".repeat(64)),
        hdd_template_sha256: Some("3".repeat(64)),
        eeprom_initial_sha256: Some("4".repeat(64)),
    };
    let mut controlled = XemuBridge::new(
        FakeQmp::new(vec![
            ("xemu-emucap-status", extension(0, false, 0, false)),
            ("query-status", machine(false)),
        ]),
        FakeGdb::with_replies(vec![("g", &registers)]),
        GdbBridgeEnv::default(),
        temp.path().into(),
        true,
        identity,
        state_environment(temp.path()),
    );
    let hello = result(controlled.handle_request(Request::new(12, "hello", json!({}))));
    assert_eq!(hello["launch_start"]["controlled"], true);
    assert_eq!(hello["launch_start"]["boundary"], "pre_first_instruction");
    assert_eq!(
        hello["launch_start"]["reset_linear_address"],
        0xffff_fff0u64
    );
    assert_eq!(
        hello["machine_inputs"]["hdd"]["template_sha256"],
        "3".repeat(64)
    );
    assert_eq!(controlled.gdb.calls, ["g"]);
    controlled.qmp.assert_drained();
}

#[test]
fn main_memory_cross_boundary_fails_before_touching_a_transport() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(FakeQmp::new(vec![]), FakeGdb::empty(), temp.path());
    let response = bridge.handle_request(Request::new(
        2,
        "read_memory",
        json!({"memory_type":"main", "address":XBOX_RAM_SIZE - 1, "length":2}),
    ));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert!(bridge.qmp.calls.is_empty());
    assert!(bridge.gdb.calls.is_empty());
}

#[test]
fn set_input_maps_buttons_and_triggers_then_returns_native_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("xemu-emucap-set-input", json!({})),
            ("xemu-emucap-status", extension(0, false, 0, true)),
            ("xemu-emucap-set-input", json!({})),
            ("xemu-emucap-status", extension(0, false, 0, false)),
        ]),
        FakeGdb::empty(),
        temp.path(),
    );
    let engaged = result(bridge.handle_request(Request::new(
        3,
        "set_input",
        json!({"buttons":["a", "up", "lt"]}),
    )));
    assert_eq!(engaged["override_engaged"], true);
    let args = bridge.qmp.calls[0].1.as_ref().unwrap();
    assert_eq!(args["buttons"], (1u16 << 0) | (1u16 << 5));
    assert_eq!(args["ltrigger"], i16::MAX);

    let released =
        result(bridge.handle_request(Request::new(4, "set_input", json!({"buttons":[]}))));
    assert_eq!(released["ownership"], "native");
    assert_eq!(bridge.qmp.calls[2].1.as_ref().unwrap()["engaged"], false);
    bridge.qmp.assert_drained();
}

#[test]
fn set_input_maps_axis_only_state_and_neutral_axis_still_owns_the_controller() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("xemu-emucap-set-input", json!({})),
            ("xemu-emucap-status", extension(0, false, 0, true)),
            ("query-status", machine(false)),
            ("xemu-emucap-status", extension(0, false, 0, true)),
        ]),
        FakeGdb::empty(),
        temp.path(),
    );
    let engaged = result(bridge.handle_request(Request::new(
        31,
        "set_input",
        json!({
            "buttons":[],
            "axes":{"left_x":-32768, "left_y":32767, "right_x":0, "right_trigger":12345}
        }),
    )));
    assert_eq!(engaged["override_engaged"], true);
    assert_eq!(engaged["axes"]["right_x"], 0);
    let args = bridge.qmp.calls[0].1.as_ref().unwrap();
    assert_eq!(args["lstick-x"], -32768);
    assert_eq!(args["lstick-y"], 32767);
    assert_eq!(args["rstick-x"], 0);
    assert_eq!(args["rtrigger"], 12345);

    let status = result(bridge.handle_request(Request::new(32, "status", json!({}))));
    assert_eq!(status["input_override"]["engaged"], true);
    assert_eq!(status["input_override"]["axes"]["right_x"], 0);
    bridge.qmp.assert_drained();
}

#[test]
fn set_input_rejects_invalid_or_conflicting_axes_before_qmp_mutation() {
    for params in [
        json!({"buttons":[], "axes":{"throttle":1}}),
        json!({"buttons":[], "axes":{"left_trigger":-1}}),
        json!({"buttons":[], "axes":{"left_x":32768}}),
        json!({"buttons":["l"], "axes":{"left_trigger":100}}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut bridge = bridge(FakeQmp::new(vec![]), FakeGdb::empty(), temp.path());
        let response = bridge.handle_request(Request::new(33, "set_input", params));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "bad_params");
        assert!(bridge.qmp.calls.is_empty());
    }
}

#[test]
fn save_state_rejects_running_vm_before_snapshot_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = state_bridge(
        FakeQmp::new(vec![("query-status", machine(true))]),
        FakeGdb::empty(),
        temp.path(),
    );
    let response = bridge.handle_request(Request::new(
        101,
        "save_state",
        json!({"path":temp.path().join("running-state.json")}),
    ));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_state");
    assert_eq!(bridge.qmp.calls.len(), 1);
    bridge.qmp.assert_drained();
}

#[test]
fn save_state_publishes_generation_media_eeprom_and_host_identity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("xbox-state.json");
    let saved = save_state_container(temp.path(), &path);
    assert_eq!(saved["state"], "frozen");
    assert_eq!(saved["scope"], "same_generation");
    assert_eq!(saved["boundary"], "frame_boundary");
    assert_eq!(saved["frame"], 42);

    let container: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(container["format"], "emucap-xemu-state");
    assert_eq!(container["launch_id"], "launch-state-test");
    assert_eq!(container["host_build"]["commit"], "1".repeat(40));
    assert_eq!(
        container["storage"]["eeprom_hex"].as_str().unwrap().len(),
        512
    );
    assert_eq!(
        container["media"]["path"],
        temp.path().join("game.xiso").display().to_string()
    );
    assert_eq!(container["controller"]["port"], 0);
}

#[test]
fn load_state_restores_eeprom_and_proves_both_transports_while_frozen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("xbox-state.json");
    save_state_container(temp.path(), &path);
    let container: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let snapshot_tag = container["storage"]["snapshot_tag"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::write(temp.path().join("eeprom.bin"), [0xaa_u8; 256]).unwrap();
    let registers = hex::encode([0_u8; 64]);
    let mut bridge = state_bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("query-block", state_block_layout(temp.path())),
            ("snapshot-save", json!({})),
            ("query-jobs", concluded_job("emucap-save-1", "save")),
            ("job-dismiss", json!({})),
            ("snapshot-load", json!({})),
            ("query-jobs", concluded_job("emucap-load-2", "load")),
            ("job-dismiss", json!({})),
            ("query-status", machine(false)),
            ("xemu-emucap-set-input", json!({})),
            ("xemu-emucap-status", extension(84, false, 0, true)),
            ("xemu-emucap-set-input", json!({})),
            ("xemu-emucap-status", extension(84, false, 0, false)),
            ("query-block", state_block_layout(temp.path())),
            ("xemu-emucap-status", extension(84, false, 0, false)),
            ("snapshot-delete", json!({})),
            ("query-jobs", concluded_job("emucap-delete-3", "delete")),
            ("job-dismiss", json!({})),
        ]),
        FakeGdb::with_replies(vec![("g", &registers)]),
        temp.path(),
    );
    let loaded =
        result(bridge.handle_request(Request::new(102, "load_state", json!({"path":path}))));
    assert_eq!(loaded["state"], "frozen");
    assert_eq!(loaded["backend_round_trip_verified"], true);
    assert_eq!(loaded["control_serviceable"], true);
    assert_eq!(loaded["cleanup"]["rollback_snapshot"], "deleted");
    assert_eq!(
        std::fs::read(temp.path().join("eeprom.bin")).unwrap(),
        [0_u8; 256]
    );
    let target_load = bridge
        .qmp
        .calls
        .iter()
        .find(|(command, _)| command == "snapshot-load")
        .unwrap();
    assert_eq!(target_load.1.as_ref().unwrap()["tag"], snapshot_tag);
    assert_eq!(bridge.gdb.calls, ["g"]);
    bridge.qmp.assert_drained();
}

#[test]
fn load_state_rejects_foreign_generation_before_snapshot_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("xbox-state.json");
    save_state_container(temp.path(), &path);
    let mut container: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    container["launch_id"] = json!("launch-foreign");
    std::fs::write(&path, serde_json::to_vec_pretty(&container).unwrap()).unwrap();
    let mut bridge = state_bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("query-block", state_block_layout(temp.path())),
        ]),
        FakeGdb::empty(),
        temp.path(),
    );
    let response = bridge.handle_request(Request::new(103, "load_state", json!({"path":path})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_state");
    assert!(bridge
        .qmp
        .calls
        .iter()
        .all(|(command, _)| command != "snapshot-load" && command != "snapshot-save"));
    bridge.qmp.assert_drained();
}

#[test]
fn load_state_rejects_changed_disc_before_snapshot_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("xbox-state.json");
    save_state_container(temp.path(), &path);
    std::fs::write(temp.path().join("game.xiso"), b"changed-state-test-xiso").unwrap();
    let mut bridge = state_bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("query-block", state_block_layout(temp.path())),
        ]),
        FakeGdb::empty(),
        temp.path(),
    );
    let response = bridge.handle_request(Request::new(104, "load_state", json!({"path":path})));
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert_eq!(error.kind, "bad_state");
    assert!(error.message.contains("exact disc"));
    assert!(bridge
        .qmp
        .calls
        .iter()
        .all(|(command, _)| command != "snapshot-load" && command != "snapshot-save"));
    bridge.qmp.assert_drained();
}

#[test]
fn cached_disc_identity_is_invalidated_by_same_generation_file_drift() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("xbox-state.json");
    let mut bridge = state_bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("query-block", state_block_layout(temp.path())),
            ("snapshot-save", json!({})),
            ("query-jobs", concluded_job("emucap-save-1", "save")),
            ("job-dismiss", json!({})),
            ("query-status", machine(false)),
            ("xemu-emucap-status", extension(42, false, 0, false)),
            ("query-status", machine(false)),
            ("query-block", state_block_layout(temp.path())),
        ]),
        FakeGdb::empty(),
        temp.path(),
    );
    result(bridge.handle_request(Request::new(106, "save_state", json!({"path":path}))));
    std::fs::write(temp.path().join("game.xiso"), b"same-generation-drift").unwrap();
    let response = bridge.handle_request(Request::new(107, "load_state", json!({"path":path})));
    assert!(!response.ok);
    assert!(response.error.unwrap().message.contains("exact disc"));
    assert!(bridge
        .qmp
        .calls
        .iter()
        .skip(7)
        .all(|(command, _)| command != "snapshot-load" && command != "snapshot-save"));
    bridge.qmp.assert_drained();
}

#[test]
fn failed_load_restores_the_prior_frozen_snapshot_and_keeps_control_serviceable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("xbox-state.json");
    save_state_container(temp.path(), &path);
    std::fs::write(temp.path().join("eeprom.bin"), [0xaa_u8; 256]).unwrap();
    let registers = hex::encode([0_u8; 64]);
    let mut bridge = state_bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("query-block", state_block_layout(temp.path())),
            ("snapshot-save", json!({})),
            ("query-jobs", concluded_job("emucap-save-1", "save")),
            ("job-dismiss", json!({})),
            ("snapshot-load", json!({})),
            (
                "query-jobs",
                failed_job("emucap-load-2", "load", "injected load failure"),
            ),
            ("job-dismiss", json!({})),
            ("snapshot-load", json!({})),
            ("query-jobs", concluded_job("emucap-load-3", "load")),
            ("job-dismiss", json!({})),
            ("xemu-emucap-set-input", json!({})),
            ("xemu-emucap-status", extension(42, false, 0, true)),
            ("xemu-emucap-set-input", json!({})),
            ("xemu-emucap-status", extension(42, false, 0, false)),
            ("query-status", machine(false)),
            ("snapshot-delete", json!({})),
            ("query-jobs", concluded_job("emucap-delete-4", "delete")),
            ("job-dismiss", json!({})),
        ]),
        FakeGdb::with_replies(vec![("g", &registers)]),
        temp.path(),
    );
    let response = bridge.handle_request(Request::new(105, "load_state", json!({"path":path})));
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert_eq!(error.kind, "emulator_error");
    assert!(error.message.contains("prior frozen state was restored"));
    assert_eq!(
        std::fs::read(temp.path().join("eeprom.bin")).unwrap(),
        [0xaa_u8; 256]
    );
    assert!(bridge.state_integrity_error.is_none());
    bridge.qmp.assert_drained();
}

#[test]
fn probe_rejects_invalid_bounds_before_loading_state() {
    for params in [
        json!({
            "state":"/tmp/not-consumed.json", "frame":1,
            "memory_type":"main", "address":XBOX_RAM_SIZE - 1, "length":2
        }),
        json!({
            "state":"/tmp/not-consumed.json",
            "frame":crate::live::temporal::MAX_SYNC_ADVANCE_COUNT + 1,
            "memory_type":"main", "address":0, "length":1
        }),
        json!({
            "state":"/tmp/not-consumed.json", "frame":0,
            "memory_type":"main", "address":0, "length":0
        }),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut bridge = state_bridge(FakeQmp::new(vec![]), FakeGdb::empty(), temp.path());
        let response = bridge.handle_request(Request::new(108, "probe", params));
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().kind, "bad_params");
        assert!(bridge.qmp.calls.is_empty());
        assert!(bridge.gdb.calls.is_empty());
    }
}

#[test]
fn probe_loads_advances_reads_and_returns_one_frozen_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("xbox-state.json");
    save_state_container(temp.path(), &path);
    let mut qmp = load_state_replies(temp.path(), 84);
    qmp.extend([
        ("query-status", machine(false)),
        ("xemu-emucap-status", extension(84, false, 0, false)),
        ("xemu-emucap-status", extension(84, false, 0, false)),
        ("xemu-emucap-arm-frame-step", json!({})),
        ("xemu-emucap-status", extension(87, false, 0, false)),
        ("query-status", machine(false)),
        ("query-status", machine(false)),
    ]);
    let registers = i386_register_packet(0x8001_0000);
    let mut gdb = FakeGdb::with_replies(vec![
        ("g", &registers),
        ("g", &registers),
        ("Qqemu.PhyMemMode:1", "OK"),
        ("m1000,4", "deadbeef"),
        ("Qqemu.PhyMemMode:0", "OK"),
    ]);
    gdb.receive_replies.push_back(Ok("S02".into()));
    let mut bridge = state_bridge(FakeQmp::new(qmp), gdb, temp.path());

    let probed = result(bridge.handle_request(Request::new(
        109,
        "probe",
        json!({
            "state":path, "frame":3,
            "memory_type":"main", "address":"0x1000", "length":4
        }),
    )));

    assert_eq!(probed["status"], "completed");
    assert_eq!(probed["state"], "frozen");
    assert_eq!(probed["requested_frames"], 3);
    assert_eq!(probed["completed_frames"], 3);
    assert_eq!(probed["start_frame"], 84);
    assert_eq!(probed["end_frame"], 87);
    assert_eq!(probed["hex"], "deadbeef");
    assert_eq!(probed["input_override"]["ownership"], "native");
    assert_eq!(probed["base_state"]["launch_id"], "launch-state-test");
    assert_eq!(probed["base_state"]["sha256"].as_str().unwrap().len(), 64);
    bridge.qmp.assert_drained();
}

#[test]
fn probe_breakpoint_stop_preempts_target_and_reads_the_interrupted_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("xbox-state.json");
    save_state_container(temp.path(), &path);
    let mut qmp = load_state_replies(temp.path(), 84);
    qmp.extend([
        ("query-status", machine(false)),
        ("xemu-emucap-status", extension(84, false, 0, false)),
        ("xemu-emucap-status", extension(84, false, 0, false)),
        ("xemu-emucap-arm-frame-step", json!({})),
        ("xemu-emucap-status", extension(85, true, 2, false)),
        ("query-status", machine(false)),
        ("xemu-emucap-cancel-frame-step", json!({})),
        ("query-status", machine(false)),
    ]);
    let registers = i386_register_packet(0x8001_0000);
    let mut gdb = FakeGdb::with_replies(vec![
        ("g", &registers),
        ("g", &registers),
        ("Qqemu.PhyMemMode:1", "OK"),
        ("m1000,4", "01020304"),
        ("Qqemu.PhyMemMode:0", "OK"),
    ]);
    gdb.receive_replies
        .push_back(Ok("T05watch:80001000;".into()));
    let mut bridge = state_bridge(FakeQmp::new(qmp), gdb, temp.path());

    let probed = result(bridge.handle_request(Request::new(
        110,
        "probe",
        json!({
            "state":path, "frame":3,
            "memory_type":"main", "address":"0x1000", "length":4
        }),
    )));

    assert_eq!(probed["status"], "interrupted");
    assert_eq!(probed["completed_frames"], 1);
    assert_eq!(probed["end_frame"], 85);
    assert_eq!(probed["hex"], "01020304");
    assert_eq!(bridge.events.len(), 1);
    assert_eq!(bridge.events[0]["watch_address"], 0x8000_1000u64);
    bridge.qmp.assert_drained();
}

#[test]
fn frame_step_completes_only_at_exact_frozen_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("xemu-emucap-status", extension(100, false, 0, false)),
            ("xemu-emucap-status", extension(100, false, 0, false)),
            ("xemu-emucap-arm-frame-step", json!({})),
            ("xemu-emucap-status", extension(103, false, 0, false)),
            ("query-status", machine(false)),
        ]),
        frame_step_gdb("S02"),
        temp.path(),
    );
    let completed =
        result(bridge.handle_request(Request::new(5, "step", json!({"unit":"frames", "count":3}))));
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["count"], 3);
    assert_eq!(completed["state"], "frozen");
    assert_eq!(bridge.qmp.calls[3].1.as_ref().unwrap()["count"], json!(3));
    assert!(bridge.gdb.calls.iter().any(|command| command == "c"));
    bridge.qmp.assert_drained();
}

#[test]
fn debugger_stop_preempts_frame_completion_and_cancels_remaining_work() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("xemu-emucap-status", extension(200, false, 0, false)),
            ("xemu-emucap-status", extension(200, false, 0, false)),
            ("xemu-emucap-arm-frame-step", json!({})),
            ("xemu-emucap-status", extension(201, true, 2, false)),
            ("query-status", json!({"running":false, "status":"debug"})),
            ("xemu-emucap-cancel-frame-step", json!({})),
        ]),
        frame_step_gdb("S05"),
        temp.path(),
    );
    let interrupted =
        result(bridge.handle_request(Request::new(6, "step", json!({"unit":"frames", "count":3}))));
    assert_eq!(interrupted["status"], "interrupted");
    assert_eq!(interrupted["count"], 1);
    assert_eq!(bridge.events.len(), 1);
    bridge.qmp.assert_drained();
}

#[test]
fn immediate_watchpoint_stop_is_consumed_before_the_next_qmp_poll() {
    let temp = tempfile::tempdir().unwrap();
    let stop_observed = Rc::new(Cell::new(false));
    let qmp = FakeQmp::new(vec![
        ("query-status", machine(false)),
        ("xemu-emucap-status", extension(300, false, 0, false)),
        ("xemu-emucap-status", extension(300, false, 0, false)),
        ("xemu-emucap-arm-frame-step", json!({})),
        ("xemu-emucap-status", extension(300, true, 3, false)),
        ("query-status", json!({"running":false, "status":"debug"})),
        ("xemu-emucap-cancel-frame-step", json!({})),
    ])
    .require_stop_before_call(4, Rc::clone(&stop_observed));
    let mut bridge = bridge(
        qmp,
        frame_step_gdb("T05watch:80001000;thread:01;").mark_stop_observed(stop_observed),
        temp.path(),
    );

    let interrupted = result(bridge.handle_request(Request::new(
        61,
        "step",
        json!({"unit":"frames", "count":3}),
    )));

    assert_eq!(interrupted["status"], "interrupted");
    assert_eq!(interrupted["count"], 0);
    assert_eq!(bridge.events[0]["watch_address"], 0x8000_1000u64);
    bridge.qmp.assert_drained();
}

#[test]
fn watchpoint_preempts_instruction_step_without_a_followup_gdb_command() {
    let temp = tempfile::tempdir().unwrap();
    let registers = i386_register_packet(0x1000);
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("xemu-emucap-status", extension(300, false, 0, false)),
        ]),
        FakeGdb::with_replies(vec![
            ("g", &registers),
            ("s", "T05thread:01;watch:80002000;"),
        ]),
        temp.path(),
    );

    let interrupted = result(bridge.handle_request(Request::new(
        65,
        "step",
        json!({"unit":"instructions", "count":5}),
    )));

    assert_eq!(interrupted["status"], "interrupted");
    assert_eq!(interrupted["count"], 1);
    assert_eq!(interrupted["requested"], 5);
    assert_eq!(bridge.events[0]["watch_address"], 0x8000_2000u64);
    assert_eq!(bridge.gdb.calls, ["g", "s"]);
    bridge.qmp.assert_drained();
}

#[test]
fn exec_breakpoint_preempts_instruction_loop_at_the_resulting_pc() {
    let temp = tempfile::tempdir().unwrap();
    let before = i386_register_packet(0x1000);
    let hit = i386_register_packet(0x1234);
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("query-status", machine(false)),
            ("xemu-emucap-status", extension(300, false, 0, false)),
        ]),
        FakeGdb::with_replies(vec![("g", &before), ("s", "T05thread:01;"), ("g", &hit)]),
        temp.path(),
    );
    bridge.breakpoints.insert(
        1,
        XemuBreakpoint {
            kind: "exec".into(),
            memory_type: "cpu".into(),
            start: 0x1234,
            end: 0x1234,
            absolute: 0x1234,
            length: 1,
            ztype: 0,
        },
    );

    let interrupted = result(bridge.handle_request(Request::new(
        66,
        "step",
        json!({"unit":"instructions", "count":5}),
    )));

    assert_eq!(interrupted["status"], "interrupted");
    assert_eq!(interrupted["count"], 1);
    assert_eq!(bridge.events[0]["breakpoint_id"], 1);
    assert_eq!(bridge.events[0]["pc"], 0x1234);
    assert_eq!(bridge.gdb.calls, ["g", "s", "g"]);
    bridge.qmp.assert_drained();
}

#[test]
fn screenshot_accepts_only_the_request_bound_managed_png() {
    let temp = tempfile::tempdir().unwrap();
    let filename = "emucap-00000000000000000001.png";
    let file = std::fs::File::create(temp.path().join(filename)).unwrap();
    let mut encoder = png::Encoder::new(file, 2, 1);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[255, 0, 0, 0, 255, 0]).unwrap();
    writer.finish().unwrap();

    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("xemu-emucap-request-screenshot", json!({})),
            (
                "xemu-emucap-screenshot-status",
                json!({"request-id":1, "state":"completed", "filename":filename}),
            ),
            ("xemu-emucap-status", extension(77, false, 0, false)),
        ]),
        FakeGdb::empty(),
        temp.path(),
    );
    let screenshot = result(bridge.handle_request(Request::new(7, "screenshot", json!({}))));
    assert_eq!(screenshot["width"], 2);
    assert_eq!(screenshot["height"], 1);
    assert_eq!(screenshot["frame"], 77);
    assert!(!temp.path().join(filename).exists());
    bridge.qmp.assert_drained();
}

#[test]
fn i386_register_packet_is_exposed_with_named_fields() {
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = Vec::new();
    for value in 0u32..16 {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let mut bridge = bridge(
        FakeQmp::new(vec![("query-status", machine(false))]),
        FakeGdb::with_replies(vec![("g", &hex::encode(bytes))]),
        temp.path(),
    );
    let state = result(bridge.handle_request(Request::new(8, "get_state", json!({}))));
    assert_eq!(state["state"]["cpu.eax"], 0);
    assert_eq!(state["state"]["cpu.eip"], 8);
}

#[test]
fn disassemble_decodes_target_i386_bytes_without_host_monitor_support() {
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = vec![0x90, 0xc3];
    bytes.resize(30, 0);
    let mut bridge = bridge(
        FakeQmp::new(vec![("query-status", machine(false))]),
        FakeGdb::with_replies(vec![
            ("Qqemu.PhyMemMode:1", "OK"),
            ("m0,1e", &hex::encode(bytes)),
            ("Qqemu.PhyMemMode:0", "OK"),
        ]),
        temp.path(),
    );
    let value = result(bridge.handle_request(Request::new(
        9,
        "disassemble",
        json!({"memory_type":"main", "address":0, "count":2}),
    )));
    assert_eq!(value["instructions"][0]["text"], "nop");
    assert_eq!(value["instructions"][1]["text"], "ret");
    assert_eq!(value["instructions"][0]["bytes"], "90");
    assert_eq!(
        bridge.gdb.calls,
        ["Qqemu.PhyMemMode:1", "m0,1e", "Qqemu.PhyMemMode:0"]
    );
}

#[test]
fn main_memory_reads_physical_ram_and_restores_virtual_mode() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(
        FakeQmp::new(vec![("query-status", machine(false))]),
        FakeGdb::with_replies(vec![
            ("Qqemu.PhyMemMode:1", "OK"),
            ("m20,4", "deadbeef"),
            ("Qqemu.PhyMemMode:0", "OK"),
        ]),
        temp.path(),
    );

    let value = result(bridge.handle_request(Request::new(
        63,
        "read_memory",
        json!({"memory_type":"main", "address":0x20, "length":4}),
    )));

    assert_eq!(value["hex"], "deadbeef");
    assert_eq!(value["address"], 0x20);
    assert_eq!(
        bridge.gdb.calls,
        ["Qqemu.PhyMemMode:1", "m20,4", "Qqemu.PhyMemMode:0"]
    );
}

#[test]
fn failed_main_memory_read_still_restores_virtual_mode() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = bridge(
        FakeQmp::new(vec![("query-status", machine(false))]),
        FakeGdb::with_replies(vec![
            ("Qqemu.PhyMemMode:1", "OK"),
            ("m0,4", "E14"),
            ("Qqemu.PhyMemMode:0", "OK"),
        ]),
        temp.path(),
    );

    let response = bridge.handle_request(Request::new(
        64,
        "read_memory",
        json!({"memory_type":"main", "address":0, "length":4}),
    ));

    assert!(!response.ok);
    assert_eq!(
        bridge.gdb.calls,
        ["Qqemu.PhyMemMode:1", "m0,4", "Qqemu.PhyMemMode:0"]
    );
}

#[test]
fn disassemble_defaults_to_the_cpu_virtual_address_view() {
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = vec![0x90, 0xc3];
    bytes.resize(30, 0);
    let mut bridge = bridge(
        FakeQmp::new(vec![("query-status", machine(false))]),
        FakeGdb::with_replies(vec![("m10e3a4,1e", &hex::encode(bytes))]),
        temp.path(),
    );

    let value = result(bridge.handle_request(Request::new(
        62,
        "disassemble",
        json!({"address":0x10e3a4, "count":2}),
    )));

    assert_eq!(value["memory_type"], "cpu");
    assert_eq!(value["instructions"][0]["addr"], 0x10e3a4u64);
    assert_eq!(value["instructions"][0]["text"], "nop");
}

#[test]
fn disassemble_caps_lookahead_at_the_cpu_address_space_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = vec![0x90, 0xc3];
    bytes.resize(16, 0);
    let mut bridge = bridge(
        FakeQmp::new(vec![("query-status", machine(false))]),
        FakeGdb::with_replies(vec![("mfffffff0,10", &hex::encode(bytes))]),
        temp.path(),
    );

    let value = result(bridge.handle_request(Request::new(
        65,
        "disassemble",
        json!({"address":0xfffffff0u64, "count":2}),
    )));

    assert_eq!(value["instructions"][0]["addr"], 0xfffffff0u64);
    assert_eq!(value["instructions"][0]["text"], "nop");
    assert_eq!(value["instructions"][1]["text"], "ret");
    assert_eq!(bridge.gdb.calls, ["mfffffff0,10"]);
}

#[test]
fn debug_runstate_queries_one_gdb_stop_and_does_not_duplicate_it() {
    let temp = tempfile::tempdir().unwrap();
    let mut registers = Vec::new();
    for value in 0u32..16 {
        registers.extend_from_slice(&value.to_le_bytes());
    }
    let mut gdb = FakeGdb::with_replies(vec![("g", &hex::encode(registers))]);
    gdb.asynchronous
        .extend([Ok(None), Ok(Some("T05thread:01;".into())), Ok(None)]);
    let mut bridge = bridge(
        FakeQmp::new(vec![
            ("query-status", json!({"running":false, "status":"debug"})),
            ("query-status", json!({"running":false, "status":"debug"})),
        ]),
        gdb,
        temp.path(),
    );
    let first = result(bridge.handle_request(Request::new(10, "poll_events", json!({}))));
    assert_eq!(first["events"].as_array().unwrap().len(), 1);
    assert_eq!(first["events"][0]["signal"], "05");

    let second = result(bridge.handle_request(Request::new(11, "poll_events", json!({}))));
    assert!(second["events"].as_array().unwrap().is_empty());
    assert_eq!(bridge.gdb.calls, ["g"]);
}
