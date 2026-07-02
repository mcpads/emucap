//! 추적 MCP 도구의 *순수 로직* 어댑터 — rmcp·에뮬레이터·active_run 상태와 무관하다.
//!
//! 각 함수는 이미 해소된 값(rom_sha1·run_id·now·connection_ref·git_root 등)을 인자로 받아,
//! `ops`/`store`/`compare`/`summary`/`index`/`query` 위에 얇은 어댑터를 얹고 응답 JSON(`Value`)을
//! 조립한다. 에러는 `String`으로 평탄화한다(rmcp 타입 비의존). 호출부는 추적 MCP(`emucap-track-mcp`)
//! 하나다 — lib로 추출한 목적은 로직을 rmcp·에뮬·바이너리 상태와 떼어 단위 테스트가 가능하게 하는 것
//! (제어 MCP는 분리 후 추적 도구가 없어 호출하지 않는다). active_run(Mutex)·rom_sha1 해소(get_rom_info
//! 추론) 같은 *바이너리 상태*는 여기 없다 — 호출부가 들고 인자로 넘긴다.

use std::path::Path;

use serde_json::Value;

use crate::track::compare;
use crate::track::id::IdGen;
use crate::track::index;
use crate::track::model::{GateKind, RunStatus};
use crate::track::ops;
use crate::track::query::{self, RunFilter};
use crate::track::store::{self, resolve_artifact_path};
use crate::track::summary::{self, SummaryFilter};

/// run_finish의 status 문자열을 RunStatus로 파싱한다(done|aborted|error).
pub fn parse_run_status(s: &str) -> Result<RunStatus, String> {
    match s {
        "done" => Ok(RunStatus::Done),
        "aborted" => Ok(RunStatus::Aborted),
        "error" => Ok(RunStatus::Error),
        other => Err(format!("알 수 없는 status: {other}")),
    }
}

/// log_gate의 kind 문자열을 GateKind로 파싱한다(machine|judgment).
pub fn parse_gate_kind(s: &str) -> Result<GateKind, String> {
    match s {
        "machine" => Ok(GateKind::Machine),
        "judgment" => Ok(GateKind::Judgment),
        other => Err(format!("알 수 없는 gate kind: {other}")),
    }
}

/// 새 Run을 시작한다: 같은 connection의 고아 running run을 자동 마감(#56)한 뒤 create_run.
/// in-memory active_run의 마감은 호출부(바이너리) 책임 — 여기선 connection_ref 기반 디스크 위생만.
/// 반환 `{run_id, rom_sha1, ledger_path}`. 호출부는 이 run_id로 active_run을 갱신한다.
#[allow(clippy::too_many_arguments)]
pub fn start_run(
    root: &Path,
    gen: &dyn IdGen,
    now: &str,
    rom_sha1: &str,
    connection_ref: Option<String>,
    goal: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
) -> Result<Value, String> {
    // 원장 위생(#56): 같은 connection의 디스크 고아 running을 aborted(superseded)로. best-effort —
    // 실패해도 새 run 진행(서버 재시작으로 in-memory가 사라져도 같은-connection 고아를 정리).
    if let Some(cref) = connection_ref.as_deref() {
        let _ = ops::finish_stale_running(root, cref, RunStatus::Aborted, now);
    }
    let run = ops::create_run(
        root,
        gen,
        now,
        rom_sha1,
        goal,
        description,
        tags,
        connection_ref,
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "run_id": run.id,
        "rom_sha1": run.rom_sha1,
        "ledger_path": root.display().to_string(),
    }))
}

/// active run 재바인딩(resume)에 필요한 최소 정보. 호출부(바이너리)가 이 값으로 in-memory active를
/// 복원한다 — resume은 supersede+새 run이 아니라 *디스크의 still-running run을 다시 active로 잡는 것*이다.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeBinding {
    pub run_id: String,
    pub rom_sha1: String,
    pub connection_ref: Option<String>,
}

/// 재연결 복원용: connection_ref + rom_sha1이 모두 일치하는 still-running run을 찾는다(없으면 None).
/// /mcp 재연결로 in-memory active가 사라졌을 때, run_start가 supersede+새 run 대신 이 run을 resume하게
/// 한다(파편화 0). 같은 connection의 running이 여럿이면(정상 불변식 위반이나 방어적으로) started_at 최신을
/// 고른다. 손상 run.json은 건너뛴다(lenient — 무관한 손상이 resume을 막지 않게). rom이 다르면 일치하지
/// 않아 None → 호출부는 기존 supersede 경로(finish_stale_running)로 직전 run을 마감하고 새 run을 만든다.
pub fn find_resumable_run(
    root: &Path,
    connection_ref: &str,
    rom_sha1: &str,
) -> Result<Option<ResumeBinding>, String> {
    let (runs, _skipped) = store::walk_runs_lenient(root).map_err(|e| e.to_string())?;
    let best = runs
        .into_iter()
        .filter(|r| {
            r.status == RunStatus::Running
                && r.connection_ref.as_deref() == Some(connection_ref)
                && r.rom_sha1 == rom_sha1
        })
        .max_by(|a, b| a.started_at.cmp(&b.started_at));
    Ok(best.map(|r| ResumeBinding {
        run_id: r.id,
        rom_sha1: r.rom_sha1,
        connection_ref: r.connection_ref,
    }))
}

/// 명시 재바인딩용: run_id(전역 유일)로 running run을 찾아 resume binding을 만든다.
/// 미존재면 Err, status가 running이 아니면 Err(이미 종료된 run은 resume 불가 — 새 run_start로).
/// 디스크가 정본이라 in-memory active 상태와 무관하게 동작한다(서버 재시작/재연결 복구).
pub fn resume_run_by_id(root: &Path, run_id: &str) -> Result<ResumeBinding, String> {
    let run = store::find_run_by_id(root, run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("run_id 없음: {run_id}"))?;
    if run.status != RunStatus::Running {
        return Err(format!(
            "run {run_id}의 status가 {:?}라 resume 불가 — running run만 재바인딩할 수 있다(종료된 run은 새 run_start로 시작).",
            run.status
        ));
    }
    Ok(ResumeBinding {
        run_id: run.id,
        rom_sha1: run.rom_sha1,
        connection_ref: run.connection_ref,
    })
}

/// run_id(전역 유일)로 run을 직접 종료한다(서버 재시작 등 고아 복구). 미존재면 Err.
/// 반환 `{finished: <id>}`. 호출부는 이 id가 active와 같으면 active를 비운다.
pub fn finish_run_by_id(
    root: &Path,
    run_id: &str,
    status: RunStatus,
    now: &str,
) -> Result<Value, String> {
    match ops::finish_run_by_id(root, run_id, status, now).map_err(|e| e.to_string())? {
        Some(id) => Ok(serde_json::json!({ "finished": id })),
        None => Err(format!("run_id 없음: {run_id}")),
    }
}

/// active run(rom_sha1/run_id 해소됨)을 종료한다. 반환 `{finished: <run_id>}`.
/// active_run 상태 비우기는 호출부 책임.
pub fn finish_active_run(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    status: RunStatus,
    now: &str,
) -> Result<Value, String> {
    ops::finish_run(root, rom_sha1, run_id, status, now).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "finished": run_id }))
}

/// 활성 run에 메트릭 1건 기록. 반환 `{ok:true}`.
#[allow(clippy::too_many_arguments)]
pub fn log_metric(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    gen: &dyn IdGen,
    now: &str,
    key: &str,
    value: f64,
) -> Result<Value, String> {
    ops::log_metric(root, rom_sha1, run_id, gen, now, key, value).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "ok": true }))
}

/// 활성 run에 게이트 1건 기록(kind 문자열 파싱 포함). 반환 `{ok:true}`.
#[allow(clippy::too_many_arguments)]
pub fn log_gate(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    gen: &dyn IdGen,
    now: &str,
    name: &str,
    kind: &str,
    passed: Option<bool>,
    evidence_ref: Option<String>,
    detail: Option<String>,
    case_ref: Option<String>,
) -> Result<Value, String> {
    let kind = parse_gate_kind(kind)?;
    ops::log_gate(
        root,
        rom_sha1,
        run_id,
        gen,
        now,
        name,
        kind,
        passed,
        evidence_ref,
        detail,
        case_ref,
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "ok": true }))
}

/// 이미 캡처된 파일을 artifact로 등록한다 — 경로해소(절대=그대로·상대=git_root 기준) + 존재검사
/// + 정직 에러까지 여기서 한다. 반환 `{artifact_id: <id>}`.
#[allow(clippy::too_many_arguments)]
pub fn log_artifact(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    gen: &dyn IdGen,
    kind: &str,
    raw_path: &Path,
    git_root: Option<&Path>,
    meta: Option<Value>,
) -> Result<Value, String> {
    // 상대경로는 MCP 서버 cwd가 아니라 *작업 repo* 루트 기준으로 해소(최소놀람·재현성).
    let resolved = resolve_artifact_path(raw_path, git_root);
    if !resolved.exists() {
        let base = git_root
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "현재 작업 디렉터리".into());
        return Err(format!(
            "아티팩트 경로 없음: {} — 상대경로는 repo root({}) 기준으로 해소된다. 절대경로를 넘기거나 repo root 기준 경로를 써라.",
            resolved.display(),
            base,
        ));
    }
    let id = ops::log_artifact(root, rom_sha1, run_id, gen, kind, &resolved, meta)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "artifact_id": id }))
}

/// 활성 run에 개입(intervention) 1건을 명시 기록한다(lineage). 추적 MCP의 공개 도구용 —
/// 제어 MCP가 더는 자동 기록하지 않으므로(제어→추적 의존 금지) 에이전트가
/// write_memory/load_state/reset/입력 등 상태변경을 직접 기록해 repro_status 충실도를 유지한다.
/// op/args/at_frame/at_event/frozen_context는 ops::log_intervention 시그니처를 그대로 받는다.
/// 반환 `{ok:true}`.
#[allow(clippy::too_many_arguments)]
pub fn log_intervention(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    gen: &dyn IdGen,
    now: &str,
    at_frame: Option<u64>,
    at_event: Option<String>,
    frozen_context: bool,
    op: &str,
    args: Value,
) -> Result<Value, String> {
    ops::log_intervention(
        root,
        rom_sha1,
        run_id,
        gen,
        now,
        at_frame,
        at_event,
        frozen_context,
        op,
        args,
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "ok": true }))
}

/// 활성 run의 재현 base/movie를 설정한다(repro_status 자동 도출). 반환 `{ok:true}`.
pub fn set_reproduction(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    base: Option<String>,
    movie_ref: Option<String>,
) -> Result<Value, String> {
    ops::set_reproduction(root, rom_sha1, run_id, base, movie_ref).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "ok": true }))
}

/// 발견을 ROM 스코프로 기록한다. 반환 `{finding_id: <id>}`.
/// rom_sha1·run_id 해소(arg 우선·active 폴백)는 호출부 책임 — 여기선 해소된 값을 받는다.
#[allow(clippy::too_many_arguments)]
pub fn log_finding(
    root: &Path,
    rom_sha1: &str,
    gen: &dyn IdGen,
    now: &str,
    claim: &str,
    run_id: Option<String>,
    evidence_refs: Vec<String>,
    promoted: bool,
) -> Result<Value, String> {
    let id = ops::log_finding(
        root,
        rom_sha1,
        gen,
        now,
        claim,
        run_id,
        evidence_refs,
        promoted,
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "finding_id": id }))
}

/// 추적 원장에서 run을 질의한다(lenient reindex 후 SQLite 질의). 반환 `{runs:[...], skipped:n}`.
pub fn query_runs(root: &Path, filter: RunFilter) -> Result<Value, String> {
    let conn = index::open_index(&root.join("index.sqlite")).map_err(|e| e.to_string())?;
    // fast-read는 lenient reindex: 손상/이질 JSON 하나가 query 전체를 죽이지 않게 skip하되 노출한다.
    let (_, skipped) = index::reindex_lenient(root, &conn).map_err(|e| e.to_string())?;
    let rows = query::query_runs(&conn, &filter).map_err(|e| e.to_string())?;
    let arr: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "rom_sha1": r.rom_sha1,
                "goal": r.goal,
                "status": r.status,
                "repro_status": r.repro_status,
            })
        })
        .collect();
    Ok(serde_json::json!({ "runs": arr, "skipped": skipped.len() }))
}

/// Run 상세(run.json 정본 + ledger_path)를 반환한다.
pub fn get_run(root: &Path, rom_sha1: &str, run_id: &str) -> Result<Value, String> {
    let run = store::load_run(root, rom_sha1, run_id).map_err(|e| e.to_string())?;
    let mut v = serde_json::to_value(&run).map_err(|e| e.to_string())?;
    // ledger_path 노출 — 기록이 실제로 어디(어느 repo) 사는지 에이전트가 확인하게.
    if let Some(obj) = v.as_object_mut() {
        obj.insert("ledger_path".into(), root.display().to_string().into());
    }
    Ok(v)
}

/// 두 run을 구조화 diff한다(순수 추적 읽기).
pub fn compare_runs(root: &Path, id_a: &str, id_b: &str) -> Result<Value, String> {
    let cmp = compare::compare_runs(root, id_a, id_b).map_err(|e| e.to_string())?;
    serde_json::to_value(&cmp).map_err(|e| e.to_string())
}

/// goal/tag/rom로 묶은 run들의 횡단 rollup을 낸다(순수 추적 읽기).
pub fn summarize_runs(root: &Path, filter: SummaryFilter) -> Result<Value, String> {
    let s = summary::summarize_runs(root, &filter).map_err(|e| e.to_string())?;
    serde_json::to_value(&s).map_err(|e| e.to_string())
}
