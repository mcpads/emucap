use super::*;

use crate::args::{Num, VerifyDeterminismArgs};
use crate::regression::tests::{det_input_case, DetReplayLink};

/// CallToolResult의 텍스트 본문을 추출한다(검증용).
fn body_text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("")
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
