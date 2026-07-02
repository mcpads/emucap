use std::path::{Path, PathBuf};

use crate::track::model::{Finding, Rom, Run};

#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    #[error("입출력 오류: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON 파싱 실패 {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("JSON 직렬화 실패: {0}")]
    Serialize(serde_json::Error),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
}

/// cwd에서 위로 올라가며 `.git`을 가진 가장 가까운 디렉터리(=에이전트가 작업하는 패치 프로젝트의
/// git root). 추적 ledger·아티팩트는 이 repo에 살아야 한다("모든 기록은 레포지토리에" 불변식 —
/// commit 가능한 repo에 증거가 남아야 agent-independent·evidence-first가 성립). emucap 자체 repo를
/// 찾는 bin의 find_repo_root와 의도적으로 다르다 — 여기 기준은 *작업 중인* repo다.
pub fn nearest_git_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .find(|a| a.join(".git").exists())
        .map(|p| p.to_path_buf())
}

/// 추적 루트가 어떻게 정해졌나 — bootstrap이 경로 모호성을 진단하게 source를 함께 노출한다.
/// `CwdFallback`은 비-git working dir라 MCP 서버 cwd에 의존하는 위험 케이스다(경고 대상).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackRootSource {
    /// EMUCAP_TRACK_ROOT 명시.
    Env,
    /// 작업 repo의 nearest git root + `.emucap`.
    GitRoot,
    /// git root도 없어 `./.emucap`(서버 cwd 상대) — 위치 모호, 경고 대상.
    CwdFallback,
}

impl TrackRootSource {
    /// bootstrap 응답용 안정 식별자(env|git_root|cwd_fallback).
    pub fn as_str(self) -> &'static str {
        match self {
            TrackRootSource::Env => "env",
            TrackRootSource::GitRoot => "git_root",
            TrackRootSource::CwdFallback => "cwd_fallback",
        }
    }

    /// cwd_fallback이면 사람이 읽을 경고, 아니면 None. bootstrap이 ledger_path_warning으로 노출한다.
    pub fn warning(self) -> Option<&'static str> {
        match self {
            TrackRootSource::CwdFallback => Some(
                "비-git working dir라 ledger가 MCP 서버 cwd에 의존(위치 모호) — EMUCAP_TRACK_ROOT를 명시하거나 작업 디렉터리에서 git init을 권장한다.",
            ),
            _ => None,
        }
    }
}

/// 추적 루트 결정(순수): 명시 override > git root의 .emucap > cwd 상대 .emucap(폴백). source도 함께 돌려준다.
/// git root 기본이라 cwd가 roms/(gitignore 영역)여도 ledger가 commit 가능한 repo 루트에 남는다.
pub fn resolve_track_root_with_source(
    explicit: Option<std::ffi::OsString>,
    git_root: Option<PathBuf>,
) -> (PathBuf, TrackRootSource) {
    if let Some(explicit) = explicit {
        return (PathBuf::from(explicit), TrackRootSource::Env);
    }
    if let Some(git_root) = git_root {
        return (git_root.join(".emucap"), TrackRootSource::GitRoot);
    }
    (PathBuf::from(".emucap"), TrackRootSource::CwdFallback)
}

/// 추적 루트 결정(순수, 경로만): source가 필요 없는 호출부용 얇은 래퍼.
pub fn resolve_track_root(
    explicit: Option<std::ffi::OsString>,
    git_root: Option<PathBuf>,
) -> PathBuf {
    resolve_track_root_with_source(explicit, git_root).0
}

/// 추적 루트 + source: EMUCAP_TRACK_ROOT(명시) > nearest git root의 .emucap > ./.emucap(폴백).
pub fn root_from_env_with_source() -> (PathBuf, TrackRootSource) {
    resolve_track_root_with_source(std::env::var_os("EMUCAP_TRACK_ROOT"), nearest_git_root())
}

/// 추적 루트(경로만): EMUCAP_TRACK_ROOT(명시) > nearest git root의 .emucap > ./.emucap(폴백).
pub fn root_from_env() -> PathBuf {
    root_from_env_with_source().0
}

/// 아티팩트 상대경로 해소(순수): 절대경로는 그대로, 상대경로는 git root 기준(없으면 cwd 상대 폴백).
/// log_artifact가 MCP 서버 cwd(에이전트 cwd와 다를 수 있음)에 의존하지 않게 — 상대경로 기준을
/// *작업 repo* 루트로 고정해 최소놀람·재현성을 준다.
pub fn resolve_artifact_path(raw: &Path, git_root: Option<&Path>) -> PathBuf {
    if raw.is_absolute() {
        return raw.to_path_buf();
    }
    match git_root {
        Some(root) => root.join(raw),
        None => raw.to_path_buf(),
    }
}

pub fn run_dir(root: &Path, rom_sha1: &str, run_id: &str) -> PathBuf {
    root.join("roms").join(rom_sha1).join("runs").join(run_id)
}

/// tmp+rename 원자적 쓰기.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), TrackError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // tmp 이름은 writer별로 유일해야 한다 — 고정 이름이면 두 writer가 같은 tmp를 truncate/interleave해
    // 대상이 깨질 수 있다(rename 자체는 원자라 reader는 안전하나, tmp 충돌은 별개). pid + 프로세스 내
    // 단조 카운터로 충돌을 막는다. 숨김 prefix(.)와 .tmp 접미라 walk_runs(run.json 정확 일치)에서 무시된다.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("run.json");
    let tmp = path.with_file_name(format!(".{base}.{}.{seq}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_run(root: &Path, run: &Run) -> Result<(), TrackError> {
    let json = serde_json::to_vec_pretty(run).map_err(TrackError::Serialize)?;
    atomic_write(
        &run_dir(root, &run.rom_sha1, &run.id).join("run.json"),
        &json,
    )
}

pub fn load_run(root: &Path, rom_sha1: &str, run_id: &str) -> Result<Run, TrackError> {
    let path = run_dir(root, rom_sha1, run_id).join("run.json");
    read_json(&path)
}

pub fn save_rom(root: &Path, rom: &Rom) -> Result<(), TrackError> {
    let json = serde_json::to_vec_pretty(rom).map_err(TrackError::Serialize)?;
    atomic_write(&root.join("roms").join(&rom.sha1).join("rom.json"), &json)
}

pub fn load_rom(root: &Path, sha1: &str) -> Result<Rom, TrackError> {
    read_json(&root.join("roms").join(sha1).join("rom.json"))
}

pub fn save_finding(root: &Path, finding: &Finding) -> Result<(), TrackError> {
    let json = serde_json::to_vec_pretty(finding).map_err(TrackError::Serialize)?;
    atomic_write(
        &root.join("findings").join(format!("{}.json", finding.id)),
        &json,
    )
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, TrackError> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|source| TrackError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// roms/*/runs/*/run.json 경로를 모은다(.tmp 등 비-run.json·비-디렉터리는 무시).
/// 디렉터리 읽기 실패는 전파. 엄격/관용 walk가 공유하는 단일 트래버설.
fn run_json_paths(root: &Path) -> Result<Vec<PathBuf>, TrackError> {
    let mut out = Vec::new();
    let roms_dir = root.join("roms");
    if !roms_dir.is_dir() {
        return Ok(out);
    }
    for rom_entry in std::fs::read_dir(&roms_dir)? {
        let runs_dir = rom_entry?.path().join("runs");
        if !runs_dir.is_dir() {
            continue;
        }
        for run_entry in std::fs::read_dir(&runs_dir)? {
            let rj = run_entry?.path().join("run.json");
            if rj.is_file() {
                out.push(rj);
            }
        }
    }
    Ok(out)
}

/// roms/*/rom.json 경로를 모은다.
fn rom_json_paths(root: &Path) -> Result<Vec<PathBuf>, TrackError> {
    let mut out = Vec::new();
    let roms_dir = root.join("roms");
    if !roms_dir.is_dir() {
        return Ok(out);
    }
    for rom_entry in std::fs::read_dir(&roms_dir)? {
        let rj = rom_entry?.path().join("rom.json");
        if rj.is_file() {
            out.push(rj);
        }
    }
    Ok(out)
}

/// findings/*.json 경로를 모은다.
fn finding_json_paths(root: &Path) -> Result<Vec<PathBuf>, TrackError> {
    let mut out = Vec::new();
    let dir = root.join("findings");
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir)? {
        let p = entry?.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(p);
        }
    }
    Ok(out)
}

/// 경로 목록을 로드하되 파싱 실패는 에러 전파 대신 skipped로 모은다.
fn load_lenient<T: serde::de::DeserializeOwned>(paths: Vec<PathBuf>) -> (Vec<T>, Vec<PathBuf>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for p in paths {
        match read_json::<T>(&p) {
            Ok(v) => out.push(v),
            Err(_) => skipped.push(p),
        }
    }
    (out, skipped)
}

/// roms/*/runs/*/run.json 전부 로드. 손상 run.json은 에러(무결성 검사·정합용).
pub fn walk_runs(root: &Path) -> Result<Vec<Run>, TrackError> {
    run_json_paths(root)?.iter().map(|p| read_json(p)).collect()
}

/// roms/*/rom.json 전부 로드. 손상은 에러.
pub fn walk_roms(root: &Path) -> Result<Vec<Rom>, TrackError> {
    rom_json_paths(root)?.iter().map(|p| read_json(p)).collect()
}

/// findings/*.json 전부 로드. 손상은 에러.
pub fn walk_findings(root: &Path) -> Result<Vec<Finding>, TrackError> {
    finding_json_paths(root)?
        .iter()
        .map(|p| read_json(p))
        .collect()
}

/// walk_runs의 손상 내성 변형: 파싱 실패는 skipped 경로로 모은다(디렉터리 읽기 실패는 전파).
pub fn walk_runs_lenient(root: &Path) -> Result<(Vec<Run>, Vec<PathBuf>), TrackError> {
    Ok(load_lenient(run_json_paths(root)?))
}

/// walk_roms의 손상 내성 변형.
pub fn walk_roms_lenient(root: &Path) -> Result<(Vec<Rom>, Vec<PathBuf>), TrackError> {
    Ok(load_lenient(rom_json_paths(root)?))
}

/// walk_findings의 손상 내성 변형(이질 *.json·손상 finding을 skipped로).
pub fn walk_findings_lenient(root: &Path) -> Result<(Vec<Finding>, Vec<PathBuf>), TrackError> {
    Ok(load_lenient(finding_json_paths(root)?))
}

/// run_id(전역 유일)로 run을 타깃 로드한다. roms/*/runs/<run_id>/run.json만 검사해 일치 1개만
/// 로드(무관 run 미파싱 → corrupt 격리). 미존재 Ok(None). 중복(여러 rom) → Err(Conflict).
/// 일치 run 손상 → Err(전파).
pub fn find_run_by_id(root: &Path, run_id: &str) -> Result<Option<Run>, TrackError> {
    let roms_dir = root.join("roms");
    if !roms_dir.is_dir() {
        return Ok(None);
    }
    let mut found: Option<PathBuf> = None;
    for rom_entry in std::fs::read_dir(&roms_dir)? {
        let rj = rom_entry?.path().join("runs").join(run_id).join("run.json");
        if rj.is_file() {
            if found.is_some() {
                return Err(TrackError::Conflict(format!(
                    "중복 run_id: {run_id} (여러 rom 디렉터리에 존재 — 전역 유일 위반)"
                )));
            }
            found = Some(rj);
        }
    }
    match found {
        Some(rj) => Ok(Some(read_json::<Run>(&rj)?)),
        None => Ok(None),
    }
}
