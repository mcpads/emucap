use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use emucap::analysis::bisect::{CmpOp, Predicate};
use emucap::analysis::regression;
use emucap::live::runtime::{ManifestSpec, RuntimeStore};
use serde_json::{json, Value};

const MODERN_VERSION: &str = "2026-07-28";
const LEGACY_VERSION: &str = "2025-11-25";
const STATIC_MCP_METADATA_TTL_MS: u64 = 3_600_000;

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<String>,
    stdout_reader: Option<JoinHandle<()>>,
}

impl McpProcess {
    fn spawn(binary: &str, envs: &[(&str, String)]) -> Self {
        let mut command = Command::new(binary);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in envs {
            command.env(key, value);
        }

        let mut child = command.spawn().expect("spawn MCP server");
        let stdin = child.stdin.take().expect("MCP stdin");
        let stdout = child.stdout.take().expect("MCP stdout");
        let (sender, responses) = mpsc::channel();
        let stdout_reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            stdin: Some(stdin),
            responses,
            stdout_reader: Some(stdout_reader),
        }
    }

    fn notify(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("MCP stdin remains open");
        writeln!(stdin, "{message}").expect("write MCP request");
        stdin.flush().expect("flush MCP request");
    }

    fn request(&mut self, message: Value) -> Value {
        self.notify(message);
        let line = self
            .responses
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP response within 10 seconds");
        serde_json::from_str(&line).expect("valid JSON-RPC response")
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.stdout_reader.take() {
            let _ = reader.join();
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind temporary port")
        .local_addr()
        .expect("temporary port address")
        .port()
}

fn spawn_analysis_adapter(port: u16) -> (JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let observed_calls = calls.clone();
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => break stream,
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("connect fake analysis adapter: {error}"),
            }
        };
        let mut writer = stream.try_clone().expect("clone fake adapter stream");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        while reader.read_line(&mut line).expect("read control request") != 0 {
            let request: Value = serde_json::from_str(line.trim()).expect("control request JSON");
            line.clear();
            let method = request["method"].as_str().expect("control method");
            observed_calls.lock().unwrap().push(method.to_string());
            let result = match method {
                "hello" => {
                    let token = request["params"]["session_token"]
                        .as_str()
                        .expect("session token");
                    json!({
                        "protocol_version": 1,
                        "methods": [
                            "status", "reset", "pause", "set_input", "step",
                            "run_frames", "read_memory", "find_pattern",
                            "clear_all_breakpoints", "resume"
                        ],
                        "memory_types": ["w"],
                        "breakpoint_kinds": [],
                        "system": "test",
                        "adapter": "analysis-wire-test",
                        "build": "analysis-wire-test",
                        "launch_id": "launch-analysis-wire-test",
                        "session_token": token,
                        "contracts": {
                            "catalog": emucap::contracts::CATALOG_ID,
                            "active_exceptions": []
                        }
                    })
                }
                "status" => json!({
                    "connected": true,
                    "execution": {"state": "frozen"},
                    "execution_limits": {
                        "max_sync_advance_count": 5000,
                        "frame": {"max_count": 120}
                    }
                }),
                "read_memory" => json!({"hex": "aa"}),
                _ => json!({}),
            };
            let response = json!({
                "id": request["id"],
                "ok": true,
                "result": result
            });
            writeln!(writer, "{response}").expect("write control response");
            writer.flush().expect("flush control response");
        }
    });
    (handle, calls)
}

fn wait_for_connected_status(server: &mut McpProcess, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    let mut request_id = 4_000;
    loop {
        let response = server.request(modern_request(
            request_id,
            "tools/call",
            json!({"name": "status", "arguments": {}}),
        ));
        if response["result"]["structuredContent"]["connected"] == true {
            return response;
        }
        assert!(
            Instant::now() < deadline,
            "Control MCP did not observe the adapter connection within {timeout:?}: {response}"
        );
        request_id += 1;
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn make_analysis_case() -> (tempfile::TempDir, std::path::PathBuf) {
    let temporary = tempfile::tempdir().expect("temporary analysis case");
    let directory = temporary.path().join("wire-case");
    std::fs::create_dir_all(&directory).expect("create analysis case");
    std::fs::write(directory.join("inputs.movie"), "0:enter\n").expect("write movie");
    let case = regression::Case {
        format_version: regression::CASE_FORMAT_VERSION,
        id: "wire-case".into(),
        description: "wire analysis case".into(),
        rom: regression::RomRef {
            sha1: "unused".into(),
            path_hint: "wire.rom".into(),
        },
        repro: regression::Repro::InputReplay {
            start: "reset".into(),
            movie: "inputs.movie".into(),
            anchor: None,
        },
        predicate: Predicate {
            memory_type: "w".into(),
            address: 0,
            length: 1,
            op: CmpOp::Eq,
            value: 0,
        },
        expect: regression::Expect::Absent,
    };
    regression::save_case(&directory, &case).expect("save analysis case");
    (temporary, directory)
}

fn modern_meta(version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientInfo": {
            "name": "emucap-protocol-test",
            "version": "1"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn modern_request(id: u64, method: &str, params: Value) -> Value {
    let mut params = params.as_object().cloned().unwrap_or_default();
    params.insert("_meta".into(), modern_meta(MODERN_VERSION));
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    })
}

fn control_binary() -> String {
    std::env::var("EMUCAP_RELEASE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_emucap-mcp").to_string())
}

fn assert_modern_server(binary: &str, expected_name: &str, envs: &[(&str, String)]) {
    let mut server = McpProcess::spawn(binary, envs);
    let discover = server.request(modern_request(1, "server/discover", json!({})));
    let result = &discover["result"];
    assert_eq!(result["resultType"], "complete");
    assert!(result["supportedVersions"]
        .as_array()
        .is_some_and(|versions| versions.iter().any(|version| version == MODERN_VERSION)));
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        expected_name
    );
    assert_eq!(result["ttlMs"], STATIC_MCP_METADATA_TTL_MS);
    assert_eq!(result["cacheScope"], "public");
    if expected_name == "emucap-mcp" {
        assert!(result["capabilities"]["tools"]["listChanged"].is_null());
        assert!(
            result["instructions"]
                .as_str()
                .is_some_and(|instructions| instructions.len() <= 4 * 1024),
            "Control Server Instructions must stay within the always-loaded byte budget"
        );
    }

    let list = server.request(modern_request(2, "tools/list", json!({})));
    let result = &list["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["ttlMs"], STATIC_MCP_METADATA_TTL_MS);
    assert_eq!(result["cacheScope"], "public");
    let tools = result["tools"].as_array().expect("tool list");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert!(names.contains(&"bootstrap"));
    assert!(
        names.windows(2).all(|pair| pair[0] < pair[1]),
        "tool names must be unique and deterministically ordered"
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool["inputSchema"]["type"] == "object"
                && tool["inputSchema"]["additionalProperties"] == false),
        "every tool must expose a closed object input schema"
    );
    assert!(tools.iter().all(|tool| {
        let name = tool["name"].as_str().expect("tool name");
        (1..=128).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    }));
    if expected_name == "emucap-mcp" {
        for name in [
            "step",
            "change_media",
            "write_memory",
            "disassemble",
            "set_breakpoint",
            "clear_breakpoint",
            "list_breakpoints",
            "clear_all_breakpoints",
            "poll_events",
            "input_control",
            "debug",
            "analysis",
        ] {
            assert!(names.contains(&name), "missing front-panel route: {name}");
        }
        for name in [
            "run_frames",
            "wait_for_running_frames",
            "advance_and_freeze",
            "set_input",
            "record_window",
            "regression_run",
            "verify_determinism",
        ] {
            assert!(!names.contains(&name), "drawer operation leaked: {name}");
        }
        assert!(
            serde_json::to_vec(result)
                .expect("serialize tools/list result")
                .len()
                <= 36 * 1024,
            "Control tools/list must stay within the discovery byte budget"
        );
    }

    let missing_metadata = server.request(json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/list",
        "params": {}
    }));
    assert_eq!(
        missing_metadata["error"]["code"], -32602,
        "after modern discovery every request requires protocol metadata"
    );
    let repeated_list = server.request(modern_request(21, "tools/list", json!({})));
    assert_eq!(
        repeated_list["result"]["tools"], result["tools"],
        "the static tool list must not depend on prior calls"
    );

    let call = server.request(modern_request(
        3,
        "tools/call",
        json!({"name": "bootstrap", "arguments": {}}),
    ));
    assert_eq!(call["result"]["resultType"], "complete");
    assert!(call["result"]["content"]
        .as_array()
        .is_some_and(|content| !content.is_empty()));
    assert!(call["result"]["structuredContent"].is_object());
    let compatible_text = call["result"]["content"]
        .as_array()
        .and_then(|content| content.iter().find(|entry| entry["type"] == "text"))
        .and_then(|entry| entry["text"].as_str())
        .expect("structured tool results retain compatible JSON text");
    assert_eq!(
        serde_json::from_str::<Value>(compatible_text).expect("compatible text is JSON"),
        call["result"]["structuredContent"]
    );

    if expected_name == "emucap-mcp" {
        let bootstrap = &call["result"]["structuredContent"];
        assert!(bootstrap["listener"].is_object());
        assert!(bootstrap["listener"]["state"].is_string());
        assert!(bootstrap["listener"]["port"].is_number());
        assert!(bootstrap["adapter_connection"]["state"].is_string());
        assert!(bootstrap["entry"]["state"].is_string());
        assert!(bootstrap["entry"]["reason"].is_string());
        assert!(bootstrap["entry"]["primary_action"].is_object());
        assert!(bootstrap["supported_system_ids"].is_array());
        assert!(bootstrap["system_catalog_revision"].is_string());
        assert!(bootstrap.get("supported_systems").is_none());
        assert!(bootstrap.get("runtime_paths").is_none());
        assert!(bootstrap.get("status").is_none());
        assert!(bootstrap.get("workflow").is_none());
        assert!(bootstrap.get("do_not").is_none());
        assert!(bootstrap.get("ok").is_none());
        if let Ok(expected_build) = std::env::var("EMUCAP_EXPECT_SERVER_BUILD") {
            assert_eq!(
                bootstrap["server_build"], expected_build,
                "release-wire evaluation must use the committed build requested by the runner"
            );
        }
        assert!(
            serde_json::to_vec(bootstrap)
                .expect("serialize compact bootstrap")
                .len()
                <= 4 * 1024,
            "default bootstrap must stay compact"
        );

        let detailed = server.request(modern_request(
            10,
            "tools/call",
            json!({
                "name": "bootstrap",
                "arguments": {"include": ["systems", "installation"]}
            }),
        ));
        let detailed = &detailed["result"]["structuredContent"];
        assert!(detailed["supported_systems"].is_array());
        assert!(detailed["runtime_paths"].is_object());
        assert_eq!(detailed["entry"], bootstrap["entry"]);
        assert_eq!(detailed["listener"], bootstrap["listener"]);
        assert!(
            serde_json::to_vec(detailed)
                .expect("serialize detailed bootstrap")
                .len()
                <= 16 * 1024,
            "opt-in bootstrap details must remain bounded"
        );
    }

    let invalid = server.request(modern_request(
        4,
        "tools/call",
        json!({"name": "bootstrap", "arguments": {"bogus": true}}),
    ));
    assert_eq!(invalid["result"]["resultType"], "complete");
    assert_eq!(invalid["result"]["isError"], true);

    let after_invalid = server.request(modern_request(
        5,
        "tools/call",
        json!({"name": "bootstrap", "arguments": {}}),
    ));
    assert_eq!(after_invalid["result"]["resultType"], "complete");
    assert_ne!(after_invalid["result"]["isError"], true);

    if expected_name == "emucap-mcp" {
        let launch_failure = server.request(modern_request(
            6,
            "tools/call",
            json!({
                "name": "launch",
                "arguments": {
                    "content_path": "/definitely/missing/emucap-test.sfc",
                    "system": "snes"
                }
            }),
        ));
        assert_eq!(launch_failure["result"]["resultType"], "complete");
        assert_eq!(launch_failure["result"]["isError"], true);
        assert_eq!(
            launch_failure["result"]["structuredContent"]["launched"],
            false
        );

        let stop_failure = server.request(modern_request(
            7,
            "tools/call",
            json!({
                "name": "stop",
                "arguments": {"launch_id": "launch-does-not-exist"}
            }),
        ));
        assert_eq!(stop_failure["result"]["resultType"], "complete");
        assert_eq!(stop_failure["result"]["isError"], true);
        assert_eq!(
            stop_failure["result"]["structuredContent"]["stopped"],
            false
        );

        let status = server.request(modern_request(
            8,
            "tools/call",
            json!({"name": "status", "arguments": {}}),
        ));
        assert!(status["result"]["structuredContent"]
            .get("runtime_paths")
            .is_none());
        assert_eq!(
            status["result"]["structuredContent"]["task_entry"]["state"],
            call["result"]["structuredContent"]["entry"]["state"]
        );
        assert_eq!(
            status["result"]["structuredContent"]["task_entry"]["reason"],
            call["result"]["structuredContent"]["entry"]["reason"]
        );

        let plan = server.request(modern_request(
            9,
            "tools/call",
            json!({"name": "launch_plan", "arguments": {}}),
        ));
        let plan_body = &plan["result"]["structuredContent"];
        assert!(plan_body.get("bootstrap").is_none());
        assert!(plan_body.get("runtime_paths").is_none());
        assert_eq!(plan_body["next_action"]["kind"], "resolve_input");
        assert_eq!(
            plan_body["next_action"]["required_input"],
            serde_json::json!(["content_path"])
        );
        assert_eq!(plan_body["next_action"]["then_call"]["tool"], "launch_plan");
        assert_eq!(
            plan_body["next_action"]["then_call"]["arguments_from"],
            serde_json::json!(["content_path", "system?"])
        );
        assert!(
            serde_json::to_vec(plan_body)
                .expect("serialize launch plan")
                .len()
                <= 2 * 1024,
            "content-free launch_plan must stay compact"
        );
        assert!(plan_body.get("supported_systems").is_none());
        assert!(plan_body["supported_system_ids"].is_array());
        assert!(
            serde_json::to_vec(&status["result"]["structuredContent"])
                .expect("serialize disconnected status")
                .len()
                <= 4 * 1024,
            "disconnected status must stay within its response byte budget"
        );
    }
}

fn assert_legacy_server(binary: &str, envs: &[(&str, String)]) {
    let mut server = McpProcess::spawn(binary, envs);
    let initialize = server.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": LEGACY_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "emucap-protocol-test",
                "version": "1"
            }
        }
    }));
    assert_eq!(initialize["result"]["protocolVersion"], LEGACY_VERSION);
    assert!(initialize["result"].get("resultType").is_none());

    server.notify(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    }));
    let list = server.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    assert!(list["result"].get("resultType").is_none());
    assert!(list["result"]["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "bootstrap")));
}

#[test]
fn control_analysis_dispatcher_loads_schemas_and_executes_in_the_same_session() {
    let port = free_port();
    let envs = [("EMUCAP_PORT", port.to_string())];
    let binary = control_binary();
    let mut server = McpProcess::spawn(&binary, &envs);

    let discover = server.request(modern_request(1, "server/discover", json!({})));
    assert!(discover["result"]["capabilities"]["tools"]["listChanged"].is_null());

    let initial = server.request(modern_request(2, "tools/list", json!({})));
    let initial_size = serde_json::to_vec(&initial["result"])
        .expect("serialize initial Control tools/list")
        .len();
    assert!(
        initial_size <= 32 * 1024,
        "default Control tools/list exceeds the 32 KiB context ceiling: {initial_size} bytes"
    );
    let initial_names: Vec<&str> = initial["result"]["tools"]
        .as_array()
        .expect("initial tool list")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert!(initial_names.contains(&"analysis"));
    assert!(initial_names.contains(&"debug"));
    assert!(initial_names.contains(&"input_control"));
    assert!(initial_names.contains(&"step"));
    assert!(!initial_names.contains(&"regression_run"));
    assert!(!initial_names.contains(&"verify_determinism"));
    assert_eq!(initial["result"]["ttlMs"], STATIC_MCP_METADATA_TTL_MS);
    assert_eq!(initial["result"]["cacheScope"], "public");

    for (id, name) in [(30, "debug"), (31, "input_control")] {
        let described = server.request(modern_request(
            id,
            "tools/call",
            json!({
                "name": name,
                "arguments": {"operation": "describe"}
            }),
        ));
        assert_eq!(described["result"]["structuredContent"]["surface"], name);
        assert!(described["result"]["structuredContent"]["operations"].is_object());
        assert_eq!(
            described["result"]["structuredContent"]["next_action"]["tool"],
            "status"
        );
    }

    let described = server.request(modern_request(
        3,
        "tools/call",
        json!({
            "name": "analysis",
            "arguments": {"operation": "describe"}
        }),
    ));
    assert_eq!(
        described["result"]["structuredContent"]["surface"],
        "analysis"
    );
    assert!(
        described["result"]["structuredContent"]["operations"]["regression_run"]
            ["arguments_schema"]
            .is_object()
    );
    assert!(
        described["result"]["structuredContent"]["operations"]["verify_determinism"]
            ["arguments_schema"]
            .is_object()
    );

    let removed_route = server.request(modern_request(
        4,
        "tools/call",
        json!({
            "name": "verify_determinism",
            "arguments": {
                "case_dir": "/must/not/be/read",
                "replays": 1
            }
        }),
    ));
    assert!(removed_route["error"]["message"]
        .as_str()
        .is_some_and(|text| text.contains("not found")));

    let invalid = server.request(modern_request(
        6,
        "tools/call",
        json!({
            "name": "analysis",
            "arguments": {
                "operation": "verify_determinism",
                "arguments": {"bogus": true}
            }
        }),
    ));
    assert_eq!(invalid["result"]["isError"], true);
    assert!(invalid["result"]["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("invalid verify_determinism arguments")));

    let (_temporary, case_dir) = make_analysis_case();
    let (adapter, calls) = spawn_analysis_adapter(port);
    let connected = wait_for_connected_status(&mut server, Duration::from_secs(5));
    assert_eq!(connected["result"]["structuredContent"]["connected"], true);
    let described_debug = server.request(modern_request(
        41,
        "tools/call",
        json!({
            "name": "debug",
            "arguments": {"operation": "describe"}
        }),
    ));
    let debug_description = &described_debug["result"]["structuredContent"];
    assert!(debug_description["operations"]["find_pattern"].is_object());
    assert!(debug_description["operations"]
        .get("wait_for_running_frames")
        .is_none());
    let revision = debug_description["capability_revision"]
        .as_str()
        .expect("debug capability revision");
    let searched = server.request(modern_request(
        42,
        "tools/call",
        json!({
            "name": "debug",
            "arguments": {
                "operation": "find_pattern",
                "known_capability_revision": revision,
                "arguments": {"memory_type": "w", "hex": "aa"}
            }
        }),
    ));
    assert_ne!(searched["result"]["isError"], true);

    let verdict = server.request(modern_request(
        7,
        "tools/call",
        json!({
            "name": "analysis",
            "arguments": {
                "operation": "verify_determinism",
                "arguments": {
                    "case_dir": case_dir,
                    "observe": "memory",
                    "memory_type": "w",
                    "address": 0,
                    "length": 1,
                    "replays": 2
                }
            }
        }),
    ));
    assert_ne!(verdict["result"]["isError"], true);
    assert_eq!(
        verdict["result"]["structuredContent"]["outcome"],
        "reproducible"
    );
    assert_eq!(verdict["result"]["structuredContent"]["passed"], true);
    assert_eq!(
        verdict["result"]["structuredContent"]["hashes"]
            .as_array()
            .expect("observation hashes")
            .len(),
        2
    );

    drop(server);
    adapter.join().expect("join fake analysis adapter");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.last().map(String::as_str), Some("resume"));
    assert_eq!(
        calls
            .windows(3)
            .filter(|window| { window == &["set_input", "clear_all_breakpoints", "pause"] })
            .count(),
        2,
        "each replay must use the shared terminal cleanup path"
    );
}

#[test]
fn control_mcp_supports_modern_and_legacy_lifecycles() {
    let port = free_port().to_string();
    let envs = [("EMUCAP_PORT", port.clone())];
    let binary = control_binary();
    assert_modern_server(&binary, "emucap-mcp", &envs);
    assert_legacy_server(&binary, &envs);
}

#[test]
fn bootstrap_redacts_terminal_content_until_status_is_requested() {
    let root = tempfile::tempdir().expect("temporary emulator home");
    let port = free_port();
    let store = RuntimeStore::new(root.path().join("sessions"));
    let prepared = store.prepare(port).expect("prepare terminal generation");
    let secret_content = root.path().join("private/previous-game.sfc");
    let mut exited_process = Command::new("/usr/bin/true")
        .spawn()
        .expect("spawn short-lived process");
    let exited_pid = exited_process.id();
    exited_process.wait().expect("wait for short-lived process");
    let manifest = prepared.manifest(ManifestSpec {
        adapter: "mesen2".into(),
        system: "snes".into(),
        content: secret_content.to_string_lossy().into_owned(),
        emulator_pid: exited_pid,
        bridge_pid: None,
        backend_endpoint: None,
        build: Some("test-build".into()),
    });
    prepared
        .commit(&manifest)
        .expect("commit terminal generation");

    let envs = [
        ("EMUCAP_PORT", port.to_string()),
        (
            "EMUCAP_EMU_HOME",
            root.path().to_string_lossy().into_owned(),
        ),
    ];
    let mut server = McpProcess::spawn(env!("CARGO_BIN_EXE_emucap-mcp"), &envs);
    let bootstrap = server.request(modern_request(
        1,
        "tools/call",
        json!({"name": "bootstrap", "arguments": {}}),
    ));
    let body = &bootstrap["result"]["structuredContent"];
    assert_eq!(body["entry"]["state"], "ready_for_content");
    assert_eq!(body["entry"]["reason"], "terminal_history");
    assert_eq!(body["terminal_history_available"], true);
    assert!(
        !body
            .to_string()
            .contains(secret_content.to_string_lossy().as_ref()),
        "default bootstrap must not expose a prior content path"
    );

    let status = server.request(modern_request(
        2,
        "tools/call",
        json!({"name": "status", "arguments": {}}),
    ));
    let status = &status["result"]["structuredContent"];
    assert_eq!(status["task_entry"]["state"], body["entry"]["state"]);
    assert_eq!(status["task_entry"]["reason"], body["entry"]["reason"]);
    assert_eq!(
        status["runtime_instance"]["content"],
        secret_content.to_string_lossy().as_ref()
    );
}

#[test]
fn tracking_mcp_supports_modern_and_legacy_lifecycles() {
    let root = tempfile::tempdir().expect("temporary tracking root");
    let envs = [(
        "EMUCAP_TRACK_ROOT",
        root.path().to_string_lossy().into_owned(),
    )];
    assert_modern_server(
        env!("CARGO_BIN_EXE_emucap-track-mcp"),
        "emucap-track-mcp",
        &envs,
    );
    assert_legacy_server(env!("CARGO_BIN_EXE_emucap-track-mcp"), &envs);
}

#[test]
fn unsupported_modern_version_is_rejected_without_terminating_server() {
    let envs = [("EMUCAP_PORT", free_port().to_string())];
    let mut server = McpProcess::spawn(env!("CARGO_BIN_EXE_emucap-mcp"), &envs);
    let unsupported = server.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {
            "_meta": modern_meta("2099-01-01")
        }
    }));
    assert_eq!(unsupported["error"]["code"], -32022);

    let supported = server.request(modern_request(2, "server/discover", json!({})));
    assert_eq!(supported["result"]["resultType"], "complete");
}

#[test]
fn malformed_first_modern_request_is_rejected_without_terminating_servers() {
    let control_envs = [("EMUCAP_PORT", free_port().to_string())];
    let mut control = McpProcess::spawn(env!("CARGO_BIN_EXE_emucap-mcp"), &control_envs);
    let malformed = control.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {}
    }));
    assert_eq!(malformed["error"]["code"], -32600);
    let discover = control.request(modern_request(2, "server/discover", json!({})));
    assert_eq!(discover["result"]["resultType"], "complete");

    let root = tempfile::tempdir().expect("temporary tracking root");
    let track_envs = [(
        "EMUCAP_TRACK_ROOT",
        root.path().to_string_lossy().into_owned(),
    )];
    let mut track = McpProcess::spawn(env!("CARGO_BIN_EXE_emucap-track-mcp"), &track_envs);
    let malformed = track.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {}
    }));
    assert_eq!(malformed["error"]["code"], -32600);
    let discover = track.request(modern_request(2, "server/discover", json!({})));
    assert_eq!(discover["result"]["resultType"], "complete");
}
