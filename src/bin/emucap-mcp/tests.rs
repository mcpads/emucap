use super::*;

use crate::args::{Num, StatusArgs, VerifyDeterminismArgs, WriteMemoryArgs, WriteMemoryFileArgs};
use emucap::analysis::bisect::{CmpOp, Predicate};
use emucap::analysis::regression;
use emucap::live::link::{Capabilities, EmulatorIdentity, FakeLink};

struct DetReplayLink {
    caps: Capabilities,
    obs_queue: std::collections::VecDeque<&'static str>,
}

struct StepWireLink {
    caps: Capabilities,
    last_method: Option<String>,
    last_params: Option<serde_json::Value>,
}

struct InputWireLink {
    caps: Capabilities,
    calls: Vec<(String, serde_json::Value)>,
}

impl InputWireLink {
    fn new() -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec!["status".into(), "touch".into()],
                memory_types: vec![],
                memory_regions: vec![],
                breakpoint_kinds: vec![],
                contracts: emucap::contracts::ContractAdvertisement::Reported(
                    emucap::contracts::AdvertisedContracts {
                        catalog: emucap::contracts::CATALOG_ID.into(),
                        active_exceptions: vec![],
                        constraints: None,
                        authority: None,
                    },
                ),
                recording: None,
                identity: EmulatorIdentity::default(),
            },
            calls: vec![],
        }
    }
}

impl EmulatorLink for InputWireLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        if method == "status" {
            return Ok(serde_json::json!({"connected": true, "state": "frozen"}));
        }
        self.calls.push((method.into(), params));
        Ok(serde_json::json!({"status": "completed"}))
    }
}

impl StepWireLink {
    fn new() -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec!["step".into()],
                memory_types: vec![],
                memory_regions: vec![],
                breakpoint_kinds: vec![],
                contracts: emucap::contracts::ContractAdvertisement::Unreported,
                recording: None,
                identity: EmulatorIdentity::default(),
            },
            last_method: None,
            last_params: None,
        }
    }
}

impl EmulatorLink for StepWireLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        self.last_method = Some(method.into());
        self.last_params = Some(params);
        Ok(serde_json::json!({"status": "completed", "state": "frozen"}))
    }
}

impl DetReplayLink {
    fn new(methods: &[&str]) -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: methods.iter().map(|method| (*method).to_string()).collect(),
                memory_types: vec![],
                memory_regions: vec![],
                breakpoint_kinds: vec![],
                contracts: emucap::contracts::ContractAdvertisement::Unreported,
                recording: None,
                identity: EmulatorIdentity::default(),
            },
            obs_queue: std::collections::VecDeque::new(),
        }
    }

    fn obs(mut self, hexes: &[&'static str]) -> Self {
        self.obs_queue = hexes.iter().copied().collect();
        self
    }
}

impl EmulatorLink for DetReplayLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        match method {
            "reset" | "pause" | "set_input" | "step" | "clear_all_breakpoints" | "resume" => {
                Ok(serde_json::json!({}))
            }
            "read_memory" => Ok(serde_json::json!({
                "hex": self.obs_queue.pop_front().unwrap_or("00")
            })),
            other => Err(LinkError::Protocol(format!("unexpected: {other}"))),
        }
    }
}

fn det_input_case() -> (tempfile::TempDir, std::path::PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("c");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("inputs.movie"), "0:enter\n").unwrap();
    let case = regression::Case {
        format_version: regression::CASE_FORMAT_VERSION,
        id: "c".into(),
        description: "det".into(),
        rom: regression::RomRef {
            sha1: "unused".into(),
            path_hint: "x".into(),
        },
        repro: regression::Repro::InputReplay {
            start: "reset".into(),
            movie: "inputs.movie".into(),
            anchor: None,
        },
        predicate: Predicate {
            memory_type: "w".into(),
            address: 0,
            length: 2,
            op: CmpOp::Eq,
            value: 0,
        },
        expect: regression::Expect::Absent,
    };
    regression::save_case(&directory, &case).unwrap();
    (temporary, directory)
}

/// CallToolResult의 텍스트 본문을 추출한다(검증용).
fn body_text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("")
}

fn contains_hangul(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            '\u{1100}'..='\u{11ff}' | '\u{3130}'..='\u{318f}' | '\u{ac00}'..='\u{d7af}'
        )
    })
}

#[test]
fn server_info_identifies_the_control_binary() {
    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let info = Emucap::new(shared).get_info();
    assert_eq!(info.server_info.name, "emucap-mcp");
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.instructions.as_deref(), Some(SERVER_INSTRUCTIONS));
}

#[test]
fn control_mcp_consumer_metadata_is_english() {
    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let server = Emucap::new(shared);
    let instructions = server.get_info().instructions.unwrap_or_default();
    let tools = serde_json::to_string(&server.tool_router.list_all()).unwrap();
    assert!(!contains_hangul(&instructions), "{instructions}");
    assert!(!contains_hangul(&tools), "{tools}");
    assert!(
        instructions.len() <= 4 * 1024,
        "Server Instructions exceeded the public context budget"
    );
}

#[tokio::test]
async fn analysis_dispatcher_is_the_only_analysis_tool_surface() {
    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let server = Emucap::new(shared);

    let initial: Vec<_> = server
        .visible_tools()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert!(initial.iter().any(|name| name == "analysis"));
    for name in ["regression_run", "verify_determinism"] {
        assert!(!initial.iter().any(|visible| visible == name));
        assert!(server.get_tool(name).is_none());
    }

    let description = server
        .analysis(Parameters(AnalysisArgs {
            operation: AnalysisOperation::Describe,
            arguments: None,
        }))
        .await
        .structured_content
        .expect("analysis description");
    assert_eq!(description["surface"], "analysis");
    assert!(description["operations"]["regression_run"]["arguments_schema"].is_object());
    assert!(description["operations"]["verify_determinism"]["arguments_schema"].is_object());
    assert_eq!(description["next_action"]["tool"], "analysis");
}

#[test]
fn front_panel_exposes_basic_controls_and_hides_drawer_operations() {
    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let server = Emucap::new(shared);
    let visible: std::collections::BTreeSet<_> = server
        .visible_tools()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    let expected: std::collections::BTreeSet<_> = emucap::contracts::catalog()
        .features
        .iter()
        .filter(|feature| feature.surface == "public")
        .flat_map(|feature| match &feature.route {
            Some(route) => vec![route.clone()],
            None => feature.methods.clone(),
        })
        .collect();
    assert_eq!(visible, expected, "tools/list drifted from contract routes");

    for name in [
        "bootstrap",
        "launch_plan",
        "launch",
        "reattach",
        "stop",
        "status",
        "tap",
        "change_media",
        "write_memory",
        "disassemble",
        "call_stack",
        "set_breakpoint",
        "clear_breakpoint",
        "list_breakpoints",
        "clear_all_breakpoints",
        "poll_events",
        "pause",
        "step",
        "resume",
        "input_control",
        "debug",
        "analysis",
    ] {
        assert!(visible.contains(name), "missing front-panel tool: {name}");
    }
    for name in [
        "run_frames",
        "wait_for_running_frames",
        "advance_and_freeze",
        "set_input",
        "touch",
        "press_buttons",
        "hold_touch",
        "release_touch",
        "pulse_touch_while_running",
        "hold_until",
        "record_window",
        "power_cycle",
    ] {
        assert!(!visible.contains(name), "drawer operation leaked: {name}");
        assert!(server.get_tool(name).is_none());
    }
}

#[test]
fn drawer_operation_registries_match_the_contract_catalog() {
    for (route, implementation) in [
        ("debug", debug_surface::operation_ids()),
        ("input_control", input_surface::operation_ids()),
    ] {
        let expected: std::collections::BTreeSet<_> = emucap::contracts::catalog()
            .features
            .iter()
            .filter(|feature| feature.route.as_deref() == Some(route))
            .flat_map(|feature| feature.methods.iter().map(String::as_str))
            .collect();
        let actual: std::collections::BTreeSet<_> = implementation.iter().copied().collect();
        assert_eq!(actual, expected, "{route} drawer drifted from catalog");
    }
}

#[test]
fn drawers_publish_only_current_operations_and_bind_execution_to_the_revision() {
    let status = serde_json::json!({
        "contracts": {"state": "validated"},
        "capability_revision": "revision-a",
        "methods": [
            "probe",
            "power_cycle",
            "disassemble",
            "set_input",
            "hold_touch",
            "release_touch",
            "pulse_touch_while_running"
        ],
        "input_buttons": ["a", "b"]
    });
    let debug = debug_surface::describe(&status);
    assert!(debug["operations"]["probe"].is_object());
    assert!(debug["operations"]["power_cycle"].is_object());
    assert!(debug["operations"].get("disassemble").is_none());
    assert!(debug["operations"].get("record_window").is_none());
    assert_eq!(debug["capability_revision"], "revision-a");
    assert!(!serde_json::to_string(&debug).unwrap().contains("snes_"));

    let input = input_surface::describe(&status);
    assert!(input["operations"]["set_input"].is_object());
    assert!(input["operations"].get("touch").is_none());
    assert!(input["operations"]["hold_touch"]["arguments_schema"].is_object());
    assert!(input["operations"]["release_touch"]["arguments_schema"].is_object());
    assert!(input["operations"]["pulse_touch_while_running"]["arguments_schema"].is_object());
    assert!(input["operations"].get("tap").is_none());
    assert!(input["operations"].get("pulse_while_running").is_none());
    assert_eq!(input["input_buttons"], serde_json::json!(["a", "b"]));

    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let server = Emucap::new(shared);
    assert!(server
        .validate_routed_operation(&status, "debug", "probe", Some("revision-a"), true, true,)
        .is_ok());
    for revision in [None, Some("stale-revision")] {
        assert!(server
            .validate_routed_operation(&status, "debug", "probe", revision, true, true,)
            .is_err());
    }
}

#[tokio::test]
async fn step_keeps_the_existing_exact_frozen_wire_contract() {
    let concrete = Arc::new(Mutex::new(StepWireLink::new()));
    let shared: SharedLink = concrete.clone();
    let server = Emucap::new(shared);
    let result = server
        .step(Parameters(StepArgs {
            count: 2,
            unit: emucap::live::tools::StepUnit::Frames,
            cpu: None,
        }))
        .await;

    assert_ne!(result.is_error, Some(true));
    let link = concrete.lock().unwrap();
    assert_eq!(link.last_method.as_deref(), Some("step"));
    assert_eq!(link.last_params, Some(serde_json::json!({"frames": 2})));
}

#[tokio::test]
async fn debugger_cpu_and_mode_selection_reaches_the_adapter_wire() {
    let concrete = Arc::new(Mutex::new(StepWireLink::new()));
    let shared: SharedLink = concrete.clone();
    let server = Emucap::new(shared);

    let result = server
        .disassemble(Parameters(DisassembleArgs {
            address: Num(0x0200_0000),
            count: 4,
            output_path: None,
            cpu: Some("arm7".into()),
            mode: Some("thumb".into()),
        }))
        .await;
    assert_ne!(result.is_error, Some(true));
    {
        let link = concrete.lock().unwrap();
        assert_eq!(link.last_method.as_deref(), Some("disassemble"));
        assert_eq!(
            link.last_params,
            Some(serde_json::json!({
                "address": 0x0200_0000,
                "count": 4,
                "cpu": "arm7",
                "mode": "thumb"
            }))
        );
    }

    let result = server
        .call_stack(Parameters(CallStackArgs {
            cpu: Some("arm7".into()),
        }))
        .await;
    assert_ne!(result.is_error, Some(true));
    let link = concrete.lock().unwrap();
    assert_eq!(link.last_method.as_deref(), Some("call_stack"));
    assert_eq!(link.last_params, Some(serde_json::json!({"cpu": "arm7"})));
}

#[tokio::test]
async fn named_touch_operations_preserve_the_single_wire_contract() {
    let concrete = Arc::new(Mutex::new(InputWireLink::new()));
    let shared: SharedLink = concrete.clone();
    let server = Emucap::new(shared);
    let status = server.current_surface_status().expect("touch status");
    let revision = status["capability_revision"]
        .as_str()
        .expect("capability revision")
        .to_string();

    for (operation, arguments) in [
        ("hold_touch", serde_json::json!({"x": 10, "y": 20})),
        ("release_touch", serde_json::json!({})),
        (
            "pulse_touch_while_running",
            serde_json::json!({"x": 30, "y": 40, "frames": 5}),
        ),
    ] {
        let result = input_surface::execute(
            &server,
            RoutedOperationArgs {
                operation: operation.into(),
                arguments: Some(arguments.as_object().unwrap().clone()),
                known_capability_revision: Some(revision.clone()),
            },
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{operation}");
    }

    assert_eq!(
        concrete.lock().unwrap().calls,
        vec![
            (
                "touch".into(),
                serde_json::json!({"port": 0, "x": 10, "y": 20})
            ),
            (
                "touch".into(),
                serde_json::json!({"port": 0, "release": true})
            ),
            (
                "touch".into(),
                serde_json::json!({"port": 0, "x": 30, "y": 40, "frames": 5}),
            ),
        ]
    );
}

#[tokio::test]
async fn repeated_status_keeps_live_state_and_omits_unchanged_capabilities() {
    let shared: SharedLink = Arc::new(Mutex::new(FakeLink::ok(serde_json::json!({
        "connected": true,
        "execution": {"state": "frozen"},
        "execution_limits": {"frame": {"max_count": 60}}
    }))));
    let server = Emucap::new(shared);

    let first = server
        .status(Parameters(StatusArgs {
            known_capability_revision: None,
        }))
        .await;
    let first = first.structured_content.expect("full status JSON");
    let revision = first["capability_revision"]
        .as_str()
        .expect("capability revision")
        .to_string();
    assert_eq!(first["capability_snapshot"], "full");
    assert!(first["methods"].is_array());

    let repeated = server
        .status(Parameters(StatusArgs {
            known_capability_revision: Some(revision.clone()),
        }))
        .await;
    let repeated = repeated.structured_content.expect("compact status JSON");
    assert_eq!(repeated["capability_revision"], revision);
    assert_eq!(repeated["capability_snapshot"], "unchanged");
    assert_eq!(repeated["execution"]["state"], "frozen");
    assert!(repeated.get("continuity").is_some());
    assert!(repeated.get("methods").is_none());
    assert!(
        serde_json::to_vec(&repeated)
            .expect("serialize compact connected status")
            .len()
            <= 4 * 1024
    );
}

#[test]
fn stop_is_exposed_as_a_host_lifecycle_tool_with_required_generation_identity() {
    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let tools = Emucap::new(shared).tool_router.list_all();
    let stop = tools
        .iter()
        .find(|tool| tool.name.as_ref() == "stop")
        .expect("stop tool");
    let schema = serde_json::to_value(&stop.input_schema).unwrap();
    assert_eq!(schema["required"], serde_json::json!(["launch_id"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["launch_id"]["type"], "string");
}

#[test]
fn reattach_is_exposed_as_an_exact_generation_lifecycle_tool() {
    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let tools = Emucap::new(shared).tool_router.list_all();
    let reattach = tools
        .iter()
        .find(|tool| tool.name.as_ref() == "reattach")
        .expect("reattach tool");
    let schema = serde_json::to_value(&reattach.input_schema).unwrap();
    assert_eq!(schema["required"], serde_json::json!(["launch_id"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["launch_id"]["type"], "string");
}

#[test]
fn debugger_tools_expose_optional_cpu_and_instruction_mode_routing() {
    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let tools = Emucap::new(shared).tool_router.list_all();

    let disassemble = tools
        .iter()
        .find(|tool| tool.name.as_ref() == "disassemble")
        .expect("disassemble tool");
    let schema = serde_json::to_value(&disassemble.input_schema).unwrap();
    assert!(schema["properties"]["cpu"].is_object());
    assert!(schema["properties"]["mode"].is_object());

    let call_stack = tools
        .iter()
        .find(|tool| tool.name.as_ref() == "call_stack")
        .expect("call_stack tool");
    let schema = serde_json::to_value(&call_stack.input_schema).unwrap();
    assert!(schema["properties"]["cpu"].is_object());
}

#[test]
fn record_window_drawer_schema_is_generic_and_requires_an_explicit_evidence_root() {
    let description = debug_surface::describe(&serde_json::json!({
        "contracts": {"state": "validated"},
        "capability_revision": "revision-a",
        "methods": ["record_window"]
    }));
    let schema = &description["operations"]["record_window"]["arguments_schema"];
    assert_eq!(
        schema["required"],
        serde_json::json!(["output_root", "frames"])
    );
    let mut properties: Vec<_> = schema["properties"]
        .as_object()
        .expect("record_window properties")
        .keys()
        .map(String::as_str)
        .collect();
    properties.sort_unstable();
    assert_eq!(
        properties,
        [
            "event_arming_overrides",
            "event_classes",
            "event_filters",
            "frames",
            "initial_snapshots",
            "initial_state",
            "input_path",
            "limits",
            "origin",
            "output_root",
            "require_repeatable",
            "start_on",
            "stop_on",
            "terminal_snapshots",
            "terminal_state_profile",
            "warmup_frames"
        ]
    );
    let snapshot_items = &schema["properties"]["terminal_snapshots"]["items"];
    let definition = snapshot_items["$ref"]
        .as_str()
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        .expect("terminal snapshot definition reference");
    let snapshot = &schema["$defs"][definition];
    assert_eq!(snapshot["type"], "object");
    assert_eq!(
        snapshot["required"],
        serde_json::json!(["label", "memory_type", "address", "length"])
    );
}

#[test]
fn broker_probe_detects_a_listening_session_port() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(broker_session_accepting(
        &listener.local_addr().unwrap().to_string()
    ));
}

#[cfg(unix)]
#[test]
fn broker_helper_child_is_waited_by_reaper_thread() {
    let command = std::process::Command::new("true");
    spawn_reaped(command).join().unwrap();
}

#[test]
fn image_output_publishes_screenshot_provenance() {
    let result = tool_output_result(ToolOutput::Image {
        png_base64: "QUJD".into(),
        saved_path: Some("/tmp/shot.png".into()),
        provenance: serde_json::json!({
            "sha256": "abc",
            "byte_len": 3,
            "frame_before": 42,
            "frame_after": 42,
            "state": "frozen",
        }),
    });
    assert!(result
        .content
        .first()
        .and_then(|block| block.as_image())
        .is_some());
    let metadata = result.structured_content.as_ref().unwrap();
    assert_eq!(metadata["saved_path"], "/tmp/shot.png");
    assert_eq!(metadata["provenance"]["sha256"], "abc");
    assert_eq!(metadata["provenance"]["frame_before"], 42);
    assert_eq!(metadata["provenance"]["frame_after"], 42);
    assert_eq!(metadata["provenance"]["state"], "frozen");
    assert_eq!(body_text(&result), metadata.to_string());
}

// 한 도구가 lock을 쥔 채 panic해 뮤텍스가 poisoned돼도, link() 헬퍼가 복구해 서버가
// 죽지 않는지(다음 호출이 panic 안 함). poison이면 lock().unwrap()은 panic한다.
#[test]
fn link_helper_recovers_from_poison() {
    let shared: SharedLink = Arc::new(Mutex::new(tcp::lazy(
        "127.0.0.1:0",
        Duration::from_millis(50),
    )));
    let server = Emucap::new(shared.clone());
    let s2 = shared.clone();
    let _ = std::thread::spawn(move || {
        let _g = s2.lock().unwrap();
        panic!("의도적 poison");
    })
    .join();
    assert!(
        shared.is_poisoned(),
        "테스트 전제: 뮤텍스가 poison돼야 한다"
    );
    // 복구 — panic하면 테스트 실패.
    let _guard = server.link();
}

#[test]
fn verify_determinism_returns_result_without_ledger() {
    // 단일-writer: 제어 MCP는 원장에 쓰지 않고 결과만 반환한다(원장 바인딩·gate 기록 없음).
    let link: SharedLink = Arc::new(Mutex::new(
        DetReplayLink::new(&[
            "reset",
            "pause",
            "set_input",
            "step",
            "read_memory",
            "clear_all_breakpoints",
            "resume",
        ])
        .obs(&["aa", "aa"]),
    ));
    let srv = Emucap::new(link);
    let (_t, dir) = det_input_case();
    let args = VerifyDeterminismArgs {
        case_dir: dir.to_string_lossy().to_string(),
        observe: Some("memory".into()),
        memory_type: Some("w".into()),
        address: Some(Num(0)),
        length: Some(Num(1)),
        replays: Some(2),
    };
    let res = srv.verify_determinism_impl(args);
    assert_ne!(res.is_error, Some(true)); // success: is_error ≠ Some(true)
    let body = body_text(&res);
    assert!(body.contains("\"outcome\":\"reproducible\""), "{body}");
    assert!(body.contains("\"reproducible\":true"), "{body}");
    assert!(body.contains("\"passed\":true"), "{body}");
    // 원장 바인딩 흔적이 없어야(반환만): gate_logged/run_id 키 부재
    assert!(!body.contains("gate_logged"), "{body}");
    assert!(!body.contains("\"run_id\""), "{body}");
}

#[test]
fn verify_determinism_rejects_replays_below_two() {
    let link: SharedLink = Arc::new(Mutex::new(DetReplayLink::new(&["reset"])));
    let srv = Emucap::new(link);
    let (_t, dir) = det_input_case();
    let args = VerifyDeterminismArgs {
        case_dir: dir.to_string_lossy().to_string(),
        observe: None,
        memory_type: None,
        address: None,
        length: None,
        replays: Some(1),
    };
    let res = srv.verify_determinism_impl(args);
    assert_eq!(res.is_error, Some(true));
}

#[tokio::test]
async fn file_write_stages_bytes_before_calling_the_adapter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("payload.bin");
    std::fs::write(&path, [0xaa, 0xbb, 0xcc, 0xdd]).unwrap();

    let concrete = Arc::new(Mutex::new(FakeLink::ok(serde_json::json!({"written": 2}))));
    let shared: SharedLink = concrete.clone();
    let srv = Emucap::new(shared);
    let result = srv
        .write_memory(Parameters(WriteMemoryArgs {
            memory_type: "ram".into(),
            address: Num(0x20),
            hex: None,
            input_file: Some(WriteMemoryFileArgs {
                path: path.to_string_lossy().into_owned(),
                offset: Some(Num(1)),
                length: Num(2),
                sha256: None,
            }),
        }))
        .await;

    assert_ne!(result.is_error, Some(true));
    let body = body_text(&result);
    assert!(body.contains("\"input_kind\":\"file\""), "{body}");
    assert!(body.contains("\"input_bytes\":2"), "{body}");

    let link = concrete.lock().unwrap();
    assert_eq!(link.last_method.as_deref(), Some("write_memory"));
    assert_eq!(
        link.last_params,
        Some(serde_json::json!({
            "memory_type": "ram",
            "address": 0x20,
            "hex": "bbcc",
        }))
    );
    assert!(
        !link
            .last_params
            .as_ref()
            .unwrap()
            .to_string()
            .contains(path.to_string_lossy().as_ref()),
        "host path must not cross the adapter protocol boundary"
    );
}

#[tokio::test]
async fn file_write_validation_failure_has_no_adapter_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("payload.bin");
    std::fs::write(&path, [0xaa, 0xbb]).unwrap();

    let concrete = Arc::new(Mutex::new(FakeLink::ok(serde_json::json!({"written": 2}))));
    let shared: SharedLink = concrete.clone();
    let srv = Emucap::new(shared);
    let result = srv
        .write_memory(Parameters(WriteMemoryArgs {
            memory_type: "ram".into(),
            address: Num(0x20),
            hex: None,
            input_file: Some(WriteMemoryFileArgs {
                path: path.to_string_lossy().into_owned(),
                offset: None,
                length: Num(2),
                sha256: Some("0".repeat(64)),
            }),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert!(body_text(&result).contains("sha256 mismatch"));
    let link = concrete.lock().unwrap();
    assert_eq!(link.last_method, None);
    assert_eq!(link.last_params, None);
}
