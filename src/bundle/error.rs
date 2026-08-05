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

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("output_root must be an absolute existing directory: {0}")]
    InvalidOutputRoot(PathBuf),
    #[error("output_root must not be a symbolic link: {0}")]
    SymlinkOutputRoot(PathBuf),
    #[error("invalid capture_id: {0}")]
    InvalidCaptureId(String),
    #[error("capture destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("recording member is missing: {0}")]
    MemberMissing(PathBuf),
    #[error("recording member is not a regular owned file: {0}")]
    UnsafeMember(PathBuf),
    #[error("recording sink writer was already opened")]
    WriterAlreadyOpened,
    #[error("recording line must be one non-empty NDJSON record")]
    InvalidRecord,
    #[error("recording line exceeds limit {0}")]
    LineLimit(u64),
    #[error("recording event count exceeds limit {0}")]
    EventLimit(u64),
    #[error("recording byte count exceeds limit {0}")]
    ByteLimit(u64),
    #[error("recording identity is invalid: {0}")]
    InvalidIdentity(String),
    #[error("recording validation failed: {0}")]
    Validation(#[from] super::recording::RecordingValidationError),
    #[error("recording manifest serialization failed: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("recording publication I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("recording publication fault injected at {0}")]
    Injected(&'static str),
}

#[derive(Debug, thiserror::Error)]
pub enum PublishedBundleError {
    #[error("published recording manifest is missing or unsafe: {0}")]
    UnsafeManifest(PathBuf),
    #[error("published recording manifest exceeds the bounded size")]
    ManifestTooLarge,
    #[error("published recording manifest is invalid: {0}")]
    Manifest(#[from] super::manifest::ManifestDecodeError),
    #[error("expected a recording bundle, found legacy format")]
    LegacyFormat,
    #[error("published recording layout or identity is invalid: {0}")]
    Invalid(String),
    #[error("published recording validation failed: {0}")]
    Validation(#[from] super::recording::RecordingValidationError),
    #[error("published recording I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
