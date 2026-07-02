use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::track::id::IdGen;
use crate::track::model::*;
use crate::track::repro;
use crate::track::store::{self, TrackError};

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error(transparent)]
    Track(#[from] TrackError),
    #[error("run을 찾을 수 없음: {rom_sha1}/{run_id}")]
    RunNotFound { rom_sha1: String, run_id: String },
}

/// 새 Run을 생성하고 rom.json + run.json을 정본에 쓴다(status=running).
#[allow(clippy::too_many_arguments)]
pub fn create_run(
    root: &Path,
    gen: &dyn IdGen,
    now: &str,
    rom_sha1: &str,
    goal: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
    connection_ref: Option<String>,
) -> Result<Run, OpsError> {
    // defense-in-depth(#43): rom_sha1은 run_dir의 디렉터리 컴포넌트가 된다. 구분자·'..'·절대경로가
    // 섞이면 root.join("roms").join(rom_sha1)이 roms/ 서브트리를 탈출해(예: ".." → root/runs) walk_runs·
    // 인덱스에서 안 보이는 고아가 되거나 'File exists'로 깨진다. *단일 Normal 컴포넌트*(진짜 sha1)만 허용.
    let is_single_normal = {
        let mut comps = std::path::Path::new(rom_sha1).components();
        matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
    };
    if rom_sha1.is_empty() || !is_single_normal {
        return Err(OpsError::Track(TrackError::Invalid(format!(
            "rom_sha1이 안전한 단일 경로 컴포넌트(sha1)가 아니다: {rom_sha1:?} — 실제 sha1을 넘겨라(예: shasum -a1 <content>)"
        ))));
    }
    // rom.json이 없으면 생성(있으면 first_seen 보존). '없음'(NotFound)만 생성 트리거로 좁힌다 —
    // 손상 JSON·IO 오류를 '없음'으로 오인해 기존 first_seen/platform/title을 조용히 덮어쓰지 않는다.
    match store::load_rom(root, rom_sha1) {
        Ok(_) => {}
        Err(TrackError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            store::save_rom(
                root,
                &Rom {
                    sha1: rom_sha1.to_string(),
                    platform: String::new(),
                    title: None,
                    first_seen: now.to_string(),
                },
            )?;
        }
        Err(e) => return Err(e.into()),
    }
    let run = Run {
        format_version: RUN_FORMAT_VERSION,
        id: gen.new_id(),
        rom_sha1: rom_sha1.to_string(),
        goal,
        description,
        tags,
        status: RunStatus::Running,
        started_at: now.to_string(),
        ended_at: None,
        agent: None,
        session: None,
        connection_ref,
        repro_base: Some("reset".into()),
        repro_movie_ref: None,
        repro_status: Some(repro::derive_status(&[])),
        gates: vec![],
        metrics: vec![],
        artifacts: vec![],
        interventions: vec![],
    };
    store::save_run(root, &run)?;
    Ok(run)
}

/// 기존 run을 로드해 status/ended_at을 갱신하고 저장한다.
pub fn finish_run(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    status: RunStatus,
    now: &str,
) -> Result<(), OpsError> {
    let mut run = load(root, rom_sha1, run_id)?;
    run.status = status;
    run.ended_at = Some(now.to_string());
    store::save_run(root, &run)?;
    Ok(())
}

/// run_id(전역 유일)로 run을 찾아 종료한다. in-memory 활성 run 상태에 의존하지 않으므로, 서버
/// 재시작 등으로 고아화된 'running' run을 복구 종료하는 데 쓴다(#56). 미존재면 Ok(None).
pub fn finish_run_by_id(
    root: &Path,
    run_id: &str,
    status: RunStatus,
    now: &str,
) -> Result<Option<String>, OpsError> {
    match store::find_run_by_id(root, run_id)? {
        Some(run) => {
            finish_run(root, &run.rom_sha1, &run.id, status, now)?;
            Ok(Some(run.id))
        }
        None => Ok(None),
    }
}

/// 같은 connection_ref의 status=Running run들을 종료한다(원장 위생, #56 — 새 run 시작 시 같은
/// 연결의 고아 running을 정리). 닫은 run_id들을 반환. connection_ref로 한정해 다른 연결의
/// 정상 running run은 건드리지 않는다(무차별 종료 방지).
pub fn finish_stale_running(
    root: &Path,
    connection_ref: &str,
    status: RunStatus,
    now: &str,
) -> Result<Vec<String>, OpsError> {
    let mut closed = Vec::new();
    for run in store::walk_runs(root)? {
        if run.status == RunStatus::Running && run.connection_ref.as_deref() == Some(connection_ref)
        {
            finish_run(root, &run.rom_sha1, &run.id, status.clone(), now)?;
            closed.push(run.id);
        }
    }
    Ok(closed)
}

/// 공통: run.json 로드(없으면 RunNotFound, 손상이면 Track(e)).
pub(crate) fn load(root: &Path, rom_sha1: &str, run_id: &str) -> Result<Run, OpsError> {
    store::load_run(root, rom_sha1, run_id).map_err(|e| match &e {
        TrackError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => OpsError::RunNotFound {
            rom_sha1: rom_sha1.to_string(),
            run_id: run_id.to_string(),
        },
        _ => OpsError::Track(e),
    })
}

/// run에 메트릭 1건 append.
#[allow(clippy::too_many_arguments)]
pub fn log_metric(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    gen: &dyn IdGen,
    now: &str,
    key: &str,
    value: f64,
) -> Result<(), OpsError> {
    let mut run = load(root, rom_sha1, run_id)?;
    run.metrics.push(Metric {
        id: gen.new_id(),
        key: key.to_string(),
        value,
        created_at: now.to_string(),
    });
    store::save_run(root, &run)?;
    Ok(())
}

/// run에 게이트 1건 append.
#[allow(clippy::too_many_arguments)]
pub fn log_gate(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    gen: &dyn IdGen,
    now: &str,
    name: &str,
    kind: GateKind,
    passed: Option<bool>,
    evidence_ref: Option<String>,
    detail: Option<String>,
    case_ref: Option<String>,
) -> Result<(), OpsError> {
    let mut run = load(root, rom_sha1, run_id)?;
    run.gates.push(Gate {
        id: gen.new_id(),
        name: name.to_string(),
        kind,
        passed,
        evidence_ref,
        detail,
        case_ref,
        created_at: now.to_string(),
    });
    store::save_run(root, &run)?;
    Ok(())
}

/// 이미 존재하는 파일을 artifact로 등록(sha256 계산). 새 캡처 안 함.
///
/// `path` — run 디렉토리 기준 상대경로(run 디렉토리 밖 파일이면 절대경로, 참조).
/// 파일이 run 디렉토리 아래에 있으면 상대 경로로 저장하고, 밖이면 절대 경로를 그대로 저장한다.
#[allow(clippy::too_many_arguments)]
pub fn log_artifact(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    gen: &dyn IdGen,
    kind: &str,
    abs_path: &Path,
    meta: Option<serde_json::Value>,
) -> Result<String, OpsError> {
    let mut file = std::fs::File::open(abs_path).map_err(TrackError::Io)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).map_err(TrackError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let sha256 = format!("{:x}", hasher.finalize());

    let run_dir = store::run_dir(root, rom_sha1, run_id);
    // 저장 경로 우선순위: run_dir 상대 > repo(track root의 부모=git root) 상대 > 절대(repo 밖만).
    // repo 상대로 저장해야 ledger를 commit해 다른 머신/클론이 승계해도 참조가 유효하다(절대경로는 비이식).
    let stored = abs_path
        .strip_prefix(&run_dir)
        .ok()
        .or_else(|| {
            root.parent()
                .and_then(|repo| abs_path.strip_prefix(repo).ok())
        })
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| abs_path.to_string_lossy().into_owned());

    let mut run = load(root, rom_sha1, run_id)?;
    let id = gen.new_id();
    run.artifacts.push(Artifact {
        id: id.clone(),
        kind: kind.to_string(),
        path: stored,
        sha256,
        meta,
    });
    store::save_run(root, &run)?;
    Ok(id)
}

/// 재현 base/movie 설정. repro_status는 현재 interventions로 재도출(read-only 파생).
pub fn set_reproduction(
    root: &Path,
    rom_sha1: &str,
    run_id: &str,
    base: Option<String>,
    movie_ref: Option<String>,
) -> Result<(), OpsError> {
    let mut run = load(root, rom_sha1, run_id)?;
    if base.is_some() {
        run.repro_base = base;
    }
    if movie_ref.is_some() {
        run.repro_movie_ref = movie_ref;
    }
    run.repro_status = Some(repro::derive_status(&run.interventions));
    store::save_run(root, &run)?;
    Ok(())
}

/// 발견을 findings/<id>.json에 기록.
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
) -> Result<String, OpsError> {
    let id = gen.new_id();
    store::save_finding(
        root,
        &Finding {
            id: id.clone(),
            rom_sha1: rom_sha1.to_string(),
            run_id,
            claim: claim.to_string(),
            evidence_refs,
            promoted,
            created_at: now.to_string(),
        },
    )?;
    Ok(id)
}

/// active run에 외부 개입(write_memory/load_state/reset 등)을 기록한다. seq는 내부 부여.
/// repro_status는 전체 interventions로 재도출(read-only 파생).
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
    args: serde_json::Value,
) -> Result<(), OpsError> {
    let mut run = load(root, rom_sha1, run_id)?;
    let seq = run.interventions.len() as u64;
    run.interventions.push(Intervention {
        id: gen.new_id(),
        seq,
        at_frame,
        at_event,
        frozen_context,
        op: op.to_string(),
        args,
        created_at: now.to_string(),
    });
    run.repro_status = Some(repro::derive_status(&run.interventions));
    store::save_run(root, &run)?;
    Ok(())
}

/// steal로 lineage가 끊긴 run을 savestate_only로 강제 강등한다.
/// 평소 repro_status는 derive_status 파생이지만, 이건 시스템이 연결 steal을
/// 감지한 강등이라 파생 규칙의 예외다.
pub fn mark_savestate_only(root: &Path, rom_sha1: &str, run_id: &str) -> Result<(), OpsError> {
    let mut run = load(root, rom_sha1, run_id)?;
    run.repro_status = Some(ReproStatus::SavestateOnly);
    store::save_run(root, &run)?;
    Ok(())
}
