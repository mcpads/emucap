use super::id::IdGen;
use super::model::*;
use super::{ops, store};
use tempfile::TempDir;

struct FixedGen(std::cell::Cell<u32>);
impl IdGen for FixedGen {
    fn new_id(&self) -> String {
        let n = self.0.get();
        self.0.set(n + 1);
        format!("ID{n}")
    }
}

#[test]
fn create_run_writes_rom_and_run() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(
        root,
        &gen,
        "2026-06-29T00:00:00Z",
        "sha_a",
        Some("font".into()),
        Some("first".into()),
        vec!["t1".into()],
        Some("g1".into()),
    )
    .unwrap();
    assert_eq!(run.rom_sha1, "sha_a");
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(
        run.repro_status,
        Some(ReproStatus::ReplayableWithInterventions)
    );
    // rom.json + run.json 정본 존재
    assert_eq!(store::load_rom(root, "sha_a").unwrap().sha1, "sha_a");
    assert_eq!(
        store::load_run(root, "sha_a", &run.id)
            .unwrap()
            .goal
            .as_deref(),
        Some("font")
    );
}

#[test]
fn finish_run_sets_status_and_ended_at() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t0", "sha_a", None, None, vec![], None).unwrap();
    ops::finish_run(root, "sha_a", &run.id, RunStatus::Done, "t1").unwrap();
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.status, RunStatus::Done);
    assert_eq!(back.ended_at.as_deref(), Some("t1"));
}

#[test]
fn finish_run_missing_errors() {
    let dir = TempDir::new().unwrap();
    assert!(ops::finish_run(dir.path(), "sha_a", "nope", RunStatus::Done, "t").is_err());
}

#[test]
fn finish_run_on_corrupt_json_is_not_reported_as_not_found() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // write a corrupt run.json at the expected path
    let p = root.join("roms/sha_a/runs/IDX");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("run.json"), b"{ not json").unwrap();
    let err = ops::finish_run(root, "sha_a", "IDX", RunStatus::Done, "t").unwrap_err();
    // corrupt file must NOT be masked as RunNotFound
    assert!(!matches!(err, ops::OpsError::RunNotFound { .. }));
}

#[test]
fn log_metric_appends_to_run() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t", "sha_a", None, None, vec![], None).unwrap();
    ops::log_metric(root, "sha_a", &run.id, &gen, "t2", "diff_bytes", 12.0).unwrap();
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.metrics.len(), 1);
    assert_eq!(back.metrics[0].key, "diff_bytes");
    assert_eq!(back.metrics[0].value, 12.0);
}

#[test]
fn log_gate_appends_with_kind_and_passed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t", "sha_a", None, None, vec![], None).unwrap();
    ops::log_gate(
        root,
        "sha_a",
        &run.id,
        &gen,
        "t2",
        "no-crash",
        GateKind::Machine,
        Some(true),
        None,
        None,
        None,
    )
    .unwrap();
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.gates.len(), 1);
    assert_eq!(back.gates[0].kind, GateKind::Machine);
    assert_eq!(back.gates[0].passed, Some(true));
}

#[test]
fn log_artifact_computes_sha256_and_relpath() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t", "sha_a", None, None, vec![], None).unwrap();
    // place file INSIDE the run directory so the relative-path branch is exercised
    let run_dir = store::run_dir(root, "sha_a", &run.id);
    let f = run_dir.join("shot.png");
    std::fs::write(&f, b"PNGDATA").unwrap();
    let id = ops::log_artifact(root, "sha_a", &run.id, &gen, "screenshot", &f, None).unwrap();
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.artifacts.len(), 1);
    assert_eq!(back.artifacts[0].id, id);
    // path stored as run-dir-relative
    assert_eq!(back.artifacts[0].path, "shot.png");
    // sha256 of b"PNGDATA"
    assert_eq!(
        back.artifacts[0].sha256,
        "2d4566582844690f8634a8b2534ea5221560038c6c0650c99140759bad603ae2"
    );
}

#[test]
fn log_artifact_missing_file_errors() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t", "sha_a", None, None, vec![], None).unwrap();
    assert!(ops::log_artifact(
        root,
        "sha_a",
        &run.id,
        &gen,
        "screenshot",
        std::path::Path::new("/no/such/file.png"),
        None
    )
    .is_err());
}

#[test]
fn log_finding_writes_finding_file() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let id = ops::log_finding(
        root,
        "sha_a",
        &gen,
        "t",
        "텍스트가 잘림",
        None,
        vec!["artifact:01ART".into()],
        true,
    )
    .unwrap();
    // findings/<id>.json 존재
    let p = root.join("findings").join(format!("{id}.json"));
    assert!(p.is_file());
}

#[test]
fn log_intervention_appends_and_rederives_status() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t", "sha_a", None, None, vec![], None).unwrap();
    // write_memory 개입 → replayable_with_interventions 유지
    ops::log_intervention(
        root,
        "sha_a",
        &run.id,
        &gen,
        "t2",
        None,
        None,
        false,
        "write_memory",
        serde_json::json!({"space":"wram","address":256,"bytes":"00"}),
    )
    .unwrap();
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.interventions.len(), 1);
    assert_eq!(back.interventions[0].seq, 0);
    assert_eq!(back.interventions[0].op, "write_memory");
    assert_eq!(
        back.repro_status,
        Some(ReproStatus::ReplayableWithInterventions)
    );
}

#[test]
fn log_intervention_load_state_downgrades_to_savestate_only() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t", "sha_a", None, None, vec![], None).unwrap();
    ops::log_intervention(
        root,
        "sha_a",
        &run.id,
        &gen,
        "t2",
        None,
        None,
        false,
        "load_state",
        serde_json::json!({"path":"x.mss"}),
    )
    .unwrap();
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.repro_status, Some(ReproStatus::SavestateOnly));
}

#[test]
fn log_intervention_assigns_sequential_seq() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t", "sha_a", None, None, vec![], None).unwrap();
    ops::log_intervention(
        root,
        "sha_a",
        &run.id,
        &gen,
        "t",
        None,
        None,
        false,
        "reset",
        serde_json::Value::Null,
    )
    .unwrap();
    ops::log_intervention(
        root,
        "sha_a",
        &run.id,
        &gen,
        "t",
        None,
        None,
        false,
        "write_memory",
        serde_json::json!({}),
    )
    .unwrap();
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(
        back.interventions.iter().map(|i| i.seq).collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn mark_savestate_only_forces_status() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(root, &gen, "t", "sha_a", None, None, vec![], None).unwrap();
    ops::mark_savestate_only(root, "sha_a", &run.id).unwrap();
    let back = store::load_run(root, "sha_a", &run.id).unwrap();
    assert_eq!(back.repro_status, Some(ReproStatus::SavestateOnly));
}

#[test]
fn create_run_rejects_path_like_rom_sha1() {
    // 경로 구분자가 섞이면 rom_sha1을 거부한다.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    // 절대경로·구분자·'..'·'.'·빈 문자열 등 단일 Normal 컴포넌트가 아닌 건 전부 거부(roms/ 탈출 방지).
    for bad in ["/abs/path/to/rom.md", "..", ".", "a/b", "", "../escape"] {
        let err = ops::create_run(
            root,
            &gen,
            "2026-06-30T00:00:00Z",
            bad,
            None,
            None,
            vec![],
            None,
        );
        assert!(
            matches!(
                err,
                Err(ops::OpsError::Track(store::TrackError::Invalid(_)))
            ),
            "안전하지 않은 rom_sha1 {bad:?}은 Invalid로 거부되어야: {err:?}"
        );
    }
    // 정상 sha1은 통과
    assert!(ops::create_run(
        root,
        &gen,
        "2026-06-30T00:00:00Z",
        "abc123def",
        None,
        None,
        vec![],
        None,
    )
    .is_ok());
}

#[test]
fn create_run_does_not_overwrite_rom_on_corruption() {
    // 손상 rom.json을 '없음'으로 오인해 first_seen을 덮어쓰지 않는다(NotFound만 생성 트리거).
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    // 정상 생성으로 rom.json을 만든 뒤 손상시킨다.
    ops::create_run(
        root,
        &gen,
        "2026-06-30T00:00:00Z",
        "sha_x",
        None,
        None,
        vec![],
        None,
    )
    .unwrap();
    let rom_json = root.join("roms").join("sha_x").join("rom.json");
    std::fs::write(&rom_json, b"{ not valid json").unwrap();
    // 손상 상태에서 다시 create_run → save_rom으로 조용히 덮지 말고 에러로 드러내야 한다.
    let err = ops::create_run(
        root,
        &gen,
        "2026-07-01T00:00:00Z",
        "sha_x",
        None,
        None,
        vec![],
        None,
    );
    assert!(
        err.is_err(),
        "손상 rom.json은 조용히 덮어쓰지 말고 에러여야: {err:?}"
    );
    // rom.json은 여전히 손상된 채(덮어쓰이지 않음)
    assert_eq!(std::fs::read(&rom_json).unwrap(), b"{ not valid json");
}

#[test]
fn finish_run_by_id_closes_orphan_without_active_state() {
    // #56: 서버 재시작으로 in-memory 활성 run이 사라져도 by-id로 고아 종료 가능.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let run = ops::create_run(
        root,
        &gen,
        "2026-06-30T00:00:00Z",
        "sha_a",
        None,
        None,
        vec![],
        None,
    )
    .unwrap();
    let closed =
        ops::finish_run_by_id(root, &run.id, RunStatus::Aborted, "2026-06-30T01:00:00Z").unwrap();
    assert_eq!(closed.as_deref(), Some(run.id.as_str()));
    assert_eq!(
        store::load_run(root, "sha_a", &run.id).unwrap().status,
        RunStatus::Aborted
    );
    // 미존재 id는 Ok(None)(조용한 실패 아님 — 호출부가 정직 에러로 처리)
    assert!(
        ops::finish_run_by_id(root, "NOPE", RunStatus::Done, "2026-06-30T02:00:00Z")
            .unwrap()
            .is_none()
    );
}

#[test]
fn finish_stale_running_closes_only_same_connection() {
    // #56: 새 run 시작 시 같은 connection의 고아 running만 정리 — 다른 연결은 안 건드림.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let gen = FixedGen(std::cell::Cell::new(0));
    let a1 = ops::create_run(
        root,
        &gen,
        "t",
        "sha_a",
        None,
        None,
        vec![],
        Some("connA".into()),
    )
    .unwrap();
    let b1 = ops::create_run(
        root,
        &gen,
        "t",
        "sha_b",
        None,
        None,
        vec![],
        Some("connB".into()),
    )
    .unwrap();
    let closed = ops::finish_stale_running(root, "connA", RunStatus::Aborted, "t2").unwrap();
    assert_eq!(closed, vec![a1.id.clone()]);
    assert_eq!(
        store::load_run(root, "sha_a", &a1.id).unwrap().status,
        RunStatus::Aborted
    );
    assert_eq!(
        store::load_run(root, "sha_b", &b1.id).unwrap().status,
        RunStatus::Running
    );
}
