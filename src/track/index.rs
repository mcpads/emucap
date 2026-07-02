use std::path::Path;

use rusqlite::Connection;

use crate::track::store::{self, TrackError};

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("SQLite 오류: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Track(#[from] TrackError),
    #[error("직렬화 오류: {0}")]
    Json(#[from] serde_json::Error),
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS rom (
  sha1 TEXT PRIMARY KEY, platform TEXT NOT NULL, title TEXT, first_seen TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS run (
  id TEXT PRIMARY KEY, rom_sha1 TEXT NOT NULL, goal TEXT, description TEXT,
  status TEXT NOT NULL, started_at TEXT NOT NULL, ended_at TEXT, agent TEXT, session TEXT,
  connection_ref TEXT, repro_base TEXT, repro_movie_ref TEXT, repro_status TEXT);
CREATE TABLE IF NOT EXISTS gate (
  id TEXT PRIMARY KEY, run_id TEXT NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL,
  passed INTEGER, evidence_ref TEXT, detail TEXT, case_ref TEXT, created_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS metric (
  id TEXT PRIMARY KEY, run_id TEXT NOT NULL, key TEXT NOT NULL, value REAL NOT NULL,
  created_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS artifact (
  id TEXT PRIMARY KEY, run_id TEXT NOT NULL, kind TEXT NOT NULL,
  path TEXT NOT NULL,  -- run 디렉토리 기준 상대경로(run 디렉토리 밖 파일이면 절대경로, 참조)
  sha256 TEXT NOT NULL, meta TEXT);
CREATE TABLE IF NOT EXISTS intervention (
  id TEXT PRIMARY KEY, run_id TEXT NOT NULL, seq INTEGER NOT NULL, at_frame INTEGER,
  at_event TEXT, frozen_context INTEGER NOT NULL, op TEXT NOT NULL, args TEXT NOT NULL,
  created_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tag (run_id TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (run_id, tag));
CREATE TABLE IF NOT EXISTS finding (
  id TEXT PRIMARY KEY, rom_sha1 TEXT NOT NULL, run_id TEXT, claim TEXT NOT NULL,
  evidence_refs TEXT, promoted INTEGER NOT NULL, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_run_rom ON run(rom_sha1);
CREATE INDEX IF NOT EXISTS idx_run_goal ON run(rom_sha1, goal);
CREATE INDEX IF NOT EXISTS idx_gate_run ON gate(run_id);
CREATE INDEX IF NOT EXISTS idx_metric_rk ON metric(run_id, key);
CREATE INDEX IF NOT EXISTS idx_artifact_run ON artifact(run_id);
CREATE INDEX IF NOT EXISTS idx_interv_run ON intervention(run_id, seq);
";

/// 인덱스 연결을 열고(WAL) 스키마를 보장한다.
pub fn open_index(path: &Path) -> Result<Connection, IndexError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(TrackError::Io)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // 다중 세션이 같은 스토어를 동시에 ls/query하면 reindex 쓰기 트랜잭션이 겹친다.
    // busy_timeout으로 즉시 SQLITE_BUSY 실패 대신 잠시 재시도하게 한다.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// FS 정본을 walk해 인덱스를 재생성한다(전체·멱등). 인덱싱한 run 수를 반환.
///
/// DELETE-all + 재삽입 전체를 단일 트랜잭션으로 감싸 중단 시 부분 빈 상태가
/// 노출되지 않도록 한다(WAL + 단일-작성자 직렬화 요건).
pub fn reindex(root: &Path, conn: &Connection) -> Result<usize, IndexError> {
    let runs = store::walk_runs(root)?;
    let roms = store::walk_roms(root)?;
    let findings = store::walk_findings(root)?;
    reindex_from(conn, &roms, &runs, &findings)
}

/// reindex의 fast-read 변형: 손상/이질 JSON을 에러로 중단하지 않고 skip하되, skip한 경로를
/// 함께 반환한다(조용한 스킵 아님 — 호출자가 응답에 노출). 명시적 `reindex`는 strict 유지.
pub fn reindex_lenient(
    root: &Path,
    conn: &Connection,
) -> Result<(usize, Vec<std::path::PathBuf>), IndexError> {
    let (runs, s_runs) = store::walk_runs_lenient(root)?;
    let (roms, s_roms) = store::walk_roms_lenient(root)?;
    let (findings, s_find) = store::walk_findings_lenient(root)?;
    let mut skipped = s_runs;
    skipped.extend(s_roms);
    skipped.extend(s_find);
    let n = reindex_from(conn, &roms, &runs, &findings)?;
    Ok((n, skipped))
}

/// 메모리에 로드된 FS 정본으로 인덱스를 재생성한다(전체·멱등, 단일 트랜잭션).
fn reindex_from(
    conn: &Connection,
    roms: &[crate::track::model::Rom],
    runs: &[crate::track::model::Run],
    findings: &[crate::track::model::Finding],
) -> Result<usize, IndexError> {
    let tx = conn.unchecked_transaction()?;

    tx.execute_batch(
        "DELETE FROM rom; DELETE FROM run; DELETE FROM gate; DELETE FROM metric;
         DELETE FROM artifact; DELETE FROM intervention; DELETE FROM tag; DELETE FROM finding;",
    )?;

    for rom in roms {
        tx.execute(
            "INSERT INTO rom (sha1,platform,title,first_seen) VALUES (?1,?2,?3,?4)",
            rusqlite::params![rom.sha1, rom.platform, rom.title, rom.first_seen],
        )?;
    }
    for run in runs {
        let status = serde_json::to_value(&run.status)?;
        let repro_status = run
            .repro_status
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        tx.execute(
            "INSERT INTO run (id,rom_sha1,goal,description,status,started_at,ended_at,agent,
                 session,connection_ref,repro_base,repro_movie_ref,repro_status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                run.id,
                run.rom_sha1,
                run.goal,
                run.description,
                status.as_str(),
                run.started_at,
                run.ended_at,
                run.agent,
                run.session,
                run.connection_ref,
                run.repro_base,
                run.repro_movie_ref,
                repro_status.as_ref().and_then(|v| v.as_str())
            ],
        )?;
        for t in &run.tags {
            tx.execute(
                "INSERT OR IGNORE INTO tag (run_id,tag) VALUES (?1,?2)",
                rusqlite::params![run.id, t],
            )?;
        }
        for g in &run.gates {
            let kind = serde_json::to_value(g.kind)?;
            tx.execute(
                "INSERT INTO gate (id,run_id,name,kind,passed,evidence_ref,detail,case_ref,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![g.id, run.id, g.name, kind.as_str(),
                    g.passed.map(|b| b as i64), g.evidence_ref, g.detail, g.case_ref, g.created_at],
            )?;
        }
        for m in &run.metrics {
            tx.execute(
                "INSERT INTO metric (id,run_id,key,value,created_at) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![m.id, run.id, m.key, m.value, m.created_at],
            )?;
        }
        for a in &run.artifacts {
            let meta = a.meta.as_ref().map(|v| v.to_string());
            tx.execute(
                "INSERT INTO artifact (id,run_id,kind,path,sha256,meta) VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![a.id, run.id, a.kind, a.path, a.sha256, meta],
            )?;
        }
        for iv in &run.interventions {
            tx.execute(
                "INSERT INTO intervention (id,run_id,seq,at_frame,at_event,frozen_context,op,args,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![iv.id, run.id, iv.seq, iv.at_frame, iv.at_event,
                    iv.frozen_context as i64, iv.op, iv.args.to_string(), iv.created_at],
            )?;
        }
    }
    for f in findings {
        tx.execute(
            "INSERT INTO finding (id,rom_sha1,run_id,claim,evidence_refs,promoted,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                f.id,
                f.rom_sha1,
                f.run_id,
                f.claim,
                serde_json::to_string(&f.evidence_refs)?,
                f.promoted as i64,
                f.created_at
            ],
        )?;
    }
    tx.commit()?;
    Ok(runs.len())
}
