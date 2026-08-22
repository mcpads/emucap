use super::*;
use std::sync::{Mutex as StdMutex, MutexGuard};
use tempfile::TempDir;

/// EMUCAP_TRACK_ROOT를 임시 디렉터리로 둔다. 환경변수는 프로세스 전역이라 직렬화 lock으로
/// 테스트 간 간섭을 막는다(반환한 guard가 살아있는 동안 단독 점유). guard와 TempDir를 함께
/// 돌려줘 .await을 지나도 유효하다.
fn temp_env() -> (TempDir, MutexGuard<'static, ()>) {
    static ENV_LOCK: StdMutex<()> = StdMutex::new(());
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TempDir::new().unwrap();
    std::env::set_var("EMUCAP_TRACK_ROOT", dir.path());
    (dir, guard)
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
fn server_info_identifies_the_tracking_binary() {
    let info = EmucapTrack::new().get_info();
    assert_eq!(info.server_info.name, "emucap-track-mcp");
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.instructions.as_deref(), Some(SERVER_INSTRUCTIONS));
}

#[test]
fn tracking_mcp_consumer_metadata_is_english() {
    let server = EmucapTrack::new();
    let instructions = server.get_info().instructions.unwrap_or_default();
    let tools = serde_json::to_string(&server.tool_router.list_all()).unwrap();
    assert!(!contains_hangul(&instructions), "{instructions}");
    assert!(!contains_hangul(&tools), "{tools}");
}

#[tokio::test]
async fn get_run_classifies_missing_and_unsafe_identifiers() {
    let (_dir, _guard) = temp_env();
    let server = EmucapTrack::new();

    let missing = server
        .get_run(Parameters(GetRunArgs {
            rom_sha1: "rom-safe".into(),
            run_id: "run-missing".into(),
        }))
        .await;
    assert_eq!(missing.is_error, Some(true));
    let body: serde_json::Value = serde_json::from_str(&body_text(&missing)).unwrap();
    assert_eq!(body["error"]["code"], "run_not_found");
    assert!(!body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("os error"));

    let invalid = server
        .get_run(Parameters(GetRunArgs {
            rom_sha1: "rom-safe".into(),
            run_id: "../outside".into(),
        }))
        .await;
    assert_eq!(invalid.is_error, Some(true));
    let body: serde_json::Value = serde_json::from_str(&body_text(&invalid)).unwrap();
    assert_eq!(body["error"]["code"], "invalid_identifier");
}

#[tokio::test]
async fn run_start_binds_active_and_log_metric_round_trips() {
    let (dir, _g) = temp_env();
    let root = dir.path();
    let s = EmucapTrack::new();
    // log_metric before run_start → 활성 run 없음 에러
    let r = s
        .log_metric(Parameters(LogMetricArgs {
            key: "k".into(),
            value: 1.0,
        }))
        .await;
    assert_eq!(r.is_error, Some(true));

    // run_start binds active
    let r = s
        .run_start(Parameters(TrackRunStartArgs {
            rom_sha1: "sha-a".into(),
            connection_ref: Some("port:1".into()),
            goal: Some("font".into()),
            description: None,
            tags: vec!["t".into()],
        }))
        .await;
    assert_ne!(r.is_error, Some(true));
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    let run_id = v["run_id"].as_str().unwrap().to_string();
    assert_eq!(v["rom_sha1"], "sha-a");
    // active 바인딩 확인
    assert_eq!(
        s.active_run.lock().unwrap().as_ref().unwrap().run_id,
        run_id
    );

    // now log_metric succeeds
    let r = s
        .log_metric(Parameters(LogMetricArgs {
            key: "frames".into(),
            value: 42.0,
        }))
        .await;
    assert_ne!(r.is_error, Some(true));

    // 디스크에 기록된 값을 확인
    let run = emucap::track::store::load_run(root, "sha-a", &run_id).unwrap();
    assert_eq!(run.status, emucap::track::model::RunStatus::Running);
    assert!(run
        .metrics
        .iter()
        .any(|m| m.key == "frames" && m.value == 42.0));
}

#[tokio::test]
async fn run_finish_clears_active() {
    let (dir, _g) = temp_env();
    let root = dir.path();
    let s = EmucapTrack::new();
    let r = s
        .run_start(Parameters(TrackRunStartArgs {
            rom_sha1: "sha-b".into(),
            connection_ref: None,
            goal: None,
            description: None,
            tags: vec![],
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    let run_id = v["run_id"].as_str().unwrap().to_string();

    let r = s
        .run_finish(Parameters(RunFinishArgs {
            status: Some("done".into()),
            run_id: None,
        }))
        .await;
    assert_ne!(r.is_error, Some(true));
    assert!(s.active_run.lock().unwrap().is_none());
    let run = emucap::track::store::load_run(root, "sha-b", &run_id).unwrap();
    assert_eq!(run.status, emucap::track::model::RunStatus::Done);
}

#[tokio::test]
async fn run_finish_by_id_recovers_orphan() {
    let (dir, _g) = temp_env();
    let root = dir.path();
    // 디스크에 직접 running run을 만들고(고아), 새 서버 인스턴스로 id 종료
    let now = emucap::track::clock::now_rfc3339();
    let run = emucap::track::ops::create_run(
        root,
        &emucap::track::id::UlidGen,
        &now,
        "sha-c",
        None,
        None,
        vec![],
        None,
    )
    .unwrap();
    let s = EmucapTrack::new(); // active 없음
    let r = s
        .run_finish(Parameters(RunFinishArgs {
            status: Some("aborted".into()),
            run_id: Some(run.id.clone()),
        }))
        .await;
    assert_ne!(r.is_error, Some(true));
    let loaded = emucap::track::store::load_run(root, "sha-c", &run.id).unwrap();
    assert_eq!(loaded.status, emucap::track::model::RunStatus::Aborted);
}

#[tokio::test]
async fn log_intervention_records_to_active_run() {
    let (dir, _g) = temp_env();
    let root = dir.path();
    let s = EmucapTrack::new();
    let r = s
        .run_start(Parameters(TrackRunStartArgs {
            rom_sha1: "sha-d".into(),
            connection_ref: None,
            goal: None,
            description: None,
            tags: vec![],
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    let run_id = v["run_id"].as_str().unwrap().to_string();

    let r = s
        .log_intervention(Parameters(LogInterventionArgs {
            op: "write_memory".into(),
            args: Some(serde_json::json!({"memory_type": "snesWorkRam", "address": 104})),
            at_frame: Some(7),
            at_event: None,
            frozen_context: true,
        }))
        .await;
    assert_ne!(r.is_error, Some(true));
    let run = emucap::track::store::load_run(root, "sha-d", &run_id).unwrap();
    assert_eq!(run.interventions.len(), 1);
    assert_eq!(run.interventions[0].op, "write_memory");
    assert_eq!(run.interventions[0].at_frame, Some(7));
}

#[tokio::test]
async fn bootstrap_reports_ledger_active_and_orphans() {
    let (dir, _g) = temp_env();
    let root = dir.path();
    // 고아 running run 하나
    let now = emucap::track::clock::now_rfc3339();
    emucap::track::ops::create_run(
        root,
        &emucap::track::id::UlidGen,
        &now,
        "sha-e",
        None,
        None,
        vec![],
        None,
    )
    .unwrap();
    let s = EmucapTrack::new();
    let r = s.bootstrap(Parameters(EmptyArgs::default())).await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    assert_eq!(v["server"], "emucap-track-mcp");
    assert_eq!(v["emulator_less"], true);
    assert_eq!(v["ledger_path"], root.display().to_string());
    assert_eq!(v["active_run"], serde_json::Value::Null);
    // 고아 running이 노출돼야 한다(복구 후보)
    assert_eq!(v["running_runs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn run_start_resumes_same_connection_and_rom_without_new_run() {
    let (dir, _g) = temp_env();
    let root = dir.path();
    // 세션 시작: run R1
    let s1 = EmucapTrack::new();
    let r = s1
        .run_start(Parameters(TrackRunStartArgs {
            rom_sha1: "sha-a".into(),
            connection_ref: Some("port:1".into()),
            goal: None,
            description: None,
            tags: vec![],
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    let r1 = v["run_id"].as_str().unwrap().to_string();
    assert!(v.get("resumed").is_none(), "첫 run_start은 resume 아님");

    // /mcp 재연결 흉내: in-memory active가 사라진 새 서버 인스턴스(같은 원장)
    let s2 = EmucapTrack::new();
    let r = s2
        .run_start(Parameters(TrackRunStartArgs {
            rom_sha1: "sha-a".into(),
            connection_ref: Some("port:1".into()),
            goal: None,
            description: None,
            tags: vec![],
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    // 같은 run을 resume — 새 run 만들지 않음
    assert_eq!(v["resumed"], true);
    assert_eq!(v["run_id"], r1);
    assert_eq!(s2.active_run.lock().unwrap().as_ref().unwrap().run_id, r1);
    // 디스크: run 1개뿐이고 여전히 running(파편화·supersede 없음)
    let runs = emucap::track::store::walk_runs(root).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, emucap::track::model::RunStatus::Running);

    // 이어쓰기 동작 확인: resume 후 log_metric 성공
    let r = s2
        .log_metric(Parameters(LogMetricArgs {
            key: "frames".into(),
            value: 9.0,
        }))
        .await;
    assert_ne!(r.is_error, Some(true));
}

#[tokio::test]
async fn run_start_supersedes_on_rom_mismatch_same_connection() {
    let (dir, _g) = temp_env();
    let root = dir.path();
    let s1 = EmucapTrack::new();
    let r = s1
        .run_start(Parameters(TrackRunStartArgs {
            rom_sha1: "sha-a".into(),
            connection_ref: Some("port:1".into()),
            goal: None,
            description: None,
            tags: vec![],
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    let r1 = v["run_id"].as_str().unwrap().to_string();

    // 같은 connection이지만 다른 rom → resume 아님(기존 supersede 경로 #56)
    let s2 = EmucapTrack::new();
    let r = s2
        .run_start(Parameters(TrackRunStartArgs {
            rom_sha1: "sha-b".into(),
            connection_ref: Some("port:1".into()),
            goal: None,
            description: None,
            tags: vec![],
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    let r2 = v["run_id"].as_str().unwrap().to_string();
    assert!(v.get("resumed").is_none(), "rom 다르면 resume 아님");
    assert_ne!(r1, r2);
    // R1은 superseded(aborted), R2는 running
    assert_eq!(
        emucap::track::store::load_run(root, "sha-a", &r1)
            .unwrap()
            .status,
        emucap::track::model::RunStatus::Aborted
    );
    assert_eq!(
        emucap::track::store::load_run(root, "sha-b", &r2)
            .unwrap()
            .status,
        emucap::track::model::RunStatus::Running
    );
}

#[tokio::test]
async fn run_resume_rebinds_running_run_and_rejects_finished() {
    let (dir, _g) = temp_env();
    let root = dir.path();
    let s1 = EmucapTrack::new();
    let r = s1
        .run_start(Parameters(TrackRunStartArgs {
            rom_sha1: "sha-a".into(),
            connection_ref: Some("port:1".into()),
            goal: None,
            description: None,
            tags: vec![],
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    let r1 = v["run_id"].as_str().unwrap().to_string();

    // 재연결: 새 서버, active 없음 → log_metric 에러
    let s2 = EmucapTrack::new();
    let r = s2
        .log_metric(Parameters(LogMetricArgs {
            key: "k".into(),
            value: 1.0,
        }))
        .await;
    assert_eq!(r.is_error, Some(true));

    // run_resume로 명시 재바인딩
    let r = s2
        .run_resume(Parameters(RunResumeArgs { run_id: r1.clone() }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    assert_eq!(v["resumed"], true);
    assert_eq!(v["run_id"], r1);
    // 이제 log_metric 성공(이어쓰기)
    let r = s2
        .log_metric(Parameters(LogMetricArgs {
            key: "frames".into(),
            value: 5.0,
        }))
        .await;
    assert_ne!(r.is_error, Some(true));
    let run = emucap::track::store::load_run(root, "sha-a", &r1).unwrap();
    assert!(run
        .metrics
        .iter()
        .any(|m| m.key == "frames" && m.value == 5.0));

    // 종료된 run은 resume 거부
    s2.run_finish(Parameters(RunFinishArgs {
        status: Some("done".into()),
        run_id: None,
    }))
    .await;
    let r = s2
        .run_resume(Parameters(RunResumeArgs { run_id: r1.clone() }))
        .await;
    assert_eq!(r.is_error, Some(true));
}

#[tokio::test]
async fn bootstrap_reports_ledger_path_source() {
    // temp_env는 EMUCAP_TRACK_ROOT를 설정하므로 source=env, 경고 없음
    let (_dir, _g) = temp_env();
    let s = EmucapTrack::new();
    let r = s.bootstrap(Parameters(EmptyArgs::default())).await;
    let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
    assert_eq!(v["ledger_path_source"], "env");
    assert!(v.get("ledger_path_warning").is_none());
}

#[tokio::test]
async fn log_finding_requires_rom_or_active() {
    let (_dir, _g) = temp_env();
    let s = EmucapTrack::new();
    // active도 rom_sha1도 없으면 에러
    let r = s
        .log_finding(Parameters(LogFindingArgs {
            rom_sha1: None,
            claim: "x".into(),
            evidence_refs: vec![],
            promoted: false,
        }))
        .await;
    assert_eq!(r.is_error, Some(true));
    // 명시 rom_sha1이면 active 없어도 기록
    let r = s
        .log_finding(Parameters(LogFindingArgs {
            rom_sha1: Some("sha-f".into()),
            claim: "promoted claim".into(),
            evidence_refs: vec![],
            promoted: true,
        }))
        .await;
    assert_ne!(r.is_error, Some(true));
}
