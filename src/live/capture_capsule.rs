use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::continuity::LinkRecord;
use super::runtime::{
    capture_process, control_session_key, process_state, ProcessIdentity, ProcessState,
    RuntimeStore, MAX_CAPSULE_FILE_BYTES,
};
use crate::bundle::publish::VerifiedPublishedRecording;
use crate::bundle::recording_manifest::{
    CleanupFacts, CleanupState, EventStopFacts, ExecutionOutcome, FinalExecutionState, Integrity,
    OperationOutcome, PublicationOutcome, RecordingCounters,
};

const CAPTURE_CAPSULE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureCapsule {
    pub schema_version: u32,
    pub capture_id: String,
    pub launch_id: String,
    pub request_digest_sha256: String,
    pub capability_revision: String,
    pub output_root: String,
    pub destination_path: String,
    pub staging_path: String,
    pub output_root_identity: FilesystemIdentity,
    pub staging_identity: FilesystemIdentity,
    pub lease: CaptureLeaseIdentity,
    pub state: CaptureState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<CaptureProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<CaptureTerminalSummary>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
pub struct CapturePreparation {
    pub capture_id: String,
    pub request_digest_sha256: String,
    pub capability_revision: String,
    pub output_root: PathBuf,
    pub destination_path: PathBuf,
    pub staging_path: PathBuf,
    pub lease: CaptureLeaseIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureLeaseIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_session_key: Option<String>,
    pub holder: ProcessIdentity,
}

impl CaptureLeaseIdentity {
    pub fn current() -> Self {
        Self {
            control_session_key: control_session_key(),
            holder: capture_process(std::process::id()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemIdentity {
    pub canonical_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
}

impl FilesystemIdentity {
    pub fn capture(path: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "capture-owned path is not a real directory: {}",
                    path.display()
                ),
            ));
        }
        let canonical_path = fs::canonicalize(path)?.display().to_string();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                canonical_path,
                device: Some(metadata.dev()),
                inode: Some(metadata.ino()),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                canonical_path,
                device: None,
                inode: None,
            })
        }
    }

    pub fn matches(&self, path: &Path) -> io::Result<bool> {
        let current = Self::capture(path)?;
        Ok(self.canonical_path == current.canonical_path
            && self.device.is_some()
            && self.device == current.device
            && self.inode.is_some()
            && self.inode == current.inode)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Prepared,
    Arming,
    Armed,
    Recording,
    Closing,
    FrozenReadout,
    Finalizing,
    Published,
    PublicationFailed,
}

impl CaptureState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::PublicationFailed)
    }

    fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Prepared, Self::Arming | Self::PublicationFailed)
                | (
                    Self::Arming,
                    Self::Armed | Self::Closing | Self::PublicationFailed
                )
                | (
                    Self::Armed,
                    Self::Recording | Self::Closing | Self::PublicationFailed
                )
                | (Self::Recording, Self::Closing | Self::PublicationFailed)
                | (
                    Self::Closing,
                    Self::FrozenReadout | Self::Finalizing | Self::PublicationFailed
                )
                | (
                    Self::FrozenReadout,
                    Self::Finalizing | Self::PublicationFailed
                )
                | (Self::Finalizing, Self::Published | Self::PublicationFailed)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureProgress {
    pub sequence: u64,
    pub frame: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    pub events: u64,
    pub bytes: u64,
    pub observed_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureTerminalSummary {
    pub operation_outcome: OperationOutcome,
    pub execution_outcome: ExecutionOutcome,
    pub integrity: Integrity,
    pub publication: PublicationOutcome,
    pub final_execution_state: FinalExecutionState,
    pub final_frame: u64,
    pub counters: RecordingCounters,
    pub cleanup: CleanupFacts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_event: Option<EventStopFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CaptureTerminalSummary {
    fn cleanup_safe(&self) -> bool {
        let safe = |state| {
            matches!(
                state,
                CleanupState::Released
                    | CleanupState::NotAcquired
                    | CleanupState::GenerationTerminated
            )
        };
        let resources_safe = safe(self.cleanup.hooks)
            && safe(self.cleanup.transient_input)
            && safe(self.cleanup.sink);
        let no_resources_acquired = self.cleanup.hooks == CleanupState::NotAcquired
            && self.cleanup.transient_input == CleanupState::NotAcquired
            && self.cleanup.sink == CleanupState::NotAcquired;
        resources_safe
            && (no_resources_acquired
                || matches!(
                    self.final_execution_state,
                    FinalExecutionState::Frozen | FinalExecutionState::Terminated
                ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureCapsuleError {
    #[error("capture capsule I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("runtime generation mismatch for capture: expected {expected}, got {actual:?}")]
    GenerationMismatch {
        expected: String,
        actual: Option<String>,
    },
    #[error("capture control lease does not match the exact current holder")]
    LeaseMismatch,
    #[error("capture identity or path is invalid: {0}")]
    Invalid(String),
    #[error("capture {capture_id} blocks a new capture in state {state:?}")]
    ActiveCapture {
        capture_id: String,
        state: CaptureState,
    },
    #[error("capture state transition {from:?} -> {to:?} is invalid")]
    InvalidTransition {
        from: CaptureState,
        to: CaptureState,
    },
    #[error("capture recovery is blocked: {0}")]
    RecoveryBlocked(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    AlreadyTerminal,
    Published,
    Quarantined,
}

#[derive(Debug, Clone)]
pub struct CaptureCapsuleRepository {
    store: RuntimeStore,
    port: u16,
    launch_id: String,
}

impl CaptureCapsuleRepository {
    pub fn new(store: RuntimeStore, port: u16, launch_id: impl Into<String>) -> Self {
        Self {
            store,
            port,
            launch_id: launch_id.into(),
        }
    }

    pub fn read(&self) -> Result<Option<CaptureCapsule>, CaptureCapsuleError> {
        Ok(self.store.read_capture_json(self.port, &self.launch_id)?)
    }

    pub fn create(
        &self,
        preparation: CapturePreparation,
    ) -> Result<CaptureCapsule, CaptureCapsuleError> {
        validate_digest("request_digest_sha256", &preparation.request_digest_sha256)?;
        validate_digest("capability_revision", &preparation.capability_revision)?;
        validate_capture_id(&preparation.capture_id)?;
        self.verify_generation_and_lease(&preparation.lease, true)?;

        let output_root = fs::canonicalize(&preparation.output_root)?;
        let destination_parent = preparation
            .destination_path
            .parent()
            .ok_or_else(|| CaptureCapsuleError::Invalid("destination path has no parent".into()))?;
        if fs::canonicalize(destination_parent)? != output_root
            || preparation
                .destination_path
                .file_name()
                .and_then(|v| v.to_str())
                != Some(preparation.capture_id.as_str())
        {
            return Err(CaptureCapsuleError::Invalid(
                "destination is not the generated child of output_root".into(),
            ));
        }
        let staging_parent = preparation
            .staging_path
            .parent()
            .ok_or_else(|| CaptureCapsuleError::Invalid("staging path has no parent".into()))?;
        let staging_name = preparation
            .staging_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if fs::canonicalize(staging_parent)? != output_root
            || !staging_name.starts_with(&format!(".{}.staging-", preparation.capture_id))
        {
            return Err(CaptureCapsuleError::Invalid(
                "staging is not the generated adjacent directory".into(),
            ));
        }
        if preparation.destination_path.exists() {
            return Err(CaptureCapsuleError::Invalid(
                "capture destination already exists".into(),
            ));
        }
        let output_root_identity = FilesystemIdentity::capture(&output_root)?;
        let staging_identity = FilesystemIdentity::capture(&preparation.staging_path)?;
        let destination_path = output_root
            .join(&preparation.capture_id)
            .display()
            .to_string();
        let now = super::runtime::now_unix_ms();
        let capsule = CaptureCapsule {
            schema_version: CAPTURE_CAPSULE_SCHEMA_VERSION,
            capture_id: preparation.capture_id,
            launch_id: self.launch_id.clone(),
            request_digest_sha256: preparation.request_digest_sha256,
            capability_revision: preparation.capability_revision,
            output_root: output_root.display().to_string(),
            destination_path,
            staging_path: staging_identity.canonical_path.clone(),
            output_root_identity,
            staging_identity,
            lease: preparation.lease,
            state: CaptureState::Prepared,
            progress: None,
            terminal: None,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        };
        capsule.validate_size()?;
        let next = capsule.clone();
        match self.store.update_capture_json(
            self.port,
            &self.launch_id,
            move |current: Option<CaptureCapsule>| {
                if let Some(current) = current {
                    if current.generation_mutation_blocker().is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!(
                                "capture {} blocks a new capture in {:?}",
                                current.capture_id, current.state
                            ),
                        ));
                    }
                }
                Ok(next)
            },
        ) {
            Ok(created) => Ok(created),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let current = self.read()?.ok_or_else(|| {
                    CaptureCapsuleError::Invalid(
                        "active capture disappeared during conflict reporting".into(),
                    )
                })?;
                Err(CaptureCapsuleError::ActiveCapture {
                    capture_id: current.capture_id,
                    state: current.state,
                })
            }
            Err(error) => Err(CaptureCapsuleError::Io(error)),
        }
    }

    pub fn transition(
        &self,
        capture_id: &str,
        expected: CaptureState,
        next: CaptureState,
        terminal: Option<CaptureTerminalSummary>,
    ) -> Result<CaptureCapsule, CaptureCapsuleError> {
        let observed = self
            .read()?
            .ok_or_else(|| CaptureCapsuleError::Invalid("capture capsule is missing".into()))?;
        if observed.capture_id != capture_id || observed.state != expected {
            return Err(CaptureCapsuleError::Invalid(
                "capture identity or expected state changed".into(),
            ));
        }
        self.verify_generation_and_lease(&observed.lease, false)?;
        if !expected.permits(next) {
            return Err(CaptureCapsuleError::InvalidTransition {
                from: expected,
                to: next,
            });
        }
        if next.is_terminal() != terminal.is_some() {
            return Err(CaptureCapsuleError::Invalid(
                "terminal summary must exist exactly for a terminal state".into(),
            ));
        }
        let capture_id = capture_id.to_string();
        let launch_id = self.launch_id.clone();
        let updated = self.store.update_capture_json(
            self.port,
            &self.launch_id,
            move |current: Option<CaptureCapsule>| {
                let mut current = current.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "capture capsule is missing")
                })?;
                if current.capture_id != capture_id
                    || current.launch_id != launch_id
                    || current.state != expected
                {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "capture identity or state changed",
                    ));
                }
                current.state = next;
                current.terminal = terminal;
                current.updated_at_unix_ms = super::runtime::now_unix_ms();
                current.validate_size().map_err(to_io)?;
                Ok(current)
            },
        )?;
        Ok(updated)
    }

    pub fn update_progress(
        &self,
        capture_id: &str,
        progress: CaptureProgress,
    ) -> Result<CaptureCapsule, CaptureCapsuleError> {
        let observed = self
            .read()?
            .ok_or_else(|| CaptureCapsuleError::Invalid("capture capsule is missing".into()))?;
        if observed.capture_id != capture_id
            || !matches!(
                observed.state,
                CaptureState::Armed | CaptureState::Recording
            )
        {
            return Err(CaptureCapsuleError::Invalid(
                "capture is not the exact active recording".into(),
            ));
        }
        self.verify_generation_and_lease(&observed.lease, false)?;
        let capture_id = capture_id.to_string();
        let updated = self.store.update_capture_json(
            self.port,
            &self.launch_id,
            move |current: Option<CaptureCapsule>| {
                let mut current = current.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "capture capsule is missing")
                })?;
                if current.capture_id != capture_id
                    || !matches!(current.state, CaptureState::Armed | CaptureState::Recording)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "capture is not the active recording",
                    ));
                }
                if current.progress.as_ref().is_some_and(|previous| {
                    progress.sequence <= previous.sequence
                        || progress.frame < previous.frame
                        || matches!(
                            (previous.frames, progress.frames),
                            (Some(previous), Some(current)) if current < previous
                        )
                        || previous.frames.is_some() && progress.frames.is_none()
                        || progress.events < previous.events
                        || progress.bytes < previous.bytes
                }) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "capture progress regressed",
                    ));
                }
                current.progress = Some(progress);
                current.updated_at_unix_ms = super::runtime::now_unix_ms();
                current.validate_size().map_err(to_io)?;
                Ok(current)
            },
        )?;
        Ok(updated)
    }

    pub fn reconcile(
        &self,
        current_lease: &CaptureLeaseIdentity,
        verified_bundle: Option<&VerifiedPublishedRecording>,
        adapter_terminal: Option<CaptureTerminalSummary>,
    ) -> Result<ReconcileOutcome, CaptureCapsuleError> {
        let capsule = self.read()?.ok_or_else(|| {
            CaptureCapsuleError::RecoveryBlocked("capture capsule is missing".into())
        })?;
        if capsule.state.is_terminal() {
            return Ok(ReconcileOutcome::AlreadyTerminal);
        }
        if process_state(&capsule.lease.holder) != ProcessState::Exited {
            return Err(CaptureCapsuleError::RecoveryBlocked(
                "former capture lease holder exit is not proven".into(),
            ));
        }
        self.verify_generation_and_lease(current_lease, false)?;

        if let Some(bundle) = verified_bundle {
            if bundle.manifest.capture_id != capsule.capture_id
                || bundle.manifest.request_digest_sha256 != capsule.request_digest_sha256
                || bundle.manifest.runtime.launch_id != capsule.launch_id
                || bundle.manifest.runtime.capability_revision != capsule.capability_revision
                || bundle.bundle_path.as_path() != Path::new(&capsule.destination_path)
            {
                return Err(CaptureCapsuleError::RecoveryBlocked(
                    "published bundle identity does not match the abandoned capture".into(),
                ));
            }
            let terminal = terminal_from_bundle(bundle);
            self.force_terminal(&capsule, CaptureState::Published, terminal)?;
            return Ok(ReconcileOutcome::Published);
        }

        if Path::new(&capsule.destination_path).exists() {
            return Err(CaptureCapsuleError::RecoveryBlocked(
                "an unverified destination exists".into(),
            ));
        }
        let generation_terminated = self
            .store
            .read_current(self.port)?
            .is_some_and(|current| current.process_state() == ProcessState::Exited);
        let mut terminal = match adapter_terminal {
            Some(terminal) if terminal.cleanup_safe() => terminal,
            _ if generation_terminated => generation_terminated_summary(),
            _ => {
                return Err(CaptureCapsuleError::RecoveryBlocked(
                    "adapter cleanup and generation termination are both unproven".into(),
                ))
            }
        };
        // Without an independently verified Core bundle, adapter status can establish cleanup and
        // execution observations but cannot construct complete evidence integrity.
        terminal.integrity = Integrity::Unverifiable;
        let staging = Path::new(&capsule.staging_path);
        if staging.exists() {
            if !capsule.staging_identity.matches(staging)? {
                return Err(CaptureCapsuleError::RecoveryBlocked(
                    "staging filesystem identity changed".into(),
                ));
            }
            let root = Path::new(&capsule.output_root);
            if !capsule.output_root_identity.matches(root)? {
                return Err(CaptureCapsuleError::RecoveryBlocked(
                    "output root filesystem identity changed".into(),
                ));
            }
            let quarantine = root.join(format!(
                ".{}.invalid-{}",
                capsule.capture_id,
                ulid::Ulid::generate().to_string().to_ascii_lowercase()
            ));
            fs::rename(staging, &quarantine)?;
            terminal.reason = Some(format!(
                "abandoned staging quarantined at {}",
                quarantine.display()
            ));
        }
        terminal.publication = PublicationOutcome::Failed;
        self.force_terminal(&capsule, CaptureState::PublicationFailed, terminal)?;
        Ok(ReconcileOutcome::Quarantined)
    }

    fn force_terminal(
        &self,
        capsule: &CaptureCapsule,
        state: CaptureState,
        terminal: CaptureTerminalSummary,
    ) -> Result<CaptureCapsule, CaptureCapsuleError> {
        let capture_id = capsule.capture_id.clone();
        let launch_id = capsule.launch_id.clone();
        Ok(self.store.update_capture_json(
            self.port,
            &self.launch_id,
            move |current: Option<CaptureCapsule>| {
                let mut current = current.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "capture capsule is missing")
                })?;
                if current.capture_id != capture_id
                    || current.launch_id != launch_id
                    || current.state.is_terminal()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "capture changed during reconciliation",
                    ));
                }
                current.state = state;
                current.terminal = Some(terminal);
                current.updated_at_unix_ms = super::runtime::now_unix_ms();
                current.validate_size().map_err(to_io)?;
                Ok(current)
            },
        )?)
    }

    fn verify_generation_and_lease(
        &self,
        expected: &CaptureLeaseIdentity,
        require_alive: bool,
    ) -> Result<(), CaptureCapsuleError> {
        let current = self.store.read_current(self.port)?;
        let actual = current.as_ref().map(|current| current.launch_id.clone());
        let Some(current) = current.filter(|current| current.launch_id == self.launch_id) else {
            return Err(CaptureCapsuleError::GenerationMismatch {
                expected: self.launch_id.clone(),
                actual,
            });
        };
        if require_alive && current.process_state() != ProcessState::Alive {
            return Err(CaptureCapsuleError::RecoveryBlocked(
                "runtime generation is not provably alive".into(),
            ));
        }
        let link: Option<LinkRecord> = self.store.read_link_json(self.port, &self.launch_id)?;
        let exact = link.and_then(|record| record.lease).is_some_and(|lease| {
            lease.holder == expected.holder
                && lease.control_session_key == expected.control_session_key
        });
        if !exact {
            return Err(CaptureCapsuleError::LeaseMismatch);
        }
        Ok(())
    }
}

impl CaptureCapsule {
    /// A terminal publication result does not by itself make the emulator safe to mutate again.
    /// Pre-arm failures with no acquired resources are safe; every post-arm terminal must prove
    /// hook/input/sink cleanup and a frozen or terminated execution state.
    pub fn generation_mutation_blocker(&self) -> Option<String> {
        if !self.state.is_terminal() {
            return Some(format!("capture remains nonterminal in {:?}", self.state));
        }
        match self.terminal.as_ref() {
            Some(terminal) if terminal.cleanup_safe() => None,
            Some(_) => Some(format!(
                "capture is terminal in {:?}, but cleanup or final execution state is unverifiable",
                self.state
            )),
            None => Some(format!(
                "capture is terminal in {:?} without terminal cleanup facts",
                self.state
            )),
        }
    }

    fn validate_size(&self) -> Result<(), CaptureCapsuleError> {
        if self.schema_version != CAPTURE_CAPSULE_SCHEMA_VERSION
            || serde_json::to_vec(self)
                .map(|bytes| bytes.len() as u64 > MAX_CAPSULE_FILE_BYTES)
                .unwrap_or(true)
        {
            return Err(CaptureCapsuleError::Invalid(
                "capture capsule schema or size is invalid".into(),
            ));
        }
        Ok(())
    }
}

fn terminal_from_bundle(bundle: &VerifiedPublishedRecording) -> CaptureTerminalSummary {
    let manifest = &bundle.manifest;
    CaptureTerminalSummary {
        operation_outcome: manifest.terminal.operation_outcome,
        execution_outcome: manifest.terminal.execution_outcome,
        integrity: manifest.terminal.integrity,
        publication: manifest.terminal.publication,
        final_execution_state: manifest.terminal.final_execution_state,
        final_frame: manifest.terminal.final_frame,
        counters: manifest.counters.clone(),
        cleanup: manifest.cleanup.clone(),
        stop_event: manifest.terminal.stop_event.clone(),
        bundle_path: Some(bundle.bundle_path.display().to_string()),
        manifest_sha256: Some(bundle.manifest_sha256.clone()),
        reason: manifest.terminal.reason.clone(),
    }
}

fn generation_terminated_summary() -> CaptureTerminalSummary {
    CaptureTerminalSummary {
        operation_outcome: OperationOutcome::Failed,
        execution_outcome: ExecutionOutcome::EmulatorExited,
        integrity: Integrity::Unverifiable,
        publication: PublicationOutcome::Failed,
        final_execution_state: FinalExecutionState::Terminated,
        final_frame: 0,
        counters: RecordingCounters {
            frames: 0,
            events: 0,
            bytes: 0,
            dropped: 0,
        },
        cleanup: CleanupFacts {
            hooks: CleanupState::GenerationTerminated,
            transient_input: CleanupState::GenerationTerminated,
            sink: CleanupState::GenerationTerminated,
        },
        stop_event: None,
        bundle_path: None,
        manifest_sha256: None,
        reason: Some("capture owner exited with its emulator generation".into()),
    }
}

fn validate_digest(name: &str, value: &str) -> Result<(), CaptureCapsuleError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(CaptureCapsuleError::Invalid(format!(
            "{name} must be a SHA-256"
        )))
    }
}

fn validate_capture_id(value: &str) -> Result<(), CaptureCapsuleError> {
    if !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(CaptureCapsuleError::Invalid("invalid capture_id".into()))
    }
}

fn to_io(error: CaptureCapsuleError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
