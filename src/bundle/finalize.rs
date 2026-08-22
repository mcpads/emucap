use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

pub use super::error::FinalizeError;
use super::manifest::{Artifact, Manifest, RomId, FORMAT_VERSION};
use super::raw::parse_raw;
use crate::rom::sha1_of_file;

const NOTE_TEMPLATE: &str = "\
# 문제 설명

(이 번들에서 무엇이 잘못되었는지 사람이 적습니다. AI가 이 설명과 슬라이스의
스크린샷·상태를 근거로 분석합니다.)

## 증상

## 직전 행동

## 기대 동작
";
const MAX_RAW_JSON_BYTES: u64 = 4 * 1024 * 1024;

pub fn finalize(bundle_dir: &Path, rom_override: Option<&Path>) -> Result<Manifest, FinalizeError> {
    let raw_path = bundle_dir.join("_raw.json");
    if fs::symlink_metadata(&raw_path).is_err() {
        return Err(FinalizeError::RawNotFound(raw_path));
    }
    let raw_bytes = crate::path_safety::read_bounded_regular_member(
        bundle_dir,
        "_raw.json",
        MAX_RAW_JSON_BYTES,
    )?;
    let raw = parse_raw(
        std::str::from_utf8(&raw_bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
    )?;

    if raw.format_version != FORMAT_VERSION {
        return Err(FinalizeError::UnsupportedFormatVersion(raw.format_version));
    }
    if raw.slices.is_empty() {
        return Err(FinalizeError::NoSlices);
    }

    // ROM 해시. --rom 오버라이드가 있으면 그 경로를, 없으면 raw.rom_path를 쓴다
    // (raw.rom_path는 절대 경로이거나 번들 디렉토리 기준 상대 경로).
    let rom_path = match rom_override {
        Some(p) => p.to_path_buf(),
        None => resolve(bundle_dir, &raw.rom_path),
    };
    if !rom_path.exists() {
        return Err(FinalizeError::RomNotFound(rom_path));
    }
    let sha1 = sha1_of_file(&rom_path)?;
    let path_hint = match rom_override {
        Some(p) => p.display().to_string(),
        None => raw.rom_path.clone(),
    };

    // 아티팩트 경로 검증: 번들 디렉토리 안의 상대경로여야(자기완결성) + 파일 존재.
    // ROM(rom_path)은 sha1으로 식별하며 번들 밖 원본을 가리킬 수 있어 이 제약에서 제외한다.
    for slice in &raw.slices {
        for artifact in &slice.artifacts {
            let rel = artifact_path(artifact);
            if !is_inside_bundle(rel) {
                return Err(FinalizeError::ArtifactOutsideBundle(
                    Path::new(rel).to_path_buf(),
                ));
            }
            validate_artifact(bundle_dir, rel)?;
        }
    }
    if let Some(movie) = &raw.input_movie {
        if !is_inside_bundle(movie) {
            return Err(FinalizeError::ArtifactOutsideBundle(
                Path::new(movie).to_path_buf(),
            ));
        }
        validate_artifact(bundle_dir, movie)?;
    }

    let manifest = Manifest {
        format_version: raw.format_version,
        platform: raw.platform,
        rom: RomId {
            sha1,
            path_hint: Some(path_hint),
        },
        adapter: raw.adapter,
        emulator: raw.emulator,
        trigger: raw.trigger,
        ring_policy: raw.ring_policy,
        slices: raw.slices,
        input_movie: raw.input_movie,
    };

    // manifest.json 쓰기 (우리 타입 직렬화는 실패하지 않음)
    let json = serde_json::to_string_pretty(&manifest).expect("매니페스트 직렬화");
    crate::path_safety::atomic_write_file(&bundle_dir.join("manifest.json"), json.as_bytes())?;

    // note.md 템플릿 — 이미 있으면 덮어쓰지 않음(사람 내용 보존)
    let note_path = bundle_dir.join("note.md");
    if fs::symlink_metadata(&note_path).is_err() {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut note = options.open(&note_path)?;
        note.write_all(NOTE_TEMPLATE.as_bytes())?;
        note.sync_all()?;
    }

    Ok(manifest)
}

/// 경로가 번들 디렉토리 내부의 상대경로인지(절대경로·`..` 탈출이 없는지).
fn is_inside_bundle(p: &str) -> bool {
    use std::path::Component;
    let path = Path::new(p);
    !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn resolve(base: &Path, p: &str) -> std::path::PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn validate_artifact(bundle_dir: &Path, relative: &str) -> Result<(), FinalizeError> {
    match crate::path_safety::regular_member_path(bundle_dir, relative) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(
            FinalizeError::ArtifactMissing(resolve(bundle_dir, relative)),
        ),
        Err(_) => Err(FinalizeError::UnsafeArtifact(resolve(bundle_dir, relative))),
    }
}

fn artifact_path(a: &Artifact) -> &str {
    match a {
        Artifact::Savestate { path } => path,
        Artifact::Screenshot { path } => path,
        Artifact::MemoryRegion { path, .. } => path,
    }
}
