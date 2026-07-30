use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;

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

    let list = server.request(modern_request(2, "tools/list", json!({})));
    let result = &list["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["ttlMs"], STATIC_MCP_METADATA_TTL_MS);
    assert_eq!(result["cacheScope"], "public");
    let names: Vec<&str> = result["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert!(names.contains(&"bootstrap"));
    assert!(names.windows(2).all(|pair| pair[0] <= pair[1]));

    let call = server.request(modern_request(
        3,
        "tools/call",
        json!({"name": "bootstrap", "arguments": {}}),
    ));
    assert_eq!(call["result"]["resultType"], "complete");
    assert!(call["result"]["content"]
        .as_array()
        .is_some_and(|content| !content.is_empty()));
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
fn control_mcp_supports_modern_and_legacy_lifecycles() {
    let port = free_port().to_string();
    let envs = [("EMUCAP_PORT", port.clone())];
    assert_modern_server(env!("CARGO_BIN_EXE_emucap-mcp"), "emucap-mcp", &envs);
    assert_legacy_server(env!("CARGO_BIN_EXE_emucap-mcp"), &envs);
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
