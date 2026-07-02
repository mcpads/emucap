use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FinalizeError {
    #[error("_raw.json을 찾을 수 없음: {0}")]
    RawNotFound(PathBuf),
    #[error("_raw.json 파싱 실패: {0}")]
    RawParse(#[from] serde_json::Error),
    #[error("입출력 오류: {0}")]
    Io(#[from] std::io::Error),
    #[error("ROM 파일을 찾을 수 없음: {0}")]
    RomNotFound(PathBuf),
    #[error("아티팩트 파일 누락: {0}")]
    ArtifactMissing(PathBuf),
    #[error("아티팩트 경로가 번들 디렉토리를 벗어남(비-자기완결): {0}")]
    ArtifactOutsideBundle(PathBuf),
    #[error("번들에 슬라이스가 없음")]
    NoSlices,
    #[error("지원하지 않는 format_version: {0}")]
    UnsupportedFormatVersion(u32),
}
