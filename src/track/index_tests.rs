use super::model::*;
use super::{index, store};
use tempfile::TempDir;

fn run(id: &str, rom: &str, goal: &str) -> Run {
    Run {
        format_version: RUN_FORMAT_VERSION,
        id: id.into(),
        rom_sha1: rom.into(),
        goal: Some(goal.into()),
        description: None,
        tags: vec![],
        status: RunStatus::Done,
        started_at: "t".into(),
        ended_at: None,
        agent: None,
        session: None,
        connection_ref: None,
        repro_base: Some("reset".into()),
        repro_movie_ref: None,
        repro_status: Some(ReproStatus::ReplayableWithInterventions),
        gates: vec![],
        metrics: vec![],
        artifacts: vec![],
        interventions: vec![],
    }
}

#[test]
fn reindex_counts_runs() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_run(root, &run("01A", "sha_a", "g1")).unwrap();
    store::save_run(root, &run("01B", "sha_a", "g1")).unwrap();
    let conn = index::open_index(&root.join("index.sqlite")).unwrap();
    let n = index::reindex(root, &conn).unwrap();
    assert_eq!(n, 2);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn reindex_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_run(root, &run("01A", "sha_a", "g1")).unwrap();
    let conn = index::open_index(&root.join("index.sqlite")).unwrap();
    index::reindex(root, &conn).unwrap();
    index::reindex(root, &conn).unwrap(); // 두 번째도 안전
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn db_deletion_recovers_via_reindex() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_run(root, &run("01A", "sha_a", "g1")).unwrap();
    let dbp = root.join("index.sqlite");
    {
        let conn = index::open_index(&dbp).unwrap();
        index::reindex(root, &conn).unwrap();
    }
    std::fs::remove_file(&dbp).unwrap(); // DB 손실
    let conn = index::open_index(&dbp).unwrap();
    let n = index::reindex(root, &conn).unwrap();
    assert_eq!(n, 1); // FS 정본에서 완전 복원
}

#[test]
fn intervention_rows_indexed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let mut r = run("01A", "sha_a", "g1");
    r.interventions.push(Intervention {
        id: "iv1".into(),
        seq: 0,
        at_frame: Some(5),
        at_event: None,
        frozen_context: true,
        op: "write_memory".into(),
        args: serde_json::json!({"address":1}),
        created_at: "t".into(),
    });
    store::save_run(root, &r).unwrap();
    let conn = index::open_index(&root.join("index.sqlite")).unwrap();
    index::reindex(root, &conn).unwrap();
    let c: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM intervention WHERE op='write_memory'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(c, 1);
}

#[test]
fn reindex_strict_errors_on_corrupt_run() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_run(root, &run("01A", "sha_a", "g1")).unwrap();
    let p = root.join("roms/sha_a/runs/01B");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("run.json"), b"{ broken").unwrap();
    let conn = index::open_index(&root.join("index.sqlite")).unwrap();
    // 명시적 reindex는 strict — 손상에 에러(무결성 검사)
    assert!(index::reindex(root, &conn).is_err());
}

#[test]
fn reindex_lenient_skips_corrupt_and_indexes_rest() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_run(root, &run("01A", "sha_a", "g1")).unwrap();
    store::save_run(root, &run("01B", "sha_a", "g1")).unwrap();
    // 손상 run + 이질 finding 하나씩
    let p = root.join("roms/sha_a/runs/01C");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("run.json"), b"{ broken").unwrap();
    std::fs::create_dir_all(root.join("findings")).unwrap();
    std::fs::write(root.join("findings/notes.json"), b"{ not finding").unwrap();
    let conn = index::open_index(&root.join("index.sqlite")).unwrap();
    // lenient는 유효 2개 인덱싱 + skipped 2개 노출(query/ls가 안 죽음)
    let (n, skipped) = index::reindex_lenient(root, &conn).unwrap();
    assert_eq!(n, 2);
    assert_eq!(skipped.len(), 2);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}
