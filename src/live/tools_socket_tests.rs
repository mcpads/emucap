use super::link::{Capabilities, EmulatorIdentity, EmulatorLink, LinkError};
use super::tools::{call_session_cleanup, hold_until, tap};
use crate::test_env::{lock_env, EnvGuard};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

struct SocketRuntimeEnv {
    _env: EnvGuard,
    _runtime_home: tempfile::TempDir,
    _guard: MutexGuard<'static, ()>,
}

impl SocketRuntimeEnv {
    fn new() -> Self {
        let guard = lock_env();
        let env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
        let runtime_home = tempfile::tempdir().unwrap();
        std::env::set_var("EMUCAP_EMU_HOME", runtime_home.path());
        Self {
            _env: env,
            _runtime_home: runtime_home,
            _guard: guard,
        }
    }
}

struct GenerationSwapLink {
    caps: Capabilities,
    prepared: bool,
    calls: Vec<String>,
}

impl EmulatorLink for GenerationSwapLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, method: &str, _params: Value) -> Result<Value, LinkError> {
        self.calls.push(method.to_string());
        match (method, self.prepared) {
            ("set_input", false) => Err(LinkError::NotConnected),
            ("status", true) => {
                self.caps.identity.launch_id = Some("replacement-generation".into());
                Ok(json!({"connected": true}))
            }
            ("set_input", true) => panic!("cleanup must not mutate a replacement generation"),
            other => panic!("unexpected call: {other:?}"),
        }
    }

    fn supports_session_reconnect(&self) -> bool {
        true
    }

    fn prepare_reconnect(&mut self) {
        self.prepared = true;
    }
}

#[test]
fn session_cleanup_refuses_a_replacement_launch_generation() {
    let mut link = GenerationSwapLink {
        caps: Capabilities {
            protocol_version: 1,
            methods: vec!["status".into(), "set_input".into()],
            memory_types: vec![],
            breakpoint_kinds: vec![],
            contracts: crate::contracts::ContractAdvertisement::Unreported,
            identity: EmulatorIdentity {
                launch_id: Some("original-generation".into()),
                ..EmulatorIdentity::default()
            },
        },
        prepared: false,
        calls: Vec::new(),
    };

    let error = call_session_cleanup(
        &mut link,
        "set_input",
        json!({"buttons": []}),
        Some("original-generation"),
    )
    .unwrap_err();

    assert!(
        matches!(error, LinkError::Emulator { ref kind, .. } if kind == "bad_state"),
        "generation replacement must fail closed: {error:?}"
    );
    assert_eq!(link.calls, ["set_input", "status"]);
}

#[test]
fn session_cleanup_preflights_before_mutating_an_unknown_connection() {
    let mut link = GenerationSwapLink {
        caps: Capabilities::empty(),
        prepared: false,
        calls: Vec::new(),
    };

    let error = call_session_cleanup(
        &mut link,
        "set_input",
        json!({"buttons": []}),
        Some("original-generation"),
    )
    .unwrap_err();

    assert!(
        matches!(error, LinkError::Emulator { ref kind, .. } if kind == "bad_state"),
        "an unknown replacement generation must fail closed: {error:?}"
    );
    assert_eq!(
        link.calls,
        ["status"],
        "generation status must precede every cleanup mutation"
    );
}

#[derive(Default)]
struct SocketProjectionState {
    calls: usize,
    frozen: bool,
    buttons: Vec<String>,
    projection: Vec<Vec<String>>,
}

fn run_reconnecting_projection_adapter(
    addr: String,
    disconnect_on: usize,
    state: Arc<Mutex<SocketProjectionState>>,
) {
    let mut injected = false;
    loop {
        let stream = TcpStream::connect(&addr).unwrap();
        stream.set_nodelay(true).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;

        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        let hello: Value = serde_json::from_str(hello.trim()).unwrap();
        let token = hello["params"]["session_token"].as_str().unwrap();
        writeln!(
            writer,
            "{}",
            json!({
                "id": hello["id"],
                "ok": true,
                "result": {
                    "protocol_version": 1,
                    "system": "test",
                    "adapter": "reconnecting-projection",
                    "session_token": token,
                    "launch_id": "test-generation",
                    "methods": [
                        "status", "pause", "set_input", "step", "read_memory", "_shutdown"
                    ],
                    "memory_types": ["test"],
                }
            })
        )
        .unwrap();

        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            let method = request["method"].as_str().unwrap();
            if method == "_shutdown" {
                writeln!(
                    writer,
                    "{}",
                    json!({"id": request["id"], "ok": true, "result": {"stopped": true}})
                )
                .unwrap();
                return;
            }

            let result = {
                let mut state = state.lock().unwrap();
                state.calls += 1;
                match method {
                    "pause" => {
                        state.frozen = true;
                        json!({"state": "frozen"})
                    }
                    "set_input" => {
                        state.buttons = request["params"]["buttons"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect();
                        json!({"buttons": state.buttons})
                    }
                    "step" => {
                        assert!(state.frozen);
                        for _ in 0..request["params"]["frames"].as_u64().unwrap() {
                            let buttons = state.buttons.clone();
                            state.projection.push(buttons);
                        }
                        json!({"state": "frozen"})
                    }
                    "read_memory" => json!({
                        "hex": if state.projection.is_empty() { "00" } else { "01" }
                    }),
                    "status" => json!({
                        "connected": true,
                        "state": if state.frozen { "frozen" } else { "running" },
                        "input_override": {
                            "engaged": !state.buttons.is_empty(),
                            "buttons": state.buttons,
                        }
                    }),
                    other => panic!("unexpected method: {other}"),
                }
            };

            let should_disconnect = {
                let state = state.lock().unwrap();
                !injected && state.calls == disconnect_on
            };
            if should_disconnect {
                injected = true;
                drop(writer);
                drop(reader);
                std::thread::sleep(Duration::from_millis(50));
                break;
            }
            writeln!(
                writer,
                "{}",
                json!({"id": request["id"], "ok": true, "result": result})
            )
            .unwrap();
        }
    }
}

#[test]
fn tap_reclaims_input_after_real_socket_disconnect_at_each_boundary() {
    let _runtime = SocketRuntimeEnv::new();
    let expected = [
        (1, false, vec![]),
        (2, false, vec![]),
        (3, false, vec![vec!["a".to_string()], vec!["a".to_string()]]),
        (
            4,
            true,
            vec![vec!["a".to_string()], vec!["a".to_string()], vec![]],
        ),
        (
            5,
            false,
            vec![vec!["a".to_string()], vec!["a".to_string()], vec![]],
        ),
    ];

    for (disconnect_on, completes, projection) in expected {
        let mut link = super::tcp::bind("127.0.0.1:0", Duration::from_millis(200)).unwrap();
        let addr = link.local_addr().to_string();
        let state = Arc::new(Mutex::new(SocketProjectionState::default()));
        let adapter_state = Arc::clone(&state);
        let adapter = std::thread::spawn(move || {
            run_reconnecting_projection_adapter(addr, disconnect_on, adapter_state)
        });

        let result = tap(&mut link, 0, &["a".into()], 2, 0);
        assert_eq!(
            result.is_ok(),
            completes,
            "unexpected terminal result after disconnect at request boundary {disconnect_on}: {result:?}"
        );
        let status =
            call_session_cleanup(&mut link, "status", json!({}), Some("test-generation")).unwrap();
        assert_eq!(status["state"], "frozen");
        assert_eq!(status["input_override"]["engaged"], false);

        {
            let state = state.lock().unwrap();
            assert!(state.frozen);
            assert!(state.buttons.is_empty());
            assert_eq!(
                state.projection, projection,
                "emulator projection drifted after disconnect at request boundary {disconnect_on}"
            );
        }
        link.call("_shutdown", json!({})).unwrap();
        adapter.join().unwrap();
    }
}

#[test]
fn hold_until_reclaims_input_after_real_socket_disconnect_at_each_boundary() {
    let _runtime = SocketRuntimeEnv::new();
    let expected = [
        (1, false, vec![]),
        (2, false, vec![]),
        (3, false, vec![]),
        (4, false, vec![vec!["down".to_string()]]),
        (5, false, vec![vec!["down".to_string()]]),
        (
            6,
            true,
            vec![vec!["down".to_string()], Vec::<String>::new()],
        ),
        (
            7,
            false,
            vec![vec!["down".to_string()], Vec::<String>::new()],
        ),
    ];

    for (disconnect_on, completes, projection) in expected {
        let mut link = super::tcp::bind("127.0.0.1:0", Duration::from_millis(200)).unwrap();
        let addr = link.local_addr().to_string();
        let state = Arc::new(Mutex::new(SocketProjectionState::default()));
        let adapter_state = Arc::clone(&state);
        let adapter = std::thread::spawn(move || {
            run_reconnecting_projection_adapter(addr, disconnect_on, adapter_state)
        });

        let result = hold_until(&mut link, 0, &["down".into()], "test", 0, 1, 1);
        assert_eq!(
            result.is_ok(),
            completes,
            "unexpected terminal result after disconnect at request boundary {disconnect_on}: {result:?}"
        );
        let status =
            call_session_cleanup(&mut link, "status", json!({}), Some("test-generation")).unwrap();
        assert_eq!(status["state"], "frozen");
        assert_eq!(status["input_override"]["engaged"], false);

        {
            let state = state.lock().unwrap();
            assert!(state.frozen);
            assert!(state.buttons.is_empty());
            assert_eq!(
                state.projection, projection,
                "emulator projection drifted after disconnect at request boundary {disconnect_on}"
            );
        }
        link.call("_shutdown", json!({})).unwrap();
        adapter.join().unwrap();
    }
}
