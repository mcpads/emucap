use super::*;

use crate::args::{Num, VerifyDeterminismArgs, WriteMemoryArgs, WriteMemoryFileArgs};
use crate::regression::tests::{det_input_case, DetReplayLink};
use emucap::live::link::FakeLink;

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
    assert!(stop
        .description
        .as_deref()
        .is_some_and(|description| description.contains("process-start identities")));
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
    let result = output_result(ToolOutput::Image {
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
    let text = body_text(&result);
    assert!(text.contains("saved: /tmp/shot.png"));
    assert!(text.contains("provenance:"));
    assert!(text.contains("\"sha256\":\"abc\""));
    assert!(text.contains("\"frame_before\":42"));
    assert!(text.contains("\"frame_after\":42"));
    assert!(text.contains("\"state\":\"frozen\""));
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
    let (_t, dir, _case) = det_input_case(None);
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
    let (_t, dir, _case) = det_input_case(None);
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
