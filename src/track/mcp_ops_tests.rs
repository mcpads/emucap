//! mcp_ops 단위 테스트 — 임시 디렉터리 원장으로 round-trip(저장→읽기/질의→응답 Value 검증).
use std::cell::Cell;

use super::id::IdGen;
use super::model::RunStatus;
use super::query::RunFilter;
use super::summary::SummaryFilter;
use super::{mcp_ops, ops, store};
use tempfile::TempDir;

struct FixedGen(Cell<u32>);
impl IdGen for FixedGen {
    fn new_id(&self) -> String {
        let n = self.0.get();
        self.0.set(n + 1);
        format!("ID{n}")
    }
}
fn gen() -> FixedGen {
    FixedGen(Cell::new(0))
}

#[test]
fn parse_run_status_known_and_unknown() {
    assert_eq!(mcp_ops::parse_run_status("done").unwrap(), RunStatus::Done);
    assert_eq!(
        mcp_ops::parse_run_status("aborted").unwrap(),
        RunStatus::Aborted
    );
    assert_eq!(
        mcp_ops::parse_run_status("error").unwrap(),
        RunStatus::Error
    );
    assert!(mcp_ops::parse_run_status("nope").is_err());
}

#[test]
fn parse_gate_kind_known_and_unknown() {
    use super::model::GateKind;
    assert_eq!(
        mcp_ops::parse_gate_kind("machine").unwrap(),
        GateKind::Machine
    );
    assert_eq!(
        mcp_ops::parse_gate_kind("judgment").unwrap(),
        GateKind::Judgment
    );
    assert!(mcp_ops::parse_gate_kind("xyz").is_err());
}

#[test]
fn start_run_creates_run_and_returns_ids() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let v = mcp_ops::start_run(
        root,
        &gen(),
        "t0",
        "sha_a",
        Some("port:1".into()),
        Some("font".into()),
        Some("desc".into()),
        vec!["tag1".into()],
    )
    .unwrap();
    let run_id = v["run_id"].as_str().unwrap();
    assert_eq!(v["rom_sha1"], "sha_a");
    assert_eq!(v["ledger_path"], root.display().to_string());
    // 원장에 실제 run.json이 생겼는지 정본 확인
    let run = store::load_run(root, "sha_a", run_id).unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.goal.as_deref(), Some("font"));
    assert_eq!(run.connection_ref.as_deref(), Some("port:1"));
}

#[test]
fn start_run_aborts_same_connection_stale_running() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let g = gen(); // 단일 gen 공유 → run_id 충돌 없이 고유
                   // 같은 connection의 직전 running run
    let prev = ops::create_run(
        root,
        &g,
        "t0",
        "sha_a",
        None,
        None,
        vec![],
        Some("port:1".into()),
    )
    .unwrap();
    // 새 run 시작(같은 connection) → 직전이 aborted로 마감돼야 한다(#56)
    let v = mcp_ops::start_run(
        root,
        &g,
        "t1",
        "sha_a",
        Some("port:1".into()),
        None,
        None,
        vec![],
    )
    .unwrap();
    let new_id = v["run_id"].as_str().unwrap();
    assert_ne!(new_id, prev.id);
    assert_eq!(
        store::load_run(root, "sha_a", &prev.id).unwrap().status,
        RunStatus::Aborted
    );
    assert_eq!(
        store::load_run(root, "sha_a", new_id).unwrap().status,
        RunStatus::Running
    );
}

#[test]
fn find_resumable_run_matches_same_connection_and_rom() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = ops::create_run(
        root,
        &gen(),
        "t0",
        "sha_a",
        None,
        None,
        vec![],
        Some("port:1".into()),
    )
    .unwrap();
    // 같은 connection_ref + 같은 rom의 running → resume 대상
    let binding = mcp_ops::find_resumable_run(root, "port:1", "sha_a").unwrap();
    let binding = binding.expect("일치하는 running run을 찾아야 한다");
    assert_eq!(binding.run_id, run.id);
    assert_eq!(binding.rom_sha1, "sha_a");
    assert_eq!(binding.connection_ref.as_deref(), Some("port:1"));
}

#[test]
fn find_resumable_run_none_on_rom_mismatch() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // running이지만 rom이 다르면 resume 아님(→ 호출부가 supersede+새 run)
    ops::create_run(
        root,
        &gen(),
        "t0",
        "sha_a",
        None,
        None,
        vec![],
        Some("port:1".into()),
    )
    .unwrap();
    assert!(mcp_ops::find_resumable_run(root, "port:1", "sha_b")
        .unwrap()
        .is_none());
}

#[test]
fn find_resumable_run_none_on_connection_mismatch_or_finished() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let g = gen();
    // 다른 connection의 running
    ops::create_run(
        root,
        &g,
        "t0",
        "sha_a",
        None,
        None,
        vec![],
        Some("port:2".into()),
    )
    .unwrap();
    assert!(mcp_ops::find_resumable_run(root, "port:1", "sha_a")
        .unwrap()
        .is_none());
    // 같은 connection이지만 이미 종료(done)된 run은 resume 대상 아님
    let finished = ops::create_run(
        root,
        &g,
        "t1",
        "sha_a",
        None,
        None,
        vec![],
        Some("port:1".into()),
    )
    .unwrap();
    ops::finish_run(root, "sha_a", &finished.id, RunStatus::Done, "t2").unwrap();
    assert!(mcp_ops::find_resumable_run(root, "port:1", "sha_a")
        .unwrap()
        .is_none());
}

#[test]
fn resume_run_by_id_returns_binding_for_running_and_errors_otherwise() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let g = gen();
    let run = ops::create_run(
        root,
        &g,
        "t0",
        "sha_a",
        None,
        None,
        vec![],
        Some("port:1".into()),
    )
    .unwrap();
    // running → binding 반환
    let binding = mcp_ops::resume_run_by_id(root, &run.id).unwrap();
    assert_eq!(binding.run_id, run.id);
    assert_eq!(binding.rom_sha1, "sha_a");
    assert_eq!(binding.connection_ref.as_deref(), Some("port:1"));
    // 미존재 run_id → 에러
    assert!(mcp_ops::resume_run_by_id(root, "nope").is_err());
    // 종료된 run은 resume 불가 → 에러
    ops::finish_run(root, "sha_a", &run.id, RunStatus::Done, "t1").unwrap();
    assert!(mcp_ops::resume_run_by_id(root, &run.id).is_err());
}

#[test]
fn finish_active_run_sets_status() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = ops::create_run(root, &gen(), "t0", "sha_a", None, None, vec![], None).unwrap();
    let v = mcp_ops::finish_active_run(root, "sha_a", &run.id, RunStatus::Done, "t1").unwrap();
    assert_eq!(v["finished"], run.id);
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.status, RunStatus::Done);
    assert_eq!(back.ended_at.as_deref(), Some("t1"));
}

#[test]
fn finish_run_by_id_found_and_missing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = ops::create_run(root, &gen(), "t0", "sha_a", None, None, vec![], None).unwrap();
    let v = mcp_ops::finish_run_by_id(root, &run.id, RunStatus::Error, "t1").unwrap();
    assert_eq!(v["finished"], run.id);
    assert_eq!(
        store::load_run(root, "sha_a", &run.id).unwrap().status,
        RunStatus::Error
    );
    // 미존재 run_id
    assert!(mcp_ops::finish_run_by_id(root, "nope", RunStatus::Done, "t2").is_err());
}

#[test]
fn log_metric_appends() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = ops::create_run(root, &gen(), "t0", "sha_a", None, None, vec![], None).unwrap();
    let v = mcp_ops::log_metric(root, "sha_a", &run.id, &gen(), "t1", "fps", 60.0).unwrap();
    assert_eq!(v["ok"], true);
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.metrics.len(), 1);
    assert_eq!(back.metrics[0].key, "fps");
    assert_eq!(back.metrics[0].value, 60.0);
}

#[test]
fn log_gate_parses_kind_and_appends() {
    use super::model::GateKind;
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = ops::create_run(root, &gen(), "t0", "sha_a", None, None, vec![], None).unwrap();
    let v = mcp_ops::log_gate(
        root,
        "sha_a",
        &run.id,
        &gen(),
        "t1",
        "boot",
        "machine",
        Some(true),
        Some("ev".into()),
        Some("dt".into()),
        Some("case1".into()),
    )
    .unwrap();
    assert_eq!(v["ok"], true);
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.gates.len(), 1);
    assert_eq!(back.gates[0].name, "boot");
    assert_eq!(back.gates[0].kind, GateKind::Machine);
    assert_eq!(back.gates[0].passed, Some(true));
    // 잘못된 kind는 에러(원장 변경 없음)
    assert!(mcp_ops::log_gate(
        root,
        "sha_a",
        &run.id,
        &gen(),
        "t2",
        "x",
        "bogus",
        None,
        None,
        None,
        None
    )
    .is_err());
}

#[test]
fn log_artifact_resolves_relative_against_git_root() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join(".emucap");
    let repo = dir.path(); // git_root 격
    let run = ops::create_run(&root, &gen(), "t0", "sha_a", None, None, vec![], None).unwrap();
    // repo 루트 기준 상대경로 파일 생성
    std::fs::write(repo.join("shot.png"), b"PNGDATA").unwrap();
    let v = mcp_ops::log_artifact(
        &root,
        "sha_a",
        &run.id,
        &gen(),
        "screenshot",
        std::path::Path::new("shot.png"),
        Some(repo),
        None,
    )
    .unwrap();
    let aid = v["artifact_id"].as_str().unwrap();
    let back = store::load_run(&root, "sha_a", &run.id).unwrap();
    assert_eq!(back.artifacts.len(), 1);
    assert_eq!(back.artifacts[0].id, aid);
    assert_eq!(back.artifacts[0].kind, "screenshot");
    // repo 기준 상대로 저장(이식성)
    assert_eq!(back.artifacts[0].path, "shot.png");
}

#[test]
fn log_artifact_missing_path_is_honest_error() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = ops::create_run(root, &gen(), "t0", "sha_a", None, None, vec![], None).unwrap();
    let err = mcp_ops::log_artifact(
        root,
        "sha_a",
        &run.id,
        &gen(),
        "screenshot",
        std::path::Path::new("/nonexistent/shot.png"),
        None,
        None,
    )
    .unwrap_err();
    assert!(err.contains("아티팩트 경로 없음"));
    // 원장 미변경
    assert_eq!(
        store::load_run(root, "sha_a", &run.id)
            .unwrap()
            .artifacts
            .len(),
        0
    );
}

#[test]
fn set_reproduction_updates_base_and_movie() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = ops::create_run(root, &gen(), "t0", "sha_a", None, None, vec![], None).unwrap();
    let v = mcp_ops::set_reproduction(
        root,
        "sha_a",
        &run.id,
        Some("savestate".into()),
        Some("m.movie".into()),
    )
    .unwrap();
    assert_eq!(v["ok"], true);
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.repro_base.as_deref(), Some("savestate"));
    assert_eq!(back.repro_movie_ref.as_deref(), Some("m.movie"));
}

#[test]
fn log_finding_writes_finding() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let v = mcp_ops::log_finding(
        root,
        "sha_a",
        &gen(),
        "t0",
        "텍스트 엔진은 LZ77",
        Some("RUN1".into()),
        vec!["ev1".into()],
        true,
    )
    .unwrap();
    let fid = v["finding_id"].as_str().unwrap();
    let findings = store::walk_findings(root).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, fid);
    assert_eq!(findings[0].claim, "텍스트 엔진은 LZ77");
    assert_eq!(findings[0].rom_sha1, "sha_a");
    assert!(findings[0].promoted);
}

#[test]
fn query_runs_filters_and_lists() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let g = gen();
    let r1 = ops::create_run(
        root,
        &g,
        "t0",
        "sha_a",
        Some("font".into()),
        None,
        vec![],
        None,
    )
    .unwrap();
    let _r2 = ops::create_run(
        root,
        &g,
        "t1",
        "sha_b",
        Some("text".into()),
        None,
        vec![],
        None,
    )
    .unwrap();
    // 필터 없음 → 전부
    let all = mcp_ops::query_runs(root, RunFilter::default()).unwrap();
    assert_eq!(all["runs"].as_array().unwrap().len(), 2);
    assert_eq!(all["skipped"], 0);
    // rom_sha1 필터
    let only_a = mcp_ops::query_runs(
        root,
        RunFilter {
            rom_sha1: Some("sha_a".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let rows = only_a["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], r1.id);
    assert_eq!(rows[0]["goal"], "font");
}

#[test]
fn get_run_returns_detail_with_ledger_path() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = ops::create_run(
        root,
        &gen(),
        "t0",
        "sha_a",
        Some("font".into()),
        None,
        vec![],
        None,
    )
    .unwrap();
    let v = mcp_ops::get_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(v["id"], run.id);
    assert_eq!(v["rom_sha1"], "sha_a");
    assert_eq!(v["goal"], "font");
    assert_eq!(v["ledger_path"], root.display().to_string());
    // 미존재는 에러
    assert!(mcp_ops::get_run(root, "sha_a", "nope").is_err());
}

#[test]
fn compare_runs_diffs_two_runs() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let g = gen();
    let a = ops::create_run(root, &g, "t0", "sha_a", None, None, vec![], None).unwrap();
    let b = ops::create_run(root, &g, "t1", "sha_a", None, None, vec![], None).unwrap();
    ops::log_metric(root, "sha_a", &a.id, &g, "t2", "fps", 30.0).unwrap();
    ops::log_metric(root, "sha_a", &b.id, &g, "t3", "fps", 60.0).unwrap();
    let v = mcp_ops::compare_runs(root, &a.id, &b.id).unwrap();
    // 구조화 diff가 양쪽 run 메타를 담는다
    assert_eq!(v["a"]["run_id"], a.id);
    assert_eq!(v["b"]["run_id"], b.id);
    // 미존재 run은 에러
    assert!(mcp_ops::compare_runs(root, &a.id, "nope").is_err());
}

#[test]
fn summarize_runs_rolls_up() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let g = gen();
    ops::create_run(
        root,
        &g,
        "t0",
        "sha_a",
        Some("font".into()),
        None,
        vec![],
        None,
    )
    .unwrap();
    ops::create_run(
        root,
        &g,
        "t1",
        "sha_a",
        Some("font".into()),
        None,
        vec![],
        None,
    )
    .unwrap();
    let v = mcp_ops::summarize_runs(
        root,
        SummaryFilter {
            goal: Some("font".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(v["total"], 2);
    assert_eq!(v["skipped"], 0);
}
