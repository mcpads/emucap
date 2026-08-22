//! 회귀 라이브러리 순수 코어 — 케이스 타입·판정·집계·무비 파싱. 에뮬레이터·link 비특이.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::analysis::bisect::Predicate;
pub use crate::input_movie::{parse_movie, Movie, MovieFrame};

/// 케이스를 잡은 빌드 식별.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RomRef {
    pub sha1: String,
    pub path_hint: String,
}

/// 재현 방식. 태그된 enum — 미래 종류는 변형 추가로 확장(기존 불변).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Repro {
    Savestate {
        state_sha1: String,
        advance_frames: u64,
    },
    InputReplay {
        /// "reset" 또는 베이스 savestate sha1
        start: String,
        movie: String,
        /// 판정 시점 앵커 BP(없으면 타이밍 불변 가정)
        anchor: Option<Predicate>,
    },
}

/// 기대 — 버그가 없어야(absent) / 아직 있어야(present).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    #[default]
    Absent,
    Present,
}

/// 회귀 케이스(=`case.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Case {
    pub format_version: u32,
    pub id: String,
    pub description: String,
    pub rom: RomRef,
    pub repro: Repro,
    pub predicate: Predicate,
    #[serde(default)]
    pub expect: Expect,
}

pub const CASE_FORMAT_VERSION: u32 = 1;
const MAX_CASE_JSON_BYTES: u64 = 1024 * 1024;

/// 한 케이스의 판정 결과. 신호(pass/fail)와 무효(나머지)를 구분. 미래 무효 사유는 변형 추가.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Pass,
    Fail,
    RomMismatch,
    MissingPayload,
    Unsupported,
    Invalid(String),
    InvalidRead,
    ReproError(String),
    DriftSuspected,
}

/// 결과 버킷 — 종료코드·요약 집계 단위.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bucket {
    Passed,
    Failed,
    Invalid,
}

impl Verdict {
    /// 판정을 버킷으로. 새 Verdict 변형은 여기서 컴파일러가 분류를 강제한다(확장성).
    pub fn bucket(&self) -> Bucket {
        match self {
            Verdict::Pass => Bucket::Passed,
            Verdict::Fail => Bucket::Failed,
            Verdict::RomMismatch
            | Verdict::MissingPayload
            | Verdict::Unsupported
            | Verdict::Invalid(_)
            | Verdict::InvalidRead
            | Verdict::ReproError(_)
            | Verdict::DriftSuspected => Bucket::Invalid,
        }
    }

    /// 사람·JSON용 짧은 코드.
    pub fn code(&self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::RomMismatch => "rom_mismatch",
            Verdict::MissingPayload => "missing_payload",
            Verdict::Unsupported => "unsupported",
            Verdict::Invalid(_) => "invalid",
            Verdict::InvalidRead => "invalid_read",
            Verdict::ReproError(_) => "repro_error",
            Verdict::DriftSuspected => "drift_suspected",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseResult {
    pub id: String,
    pub verdict: Verdict,
}

/// 스위트 집계.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub passed: usize,
    pub failed: usize,
    pub invalid: usize,
    pub results: Vec<CaseResult>,
}

impl Summary {
    pub fn from_results(results: Vec<CaseResult>) -> Self {
        let mut passed = 0;
        let mut failed = 0;
        let mut invalid = 0;
        for r in &results {
            match r.verdict.bucket() {
                Bucket::Passed => passed += 1,
                Bucket::Failed => failed += 1,
                Bucket::Invalid => invalid += 1,
            }
        }
        Summary {
            passed,
            failed,
            invalid,
            results,
        }
    }

    /// 성공 종료코드 여부. 실패·무효가 없고 통과가 1건 이상이어야 한다("검증 0건"은 실패).
    pub fn ok(&self) -> bool {
        self.failed == 0 && self.invalid == 0 && self.passed > 0
    }
}

/// 읽은 바이트로 케이스를 판정한다. 읽기 길이가 predicate.length와 다르면 `InvalidRead`
/// (조용한 패딩/절단 금지). predicate.eval은 "버그 있음(bad)"; expect와 대조해 pass/fail.
pub fn evaluate(read: &[u8], predicate: &Predicate, expect: Expect) -> Verdict {
    if predicate.length == 0 || predicate.length > 8 {
        return Verdict::InvalidRead;
    }
    if read.len() as u64 != predicate.length {
        return Verdict::InvalidRead;
    }
    let bad = predicate.eval(read);
    match (expect, bad) {
        (Expect::Absent, false) => Verdict::Pass,
        (Expect::Absent, true) => Verdict::Fail,
        (Expect::Present, true) => Verdict::Pass,
        (Expect::Present, false) => Verdict::Fail,
    }
}

/// dir/case.json에 케이스를 직렬화한다.
pub fn save_case(dir: &Path, case: &Case) -> std::io::Result<()> {
    validate_case_file_references(case).map_err(std::io::Error::other)?;
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "regression case directory is not a regular directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_string_pretty(case).map_err(std::io::Error::other)?;
    let path = dir.join("case.json");
    if std::fs::symlink_metadata(&path)
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "regression case.json is a symlink",
        ));
    }
    crate::path_safety::atomic_write_file(&path, json.as_bytes())
}

/// dir/case.json을 읽는다.
pub fn load_case(dir: &Path) -> Result<Case, String> {
    let path = dir.join("case.json");
    let bytes =
        crate::path_safety::read_bounded_regular_member(dir, "case.json", MAX_CASE_JSON_BYTES)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    let case: Case =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    validate_case_file_references(&case)?;
    Ok(case)
}

/// 스위트 디렉토리의 하위 디렉토리마다 case.json을 로드한다(디렉토리명 = 케이스 폴더).
pub fn load_suite(suite_dir: &Path) -> Result<Vec<(PathBuf, Case)>, String> {
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(suite_dir).map_err(|e| format!("{}: {e}", suite_dir.display()))?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_symlink() {
            return Err(format!(
                "regression suite entry is a symlink: {}",
                entry.path().display()
            ));
        }
        if !file_type.is_dir() {
            continue;
        }
        let case_path = entry.path().join("case.json");
        match std::fs::symlink_metadata(&case_path) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                dirs.push(entry.path())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", case_path.display())),
        }
    }
    dirs.sort();
    for d in dirs {
        let case = load_case(&d)?;
        out.push((d, case));
    }
    Ok(out)
}

pub fn validate_case_file_references(case: &Case) -> Result<(), String> {
    if !crate::path_safety::is_hyphenated_ascii_id(&case.id, 96) {
        return Err(
            "case id must use ASCII alphanumeric segments separated by single hyphens".into(),
        );
    }
    match &case.repro {
        Repro::Savestate { state_sha1, .. } => {
            if !crate::path_safety::is_hyphenated_ascii_id(state_sha1, 128) {
                return Err("savestate identity is not a safe identifier".into());
            }
        }
        Repro::InputReplay { start, movie, .. } => {
            if start != "reset" && !crate::path_safety::is_hyphenated_ascii_id(start, 128) {
                return Err("input replay start identity is not a safe identifier".into());
            }
            if !crate::path_safety::is_portable_file_name(movie, 128) {
                return Err("input replay movie must be a portable file name, not a path".into());
            }
        }
    }
    Ok(())
}
