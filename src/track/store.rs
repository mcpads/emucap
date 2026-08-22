use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::track::model::{Finding, Rom, Run};

const MAX_LEDGER_JSON_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LEDGER_ID_BYTES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse JSON at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize JSON: {0}")]
    Serialize(serde_json::Error),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("ledger record is corrupt: {0}")]
    Corrupt(String),
    #[error("ledger JSON exceeds {limit} bytes: {path}")]
    TooLarge { path: PathBuf, limit: u64 },
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
                "The working directory is not a Git repository, so the ledger location depends on the MCP server's current directory. Set EMUCAP_TRACK_ROOT or initialize Git in the working directory.",
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

pub fn validate_ledger_id(field: &str, value: &str) -> Result<(), TrackError> {
    if crate::path_safety::is_hyphenated_ascii_id(value, MAX_LEDGER_ID_BYTES) {
        Ok(())
    } else {
        Err(TrackError::Invalid(format!(
            "{field} must contain 1..={MAX_LEDGER_ID_BYTES} ASCII alphanumeric characters separated only by single hyphens"
        )))
    }
}

pub fn run_dir(root: &Path, rom_sha1: &str, run_id: &str) -> Result<PathBuf, TrackError> {
    validate_ledger_id("rom_sha1", rom_sha1)?;
    validate_ledger_id("run_id", run_id)?;
    Ok(root.join("roms").join(rom_sha1).join("runs").join(run_id))
}

/// tmp+rename 원자적 쓰기.
fn atomic_write(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), TrackError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_LEDGER_JSON_BYTES {
        return Err(TrackError::TooLarge {
            path: path.to_path_buf(),
            limit: MAX_LEDGER_JSON_BYTES,
        });
    }
    let parent = path
        .parent()
        .ok_or_else(|| TrackError::Invalid("ledger output path has no parent directory".into()))?;
    create_managed_directory(root, parent)?;
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
    let result = (|| -> Result<(), TrackError> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    Ok(())
}

pub fn save_run(root: &Path, run: &Run) -> Result<(), TrackError> {
    validate_ledger_id("rom_sha1", &run.rom_sha1)?;
    validate_ledger_id("run_id", &run.id)?;
    let json = serde_json::to_vec_pretty(run).map_err(TrackError::Serialize)?;
    atomic_write(
        root,
        &run_dir(root, &run.rom_sha1, &run.id)?.join("run.json"),
        &json,
    )
}

pub fn load_run(root: &Path, rom_sha1: &str, run_id: &str) -> Result<Run, TrackError> {
    let path = run_dir(root, rom_sha1, run_id)?.join("run.json");
    read_run(root, &path, rom_sha1, run_id)
}

pub fn save_rom(root: &Path, rom: &Rom) -> Result<(), TrackError> {
    validate_ledger_id("rom_sha1", &rom.sha1)?;
    let json = serde_json::to_vec_pretty(rom).map_err(TrackError::Serialize)?;
    atomic_write(
        root,
        &root.join("roms").join(&rom.sha1).join("rom.json"),
        &json,
    )
}

pub fn load_rom(root: &Path, sha1: &str) -> Result<Rom, TrackError> {
    validate_ledger_id("rom_sha1", sha1)?;
    let path = root.join("roms").join(sha1).join("rom.json");
    let rom: Rom = read_json(root, &path)?;
    if rom.sha1 != sha1 || validate_ledger_id("stored rom_sha1", &rom.sha1).is_err() {
        return Err(TrackError::Corrupt(format!(
            "rom identity does not match {}",
            path.display()
        )));
    }
    Ok(rom)
}

pub fn save_finding(root: &Path, finding: &Finding) -> Result<(), TrackError> {
    validate_ledger_id("finding_id", &finding.id)?;
    validate_ledger_id("rom_sha1", &finding.rom_sha1)?;
    if let Some(run_id) = finding.run_id.as_deref() {
        validate_ledger_id("run_id", run_id)?;
    }
    let json = serde_json::to_vec_pretty(finding).map_err(TrackError::Serialize)?;
    atomic_write(
        root,
        &root.join("findings").join(format!("{}.json", finding.id)),
        &json,
    )
}

fn read_json<T: serde::de::DeserializeOwned>(root: &Path, path: &Path) -> Result<T, TrackError> {
    validate_existing_managed_parent(root, path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TrackError::Corrupt(format!(
            "ledger member is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_LEDGER_JSON_BYTES {
        return Err(TrackError::TooLarge {
            path: path.to_path_buf(),
            limit: MAX_LEDGER_JSON_BYTES,
        });
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    Read::by_ref(&mut file)
        .take(MAX_LEDGER_JSON_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_LEDGER_JSON_BYTES {
        return Err(TrackError::TooLarge {
            path: path.to_path_buf(),
            limit: MAX_LEDGER_JSON_BYTES,
        });
    }
    serde_json::from_slice(&bytes).map_err(|source| TrackError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn read_run(root: &Path, path: &Path, rom_sha1: &str, run_id: &str) -> Result<Run, TrackError> {
    let run: Run = read_json(root, path)?;
    if run.rom_sha1 != rom_sha1
        || run.id != run_id
        || validate_ledger_id("stored rom_sha1", &run.rom_sha1).is_err()
        || validate_ledger_id("stored run_id", &run.id).is_err()
    {
        return Err(TrackError::Corrupt(format!(
            "run identity does not match {}",
            path.display()
        )));
    }
    Ok(run)
}

fn create_managed_directory(root: &Path, directory: &Path) -> Result<(), TrackError> {
    if !directory.starts_with(root) {
        return Err(TrackError::Invalid(
            "ledger path escapes the configured root".into(),
        ));
    }
    if !root.exists() {
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
    }
    create_directory_component(root)?;
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| TrackError::Invalid("ledger path escapes the configured root".into()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(TrackError::Invalid(
                "ledger path contains a non-normal component".into(),
            ));
        };
        current.push(component);
        create_directory_component(&current)?;
    }
    Ok(())
}

pub(super) fn prepare_index_path(path: &Path) -> Result<(), TrackError> {
    let parent = path
        .parent()
        .ok_or_else(|| TrackError::Invalid("index path has no parent directory".into()))?;
    create_managed_directory(parent, parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| TrackError::Invalid("index path is not portable UTF-8".into()))?;
    for candidate in [
        path.to_path_buf(),
        parent.join(format!("{file_name}-wal")),
        parent.join(format!("{file_name}-shm")),
        parent.join(format!("{file_name}-journal")),
    ] {
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(TrackError::Corrupt(format!(
                    "index member is not a regular file: {}",
                    candidate.display()
                )))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn create_directory_component(path: &Path) -> Result<(), TrackError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(TrackError::Corrupt(format!(
                "ledger directory is not a regular directory: {}",
                path.display()
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_existing_managed_parent(root: &Path, path: &Path) -> Result<(), TrackError> {
    let parent = path
        .parent()
        .ok_or_else(|| TrackError::Invalid("ledger input path has no parent directory".into()))?;
    if !parent.starts_with(root) {
        return Err(TrackError::Invalid(
            "ledger path escapes the configured root".into(),
        ));
    }
    let root_metadata = fs::symlink_metadata(root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(TrackError::Corrupt(format!(
            "ledger root is not a regular directory: {}",
            root.display()
        )));
    }
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| TrackError::Invalid("ledger path escapes the configured root".into()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(TrackError::Invalid(
                "ledger path contains a non-normal component".into(),
            ));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(TrackError::Corrupt(format!(
                "ledger path traverses a non-directory: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

fn managed_directory_exists(root: &Path, directory: &Path) -> Result<bool, TrackError> {
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(TrackError::Corrupt(format!(
                "ledger path is not a regular directory: {}",
                directory.display()
            )));
        }
        Ok(_) => {}
    }
    validate_existing_managed_parent(root, &directory.join("member"))?;
    Ok(true)
}

/// roms/*/runs/*/run.json 경로를 모은다(.tmp 등 비-run.json·비-디렉터리는 무시).
/// 디렉터리 읽기 실패는 전파. 엄격/관용 walk가 공유하는 단일 트래버설.
fn run_json_paths(root: &Path) -> Result<Vec<PathBuf>, TrackError> {
    let mut out = Vec::new();
    let roms_dir = root.join("roms");
    if !managed_directory_exists(root, &roms_dir)? {
        return Ok(out);
    }
    for rom_entry in fs::read_dir(&roms_dir)? {
        let rom_entry = rom_entry?;
        let file_type = rom_entry.file_type()?;
        if file_type.is_symlink() {
            return Err(TrackError::Corrupt(format!(
                "ROM ledger entry is a symlink: {}",
                rom_entry.path().display()
            )));
        }
        if !file_type.is_dir() {
            continue;
        }
        let runs_dir = rom_entry.path().join("runs");
        if !managed_directory_exists(root, &runs_dir)? {
            continue;
        }
        for run_entry in fs::read_dir(&runs_dir)? {
            let run_entry = run_entry?;
            let file_type = run_entry.file_type()?;
            if file_type.is_symlink() {
                return Err(TrackError::Corrupt(format!(
                    "run ledger entry is a symlink: {}",
                    run_entry.path().display()
                )));
            }
            if !file_type.is_dir() {
                continue;
            }
            let rj = run_entry.path().join("run.json");
            match fs::symlink_metadata(&rj) {
                Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                    out.push(rj)
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(out)
}

/// roms/*/rom.json 경로를 모은다.
fn rom_json_paths(root: &Path) -> Result<Vec<PathBuf>, TrackError> {
    let mut out = Vec::new();
    let roms_dir = root.join("roms");
    if !managed_directory_exists(root, &roms_dir)? {
        return Ok(out);
    }
    for rom_entry in fs::read_dir(&roms_dir)? {
        let rom_entry = rom_entry?;
        let file_type = rom_entry.file_type()?;
        if file_type.is_symlink() {
            return Err(TrackError::Corrupt(format!(
                "ROM ledger entry is a symlink: {}",
                rom_entry.path().display()
            )));
        }
        if !file_type.is_dir() {
            continue;
        }
        let rj = rom_entry.path().join("rom.json");
        match fs::symlink_metadata(&rj) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => out.push(rj),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(out)
}

/// findings/*.json 경로를 모은다.
fn finding_json_paths(root: &Path) -> Result<Vec<PathBuf>, TrackError> {
    let mut out = Vec::new();
    let dir = root.join("findings");
    if !managed_directory_exists(root, &dir)? {
        return Ok(out);
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json")
            && (file_type.is_file() || file_type.is_symlink())
        {
            out.push(p);
        }
    }
    Ok(out)
}

/// 경로 목록을 로드하되 파싱 실패는 에러 전파 대신 skipped로 모은다.
fn load_lenient<T, F>(paths: Vec<PathBuf>, mut load: F) -> (Vec<T>, Vec<PathBuf>)
where
    F: FnMut(&Path) -> Result<T, TrackError>,
{
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for p in paths {
        match load(&p) {
            Ok(v) => out.push(v),
            Err(_) => skipped.push(p),
        }
    }
    (out, skipped)
}

/// roms/*/runs/*/run.json 전부 로드. 손상 run.json은 에러(무결성과 데이터 일치 검사).
pub fn walk_runs(root: &Path) -> Result<Vec<Run>, TrackError> {
    run_json_paths(root)?
        .iter()
        .map(|path| {
            let (rom_sha1, run_id) = run_identity_from_path(root, path)?;
            read_run(root, path, &rom_sha1, &run_id)
        })
        .collect()
}

/// roms/*/rom.json 전부 로드. 손상은 에러.
pub fn walk_roms(root: &Path) -> Result<Vec<Rom>, TrackError> {
    rom_json_paths(root)?
        .iter()
        .map(|path| {
            let rom_sha1 = rom_identity_from_path(root, path)?;
            let rom: Rom = read_json(root, path)?;
            if rom.sha1 != rom_sha1 || validate_ledger_id("stored rom_sha1", &rom.sha1).is_err() {
                return Err(TrackError::Corrupt(format!(
                    "rom identity does not match {}",
                    path.display()
                )));
            }
            Ok(rom)
        })
        .collect()
}

/// findings/*.json 전부 로드. 손상은 에러.
pub fn walk_findings(root: &Path) -> Result<Vec<Finding>, TrackError> {
    finding_json_paths(root)?
        .iter()
        .map(|path| read_finding(root, path))
        .collect()
}

/// walk_runs의 손상 내성 변형: 파싱 실패는 skipped 경로로 모은다(디렉터리 읽기 실패는 전파).
pub fn walk_runs_lenient(root: &Path) -> Result<(Vec<Run>, Vec<PathBuf>), TrackError> {
    Ok(load_lenient(run_json_paths(root)?, |path| {
        let (rom_sha1, run_id) = run_identity_from_path(root, path)?;
        read_run(root, path, &rom_sha1, &run_id)
    }))
}

/// walk_roms의 손상 내성 변형.
pub fn walk_roms_lenient(root: &Path) -> Result<(Vec<Rom>, Vec<PathBuf>), TrackError> {
    Ok(load_lenient(rom_json_paths(root)?, |path| {
        let rom_sha1 = rom_identity_from_path(root, path)?;
        let rom: Rom = read_json(root, path)?;
        if rom.sha1 != rom_sha1 || validate_ledger_id("stored rom_sha1", &rom.sha1).is_err() {
            return Err(TrackError::Corrupt(format!(
                "rom identity does not match {}",
                path.display()
            )));
        }
        Ok(rom)
    }))
}

/// walk_findings의 손상 내성 변형(이질 *.json·손상 finding을 skipped로).
pub fn walk_findings_lenient(root: &Path) -> Result<(Vec<Finding>, Vec<PathBuf>), TrackError> {
    Ok(load_lenient(finding_json_paths(root)?, |path| {
        read_finding(root, path)
    }))
}

/// run_id(전역 유일)로 run을 타깃 로드한다. roms/*/runs/<run_id>/run.json만 검사해 일치 1개만
/// 로드(무관 run 미파싱 → corrupt 격리). 미존재 Ok(None). 중복(여러 rom) → Err(Conflict).
/// 일치 run 손상 → Err(전파).
pub fn find_run_by_id(root: &Path, run_id: &str) -> Result<Option<Run>, TrackError> {
    validate_ledger_id("run_id", run_id)?;
    let roms_dir = root.join("roms");
    if !managed_directory_exists(root, &roms_dir)? {
        return Ok(None);
    }
    let mut found: Option<PathBuf> = None;
    for rom_entry in fs::read_dir(&roms_dir)? {
        let rom_entry = rom_entry?;
        if !rom_entry.file_type()?.is_dir() {
            continue;
        }
        let rom_sha1 = rom_entry
            .file_name()
            .to_str()
            .ok_or_else(|| TrackError::Corrupt("ROM directory name is not UTF-8".into()))?
            .to_string();
        if validate_ledger_id("stored rom_sha1", &rom_sha1).is_err() {
            continue;
        }
        let rj = rom_entry.path().join("runs").join(run_id).join("run.json");
        match fs::symlink_metadata(&rj) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if found.is_some() {
                    return Err(TrackError::Conflict(format!(
                    "duplicate run_id {run_id} appears in multiple ROM directories; global uniqueness is violated"
                )));
                }
                found = Some(rj);
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(TrackError::Corrupt(format!(
                    "run record is a symlink: {}",
                    rj.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    match found {
        Some(rj) => {
            let (rom_sha1, expected_run_id) = run_identity_from_path(root, &rj)?;
            Ok(Some(read_run(root, &rj, &rom_sha1, &expected_run_id)?))
        }
        None => Ok(None),
    }
}

fn run_identity_from_path(root: &Path, path: &Path) -> Result<(String, String), TrackError> {
    let components = path
        .strip_prefix(root)
        .map_err(|_| TrackError::Corrupt("run record is outside the ledger root".into()))?
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_string),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| TrackError::Corrupt("run record path is not portable UTF-8".into()))?;
    if components.len() != 5
        || components[0] != "roms"
        || components[2] != "runs"
        || components[4] != "run.json"
    {
        return Err(TrackError::Corrupt(format!(
            "unexpected run record path: {}",
            path.display()
        )));
    }
    if validate_ledger_id("stored rom_sha1", &components[1]).is_err()
        || validate_ledger_id("stored run_id", &components[3]).is_err()
    {
        return Err(TrackError::Corrupt(format!(
            "run record path contains an invalid stored identity: {}",
            path.display()
        )));
    }
    Ok((components[1].clone(), components[3].clone()))
}

fn rom_identity_from_path(root: &Path, path: &Path) -> Result<String, TrackError> {
    let components = path
        .strip_prefix(root)
        .map_err(|_| TrackError::Corrupt("ROM record is outside the ledger root".into()))?
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_string),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| TrackError::Corrupt("ROM record path is not portable UTF-8".into()))?;
    if components.len() != 3 || components[0] != "roms" || components[2] != "rom.json" {
        return Err(TrackError::Corrupt(format!(
            "unexpected ROM record path: {}",
            path.display()
        )));
    }
    if validate_ledger_id("stored rom_sha1", &components[1]).is_err() {
        return Err(TrackError::Corrupt(format!(
            "ROM record path contains an invalid stored identity: {}",
            path.display()
        )));
    }
    Ok(components[1].clone())
}

fn read_finding(root: &Path, path: &Path) -> Result<Finding, TrackError> {
    let finding: Finding = read_json(root, path)?;
    let expected_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| TrackError::Corrupt("finding file name is not portable UTF-8".into()))?;
    if finding.id != expected_id
        || validate_ledger_id("stored finding_id", &finding.id).is_err()
        || validate_ledger_id("stored rom_sha1", &finding.rom_sha1).is_err()
        || finding
            .run_id
            .as_deref()
            .is_some_and(|run_id| validate_ledger_id("stored run_id", run_id).is_err())
    {
        return Err(TrackError::Corrupt(format!(
            "finding identity does not match {}",
            path.display()
        )));
    }
    Ok(finding)
}
