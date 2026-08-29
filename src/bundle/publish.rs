use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::error::{PublishError, PublishedBundleError};
use super::manifest::{parse_manifest, BundleManifest};
use super::recording::{
    validate_recording, ProducerTerminalReport, RecordingValidationInput, ValidatedRecording,
};
use super::recording_manifest::{
    EffectiveScope, EventOrder, InitialSnapshotRequest, InputMovieIdentity, MemberDescriptor,
    MemberRole, PublicationOutcome, RecordingCounters, RecordingManifest, RuntimeIdentity,
    StateArtifactIdentity, TerminalFacts, TerminalSnapshotRequest, TerminalStateRequest,
    RECORDING_FORMAT_VERSION,
};
use crate::event_contracts::EventContractRegistry;
use crate::input_movie::canonical_recording_movie;

const EVENTS_DIR: &str = "events";
const EVENTS_MEMBER: &str = "events/segment-000.ndjson";
const INPUT_MOVIE_MEMBER: &str = "input.movie";
const INITIAL_SNAPSHOTS_DIR: &str = "initial-snapshots";
const SNAPSHOTS_DIR: &str = "snapshots";
const TERMINAL_STATE_MEMBER: &str = "terminal-state.json";
const MANIFEST_MEMBER: &str = "manifest.json";

#[derive(Debug, Clone)]
pub struct RecordingBundleInput {
    pub capture_id: String,
    pub created_at_unix_ms: u64,
    pub request_digest_sha256: String,
    pub runtime: RuntimeIdentity,
    pub event_order: Option<EventOrder>,
    pub validation: RecordingValidationInput,
}

#[derive(Debug)]
pub struct PublishedRecording {
    pub bundle_path: PathBuf,
    pub manifest_sha256: String,
    pub manifest: RecordingManifest,
}

#[derive(Debug)]
pub struct VerifiedPublishedRecording {
    pub bundle_path: PathBuf,
    pub manifest_sha256: String,
    pub manifest: RecordingManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishFault {
    None,
    ManifestWrite,
    ManifestShortWrite,
    ManifestSync,
    Rename,
}

#[derive(Debug)]
pub struct RecordingStaging {
    output_root: PathBuf,
    staging_path: PathBuf,
    destination_path: PathBuf,
    capture_id: String,
    writer_opened: bool,
    input_movie_written: bool,
    initial_state_written: bool,
    initial_snapshot_labels_written: BTreeSet<String>,
    snapshot_labels_written: BTreeSet<String>,
    terminal_state_written: bool,
}

#[derive(Debug)]
pub struct BoundedEventWriter {
    pub(super) file: File,
    pub(super) max_events: u64,
    pub(super) max_bytes: u64,
    pub(super) max_line_bytes: u64,
    pub(super) events: u64,
    pub(super) bytes: u64,
}

pub(super) fn valid_hex_digest(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_capture_id(value: &str) -> bool {
    crate::path_safety::is_hyphenated_ascii_id(value, 96)
}

fn ensure_identity(input: &RecordingBundleInput) -> Result<(), PublishError> {
    if !valid_capture_id(&input.capture_id) {
        return Err(PublishError::InvalidCaptureId(input.capture_id.clone()));
    }
    if !valid_hex_digest(&input.request_digest_sha256, 32) {
        return Err(PublishError::InvalidIdentity(
            "request_digest_sha256 must be 64 hexadecimal characters".into(),
        ));
    }
    let runtime = &input.runtime;
    for (name, value) in [
        ("system", runtime.system.as_str()),
        ("adapter_id", runtime.adapter_id.as_str()),
        ("server_build", runtime.server_build.as_str()),
        ("adapter_build", runtime.adapter_build.as_str()),
        ("emulator_id", runtime.emulator_id.as_str()),
        ("emulator_build", runtime.emulator_build.as_str()),
        (
            "emulator_upstream_revision",
            runtime.emulator_upstream_revision.as_str(),
        ),
        ("launch_id", runtime.launch_id.as_str()),
    ] {
        if value.is_empty() {
            return Err(PublishError::InvalidIdentity(format!(
                "{name} must not be empty"
            )));
        }
    }
    if !valid_hex_digest(&runtime.emulator_patchset_sha256, 32) {
        return Err(PublishError::InvalidIdentity(
            "emulator_patchset_sha256 must be a SHA-256".into(),
        ));
    }
    if !valid_hex_digest(&runtime.capability_revision, 32) {
        return Err(PublishError::InvalidIdentity(
            "capability_revision must be a SHA-256".into(),
        ));
    }
    let content = &runtime.content;
    if content.sha1.is_none() && content.sha256.is_none() {
        return Err(PublishError::InvalidIdentity(
            "content requires sha1 or sha256".into(),
        ));
    }
    if content
        .sha1
        .as_deref()
        .is_some_and(|value| !valid_hex_digest(value, 20))
        || content
            .sha256
            .as_deref()
            .is_some_and(|value| !valid_hex_digest(value, 32))
    {
        return Err(PublishError::InvalidIdentity(
            "content digest has an invalid shape".into(),
        ));
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(super) fn regular_owned_member(path: &Path) -> Result<(), PublishError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PublishError::MemberMissing(path.to_path_buf())
        } else {
            PublishError::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PublishError::UnsafeMember(path.to_path_buf()));
    }
    Ok(())
}

impl RecordingStaging {
    pub fn prepare(output_root: &Path, capture_id: &str) -> Result<Self, PublishError> {
        if !output_root.is_absolute() {
            return Err(PublishError::InvalidOutputRoot(output_root.to_path_buf()));
        }
        let root_metadata = fs::symlink_metadata(output_root)
            .map_err(|_| PublishError::InvalidOutputRoot(output_root.to_path_buf()))?;
        if root_metadata.file_type().is_symlink() {
            return Err(PublishError::SymlinkOutputRoot(output_root.to_path_buf()));
        }
        if !root_metadata.is_dir() {
            return Err(PublishError::InvalidOutputRoot(output_root.to_path_buf()));
        }
        if !valid_capture_id(capture_id) {
            return Err(PublishError::InvalidCaptureId(capture_id.to_string()));
        }
        let output_root = fs::canonicalize(output_root)?;
        let destination_path = output_root.join(capture_id);
        if fs::symlink_metadata(&destination_path).is_ok() {
            return Err(PublishError::DestinationExists(destination_path));
        }
        let staging_path = output_root.join(format!(
            ".{capture_id}.staging-{}",
            ulid::Ulid::generate().to_string().to_ascii_lowercase()
        ));
        create_private_dir(&staging_path)?;
        let prepared = (|| -> Result<(), std::io::Error> {
            create_private_dir(&staging_path.join(EVENTS_DIR))?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(staging_path.join(EVENTS_MEMBER))?;
            Ok(())
        })();
        if let Err(error) = prepared {
            let _ = fs::remove_dir_all(&staging_path);
            return Err(PublishError::Io(error));
        }
        Ok(Self {
            output_root,
            staging_path,
            destination_path,
            capture_id: capture_id.to_string(),
            writer_opened: false,
            input_movie_written: false,
            initial_state_written: false,
            initial_snapshot_labels_written: BTreeSet::new(),
            snapshot_labels_written: BTreeSet::new(),
            terminal_state_written: false,
        })
    }

    pub fn staging_path(&self) -> &Path {
        &self.staging_path
    }

    pub fn destination_path(&self) -> &Path {
        &self.destination_path
    }

    pub fn events_path(&self) -> PathBuf {
        self.staging_path.join(EVENTS_MEMBER)
    }

    pub fn write_input_movie(
        &mut self,
        bytes: &[u8],
        identity: &InputMovieIdentity,
    ) -> Result<PathBuf, PublishError> {
        if self.input_movie_written
            || identity.bytes != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
            || identity.sha256 != hex::encode(Sha256::digest(bytes))
        {
            return Err(PublishError::InvalidIdentity(
                "input movie identity or staging state mismatch".into(),
            ));
        }
        let path = self.staging_path.join(INPUT_MOVIE_MEMBER);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o400);
        }
        let mut file = options.open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions)?;
        self.input_movie_written = true;
        Ok(path)
    }

    pub fn write_initial_state(
        &mut self,
        bytes: &[u8],
        identity: &StateArtifactIdentity,
    ) -> Result<PathBuf, PublishError> {
        let path = super::publish_state::write_initial_state(
            &self.staging_path,
            self.initial_state_written,
            bytes,
            identity,
        )?;
        self.initial_state_written = true;
        Ok(path)
    }

    pub fn open_event_writer(
        &mut self,
        max_events: u64,
        max_bytes: u64,
        max_line_bytes: u64,
    ) -> Result<BoundedEventWriter, PublishError> {
        if self.writer_opened {
            return Err(PublishError::WriterAlreadyOpened);
        }
        let path = self.events_path();
        regular_owned_member(&path)?;
        if fs::metadata(&path)?.len() != 0 {
            return Err(PublishError::UnsafeMember(path));
        }
        let file = OpenOptions::new().write(true).open(path)?;
        self.writer_opened = true;
        Ok(BoundedEventWriter {
            file,
            max_events,
            max_bytes,
            max_line_bytes,
            events: 0,
            bytes: 0,
        })
    }

    pub fn write_terminal_snapshot(
        &mut self,
        request: &TerminalSnapshotRequest,
        bytes: &[u8],
    ) -> Result<PathBuf, PublishError> {
        if request.length != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
            || !valid_snapshot_label(&request.label)
            || !self.snapshot_labels_written.insert(request.label.clone())
        {
            return Err(PublishError::InvalidIdentity(
                "terminal snapshot request or staging state mismatch".into(),
            ));
        }
        let directory = self.staging_path.join(SNAPSHOTS_DIR);
        if self.snapshot_labels_written.len() == 1 {
            create_private_dir(&directory)?;
        } else if fs::symlink_metadata(&directory)
            .map(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
            .unwrap_or(true)
        {
            return Err(PublishError::UnsafeMember(directory));
        }
        let path = terminal_snapshot_path(&self.staging_path, &request.label);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o400);
        }
        let mut file = options.open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions)?;
        Ok(path)
    }

    pub fn write_initial_snapshot(
        &mut self,
        request: &InitialSnapshotRequest,
        bytes: &[u8],
    ) -> Result<PathBuf, PublishError> {
        if request.length != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
            || !valid_snapshot_label(&request.label)
            || !self
                .initial_snapshot_labels_written
                .insert(request.label.clone())
        {
            return Err(PublishError::InvalidIdentity(
                "initial snapshot request or staging state mismatch".into(),
            ));
        }
        let directory = self.staging_path.join(INITIAL_SNAPSHOTS_DIR);
        if self.initial_snapshot_labels_written.len() == 1 {
            create_private_dir(&directory)?;
        } else if fs::symlink_metadata(&directory)
            .map(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
            .unwrap_or(true)
        {
            return Err(PublishError::UnsafeMember(directory));
        }
        let path = initial_snapshot_path(&self.staging_path, &request.label);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o400);
        }
        let mut file = options.open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions)?;
        Ok(path)
    }

    pub(crate) fn write_initial_snapshot_prefix(
        &mut self,
        request: &InitialSnapshotRequest,
        bytes: &[u8],
    ) -> Result<PathBuf, PublishError> {
        if bytes.is_empty()
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) >= request.length
            || !valid_snapshot_label(&request.label)
        {
            return Err(PublishError::InvalidIdentity(
                "initial snapshot prefix evidence is invalid".into(),
            ));
        }
        let directory = self.staging_path.join(INITIAL_SNAPSHOTS_DIR);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(PublishError::UnsafeMember(directory));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_private_dir(&directory)?;
            }
            Err(error) => return Err(PublishError::Io(error)),
        }
        let path = directory.join(format!("{}.partial.bin", request.label));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o400);
        }
        let mut file = options.open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(path)
    }

    pub fn write_terminal_state(
        &mut self,
        request: &TerminalStateRequest,
        bytes: &[u8],
    ) -> Result<PathBuf, PublishError> {
        if self.terminal_state_written
            || !valid_hex_digest(&request.contract_sha256, 32)
            || bytes.is_empty()
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                > crate::live::recording_capability::CORE_MAX_TERMINAL_STATE_BYTES
        {
            return Err(PublishError::InvalidIdentity(
                "terminal state request or staging state mismatch".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| PublishError::InvalidIdentity(error.to_string()))?;
        if !value.is_object() || crate::track::observe::canonical_json(&value).as_bytes() != bytes {
            return Err(PublishError::InvalidIdentity(
                "terminal state member is not canonical object JSON".into(),
            ));
        }
        let path = self.staging_path.join(TERMINAL_STATE_MEMBER);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o400);
        }
        let mut file = options.open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions)?;
        self.terminal_state_written = true;
        Ok(path)
    }

    pub fn quarantine(self) -> Result<PathBuf, PublishError> {
        let quarantine = self.output_root.join(format!(
            ".{}.invalid-{}",
            self.capture_id,
            ulid::Ulid::generate().to_string().to_ascii_lowercase()
        ));
        fs::rename(&self.staging_path, &quarantine)?;
        File::open(&self.output_root)?.sync_all()?;
        Ok(quarantine)
    }

    pub fn discard(self) -> Result<(), PublishError> {
        fs::remove_dir_all(&self.staging_path)?;
        File::open(&self.output_root)?.sync_all()?;
        Ok(())
    }

    pub fn publish(
        self,
        registry: &EventContractRegistry,
        input: RecordingBundleInput,
    ) -> Result<PublishedRecording, PublishError> {
        self.publish_with_fault(registry, input, PublishFault::None)
    }

    pub(crate) fn publish_with_fault(
        self,
        registry: &EventContractRegistry,
        input: RecordingBundleInput,
        fault: PublishFault,
    ) -> Result<PublishedRecording, PublishError> {
        let mut staging = self;
        let result = staging.try_publish(registry, input, fault);
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                let _ = staging.quarantine();
                Err(error)
            }
        }
    }

    fn try_publish(
        &mut self,
        registry: &EventContractRegistry,
        input: RecordingBundleInput,
        fault: PublishFault,
    ) -> Result<PublishedRecording, PublishError> {
        if input.capture_id != self.capture_id {
            return Err(PublishError::InvalidIdentity(
                "capture_id does not match staging".into(),
            ));
        }
        ensure_identity(&input)?;
        let events_path = self.events_path();
        regular_owned_member(&events_path)?;
        validate_input_movie_member(&self.staging_path, &input.validation.request)?;
        super::publish_state::validate_initial_state_member(
            &self.staging_path,
            &input.validation.request,
            &input.runtime,
        )?;
        let mut snapshot_members =
            validate_initial_snapshot_members(&self.staging_path, &input.validation.request)?;
        snapshot_members.extend(validate_terminal_snapshot_members(
            &self.staging_path,
            &input.validation.request,
        )?);
        if let Some(member) =
            validate_terminal_state_member(&self.staging_path, &input.validation.request)?
        {
            snapshot_members.push(member);
        }
        if fs::symlink_metadata(&self.destination_path).is_ok() {
            let destination = self.destination_path.clone();
            return Err(PublishError::DestinationExists(destination));
        }
        let validated = match validate_recording(&events_path, registry, input.validation.clone()) {
            Ok(value) => value,
            Err(error) => return Err(PublishError::Validation(error)),
        };
        let manifest = build_manifest(&input, registry, &validated, snapshot_members)?;
        let mut manifest_bytes = match serde_json::to_vec_pretty(&manifest) {
            Ok(value) => value,
            Err(error) => return Err(PublishError::Manifest(error)),
        };
        manifest_bytes.push(b'\n');
        let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
        let manifest_path = self.staging_path.join(MANIFEST_MEMBER);
        let mut manifest_file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest_path)
        {
            Ok(value) => value,
            Err(error) => return Err(PublishError::Io(error)),
        };
        if fault == PublishFault::ManifestWrite {
            return Err(PublishError::Injected("manifest_write"));
        }
        if fault == PublishFault::ManifestShortWrite {
            let _ = manifest_file.write(&manifest_bytes[..manifest_bytes.len() / 2]);
            return Err(PublishError::Injected("manifest_short_write"));
        }
        if let Err(error) = manifest_file.write_all(&manifest_bytes) {
            return Err(PublishError::Io(error));
        }
        if fault == PublishFault::ManifestSync {
            return Err(PublishError::Injected("manifest_sync"));
        }
        if let Err(error) = manifest_file.sync_all() {
            return Err(PublishError::Io(error));
        }
        drop(manifest_file);
        if let Err(error) = File::open(&self.staging_path).and_then(|file| file.sync_all()) {
            return Err(PublishError::Io(error));
        }
        if fs::symlink_metadata(&self.destination_path).is_ok() {
            let destination = self.destination_path.clone();
            return Err(PublishError::DestinationExists(destination));
        }
        if fault == PublishFault::Rename {
            return Err(PublishError::Injected("rename"));
        }
        if let Err(error) = fs::rename(&self.staging_path, &self.destination_path) {
            return Err(PublishError::Io(error));
        }
        self.staging_path = self.destination_path.clone();
        if let Err(error) = File::open(&self.output_root).and_then(|file| file.sync_all()) {
            return Err(PublishError::Io(error));
        }
        Ok(PublishedRecording {
            bundle_path: self.destination_path.clone(),
            manifest_sha256,
            manifest,
        })
    }
}

fn build_manifest(
    input: &RecordingBundleInput,
    registry: &EventContractRegistry,
    validated: &ValidatedRecording,
    snapshot_members: Vec<MemberDescriptor>,
) -> Result<RecordingManifest, PublishError> {
    let mut clock_domains = BTreeSet::new();
    for identity in &input.validation.request.event_classes {
        clock_domains.insert(
            registry
                .validate_identity(identity)
                .map_err(|error| PublishError::InvalidIdentity(error.to_string()))?
                .clock_domain
                .clone(),
        );
    }
    let producer = validated.terminal();
    let stream = validated.stream();
    let mut members = vec![MemberDescriptor {
        role: MemberRole::Events,
        path: EVENTS_MEMBER.into(),
        sha256: stream.sha256.clone(),
        bytes: stream.physical_bytes,
        records: Some(stream.records),
    }];
    if let Some(identity) = &input.validation.request.input_movie {
        members.push(MemberDescriptor {
            role: MemberRole::InputMovie,
            path: INPUT_MOVIE_MEMBER.into(),
            sha256: identity.sha256.clone(),
            bytes: identity.bytes,
            records: Some(identity.frames),
        });
    }
    if let Some(receipt) = &input.validation.request.initial_state {
        members.push(super::publish_state::member_descriptor(receipt));
    }
    members.extend(snapshot_members);
    Ok(RecordingManifest {
        format_version: RECORDING_FORMAT_VERSION,
        capture_id: input.capture_id.clone(),
        capture_kind: super::recording_manifest::CaptureKind::RecordWindow,
        created_at_unix_ms: input.created_at_unix_ms,
        request_digest_sha256: input.request_digest_sha256.clone(),
        runtime: input.runtime.clone(),
        request: input.validation.request.clone(),
        scope: EffectiveScope {
            origin: input.validation.origin,
            f_origin: producer.f_origin,
            f_start: input.validation.f_start,
            f_end: input.validation.f_end,
            clock_domains: clock_domains.into_iter().collect(),
            event_order: input.event_order,
            observation_start: input.validation.observation_start.clone(),
        },
        terminal: TerminalFacts {
            operation_outcome: producer.operation_outcome,
            execution_outcome: producer.execution_outcome,
            integrity: validated.integrity(),
            publication: PublicationOutcome::Published,
            final_execution_state: producer.final_execution_state,
            final_frame: producer.final_frame,
            stop_event: producer.stop_event.clone(),
            reason: producer.reason.clone(),
            event_classes: producer.event_classes.clone(),
        },
        counters: RecordingCounters {
            frames: producer.counters.frames,
            events: stream.records,
            bytes: stream.physical_bytes,
            dropped: producer.counters.dropped,
        },
        loss: producer.loss.clone(),
        cleanup: producer.cleanup.clone(),
        members,
    })
}

fn valid_snapshot_label(label: &str) -> bool {
    crate::path_safety::is_hyphenated_ascii_id(label, 64)
}

pub(super) fn valid_profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn terminal_snapshot_path(root: &Path, label: &str) -> PathBuf {
    root.join(SNAPSHOTS_DIR).join(format!("{label}.bin"))
}

fn initial_snapshot_path(root: &Path, label: &str) -> PathBuf {
    root.join(INITIAL_SNAPSHOTS_DIR)
        .join(format!("{label}.bin"))
}

fn validate_initial_snapshot_members(
    bundle_path: &Path,
    request: &super::recording_manifest::RecordingRequest,
) -> Result<Vec<MemberDescriptor>, PublishError> {
    let directory = bundle_path.join(INITIAL_SNAPSHOTS_DIR);
    if request.initial_snapshots.is_empty() {
        if fs::symlink_metadata(&directory).is_ok() {
            return Err(PublishError::InvalidIdentity(
                "unexpected initial snapshot directory".into(),
            ));
        }
        return Ok(Vec::new());
    }
    if request.initial_snapshots.len()
        > usize::try_from(crate::live::recording_capability::CORE_MAX_INITIAL_SNAPSHOT_MEMBERS)
            .unwrap_or(usize::MAX)
    {
        return Err(PublishError::InvalidIdentity(
            "initial snapshot member count exceeds the Core bound".into(),
        ));
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PublishError::UnsafeMember(directory));
    }
    let mut labels = BTreeSet::new();
    let mut expected_names = BTreeSet::new();
    let mut members = Vec::with_capacity(request.initial_snapshots.len());
    let mut total_bytes = 0u64;
    for snapshot in &request.initial_snapshots {
        if !valid_snapshot_label(&snapshot.label)
            || snapshot.memory_type.is_empty()
            || snapshot.memory_type.len() > 128
            || snapshot.length == 0
            || snapshot.length
                > crate::live::recording_capability::CORE_MAX_INITIAL_SNAPSHOT_MEMBER_BYTES
            || snapshot.address.checked_add(snapshot.length).is_none()
            || !labels.insert(snapshot.label.as_str())
        {
            return Err(PublishError::InvalidIdentity(
                "initial snapshot request identity is invalid".into(),
            ));
        }
        total_bytes = total_bytes.checked_add(snapshot.length).ok_or_else(|| {
            PublishError::InvalidIdentity("initial snapshot byte total overflow".into())
        })?;
        if total_bytes > crate::live::recording_capability::CORE_MAX_INITIAL_SNAPSHOT_TOTAL_BYTES {
            return Err(PublishError::InvalidIdentity(
                "initial snapshot byte total exceeds the Core bound".into(),
            ));
        }
        let file_name = format!("{}.bin", snapshot.label);
        expected_names.insert(file_name.clone());
        let path = initial_snapshot_path(bundle_path, &snapshot.label);
        regular_owned_member(&path)?;
        let bytes = fs::read(&path)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != snapshot.length {
            return Err(PublishError::InvalidIdentity(format!(
                "initial snapshot {} size mismatch",
                snapshot.label
            )));
        }
        members.push(MemberDescriptor {
            role: MemberRole::InitialSnapshot,
            path: format!("{INITIAL_SNAPSHOTS_DIR}/{file_name}"),
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes: snapshot.length,
            records: None,
        });
    }
    let actual_names = fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual_names != expected_names {
        return Err(PublishError::InvalidIdentity(
            "initial snapshot directory contains missing or extra members".into(),
        ));
    }
    Ok(members)
}

fn validate_terminal_snapshot_members(
    bundle_path: &Path,
    request: &super::recording_manifest::RecordingRequest,
) -> Result<Vec<MemberDescriptor>, PublishError> {
    let directory = bundle_path.join(SNAPSHOTS_DIR);
    if request.terminal_snapshots.is_empty() {
        if fs::symlink_metadata(&directory).is_ok() {
            return Err(PublishError::InvalidIdentity(
                "unexpected terminal snapshot directory".into(),
            ));
        }
        return Ok(Vec::new());
    }
    if request.terminal_snapshots.len()
        > usize::try_from(crate::live::recording_capability::CORE_MAX_TERMINAL_SNAPSHOT_MEMBERS)
            .unwrap_or(usize::MAX)
    {
        return Err(PublishError::InvalidIdentity(
            "terminal snapshot member count exceeds the Core bound".into(),
        ));
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PublishError::UnsafeMember(directory));
    }
    let mut labels = BTreeSet::new();
    let mut expected_names = BTreeSet::new();
    let mut members = Vec::with_capacity(request.terminal_snapshots.len());
    let mut total_bytes = 0u64;
    for snapshot in &request.terminal_snapshots {
        if !valid_snapshot_label(&snapshot.label)
            || snapshot.memory_type.is_empty()
            || snapshot.memory_type.len() > 128
            || snapshot.length == 0
            || snapshot.length
                > crate::live::recording_capability::CORE_MAX_TERMINAL_SNAPSHOT_MEMBER_BYTES
            || snapshot.address.checked_add(snapshot.length).is_none()
            || !labels.insert(snapshot.label.as_str())
        {
            return Err(PublishError::InvalidIdentity(
                "terminal snapshot request identity is invalid".into(),
            ));
        }
        total_bytes = total_bytes.checked_add(snapshot.length).ok_or_else(|| {
            PublishError::InvalidIdentity("terminal snapshot byte total overflow".into())
        })?;
        if total_bytes > crate::live::recording_capability::CORE_MAX_TERMINAL_SNAPSHOT_TOTAL_BYTES {
            return Err(PublishError::InvalidIdentity(
                "terminal snapshot byte total exceeds the Core bound".into(),
            ));
        }
        let file_name = format!("{}.bin", snapshot.label);
        expected_names.insert(file_name.clone());
        let path = terminal_snapshot_path(bundle_path, &snapshot.label);
        regular_owned_member(&path)?;
        let bytes = fs::read(&path)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != snapshot.length {
            return Err(PublishError::InvalidIdentity(format!(
                "terminal snapshot {} size mismatch",
                snapshot.label
            )));
        }
        members.push(MemberDescriptor {
            role: MemberRole::TerminalSnapshot,
            path: format!("{SNAPSHOTS_DIR}/{file_name}"),
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes: snapshot.length,
            records: None,
        });
    }
    let actual_names = fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual_names != expected_names {
        return Err(PublishError::InvalidIdentity(
            "terminal snapshot directory contains missing or extra members".into(),
        ));
    }
    Ok(members)
}

fn validate_terminal_state_member(
    bundle_path: &Path,
    request: &super::recording_manifest::RecordingRequest,
) -> Result<Option<MemberDescriptor>, PublishError> {
    let path = bundle_path.join(TERMINAL_STATE_MEMBER);
    let Some(identity) = &request.terminal_state else {
        if fs::symlink_metadata(&path).is_ok() {
            return Err(PublishError::InvalidIdentity(
                "unexpected terminal state member".into(),
            ));
        }
        return Ok(None);
    };
    if !valid_profile_id(&identity.profile) || !valid_hex_digest(&identity.contract_sha256, 32) {
        return Err(PublishError::InvalidIdentity(
            "terminal state request identity is invalid".into(),
        ));
    }
    regular_owned_member(&path)?;
    let bytes = fs::read(&path)?;
    if bytes.is_empty()
        || u64::try_from(bytes.len()).unwrap_or(u64::MAX)
            > crate::live::recording_capability::CORE_MAX_TERMINAL_STATE_BYTES
    {
        return Err(PublishError::InvalidIdentity(
            "terminal state member exceeds the Core bound".into(),
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| PublishError::InvalidIdentity(error.to_string()))?;
    if !value.is_object() || crate::track::observe::canonical_json(&value).as_bytes() != bytes {
        return Err(PublishError::InvalidIdentity(
            "terminal state member is not canonical object JSON".into(),
        ));
    }
    Ok(Some(MemberDescriptor {
        role: MemberRole::TerminalState,
        path: TERMINAL_STATE_MEMBER.into(),
        sha256: hex::encode(Sha256::digest(&bytes)),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        records: None,
    }))
}

fn validate_input_movie_member(
    bundle_path: &Path,
    request: &super::recording_manifest::RecordingRequest,
) -> Result<(), PublishError> {
    let path = bundle_path.join(INPUT_MOVIE_MEMBER);
    let Some(identity) = &request.input_movie else {
        if fs::symlink_metadata(&path).is_ok() {
            return Err(PublishError::InvalidIdentity(
                "unexpected input movie member".into(),
            ));
        }
        return Ok(());
    };
    if identity.format != crate::input_movie::INPUT_MOVIE_FORMAT
        || identity.port != 0
        || identity.frames != request.frames.saturating_add(request.warmup_frames)
        || !valid_hex_digest(&identity.sha256, 32)
    {
        return Err(PublishError::InvalidIdentity(
            "input movie request identity is invalid".into(),
        ));
    }
    regular_owned_member(&path)?;
    let metadata = fs::metadata(&path)?;
    if metadata.len() != identity.bytes {
        return Err(PublishError::InvalidIdentity(
            "input movie member size mismatch".into(),
        ));
    }
    let bytes = fs::read(&path)?;
    if identity.sha256 != hex::encode(Sha256::digest(&bytes)) {
        return Err(PublishError::InvalidIdentity(
            "input movie member digest mismatch".into(),
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        PublishError::InvalidIdentity(format!("input movie member is not UTF-8: {error}"))
    })?;
    let canonical = canonical_recording_movie(
        text,
        request.frames.saturating_add(request.warmup_frames),
        u64::MAX,
    )
    .map_err(PublishError::InvalidIdentity)?;
    if canonical.bytes != bytes {
        return Err(PublishError::InvalidIdentity(
            "input movie member is not canonical".into(),
        ));
    }
    Ok(())
}

pub fn verify_published_recording(
    bundle_path: &Path,
    registry: &EventContractRegistry,
) -> Result<VerifiedPublishedRecording, PublishedBundleError> {
    const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
    let metadata = fs::symlink_metadata(bundle_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PublishedBundleError::Invalid(
            "bundle path must be a real directory".into(),
        ));
    }
    let bundle_path = fs::canonicalize(bundle_path)?;
    let manifest_path = bundle_path.join(MANIFEST_MEMBER);
    let manifest_metadata = fs::symlink_metadata(&manifest_path)
        .map_err(|_| PublishedBundleError::UnsafeManifest(manifest_path.clone()))?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return Err(PublishedBundleError::UnsafeManifest(manifest_path));
    }
    if manifest_metadata.len() > MAX_MANIFEST_BYTES {
        return Err(PublishedBundleError::ManifestTooLarge);
    }
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest = match parse_manifest(std::str::from_utf8(&manifest_bytes).map_err(|error| {
        PublishedBundleError::Invalid(format!("manifest is not UTF-8: {error}"))
    })?)? {
        BundleManifest::Legacy(_) => return Err(PublishedBundleError::LegacyFormat),
        BundleManifest::Recording(manifest) => *manifest,
    };
    if manifest.format_version != RECORDING_FORMAT_VERSION
        || manifest.capture_kind != super::recording_manifest::CaptureKind::RecordWindow
        || manifest.terminal.publication != PublicationOutcome::Published
        || manifest.members.len()
            != 1 + usize::from(manifest.request.input_movie.is_some())
                + usize::from(manifest.request.initial_state.is_some())
                + manifest.request.terminal_snapshots.len()
                + manifest.request.initial_snapshots.len()
                + usize::from(manifest.request.terminal_state.is_some())
    {
        return Err(PublishedBundleError::Invalid(
            "recording manifest shape or publication state mismatch".into(),
        ));
    }
    ensure_identity(&RecordingBundleInput {
        capture_id: manifest.capture_id.clone(),
        created_at_unix_ms: manifest.created_at_unix_ms,
        request_digest_sha256: manifest.request_digest_sha256.clone(),
        runtime: manifest.runtime.clone(),
        event_order: manifest.scope.event_order,
        validation: RecordingValidationInput {
            request: manifest.request.clone(),
            origin: manifest.scope.origin,
            f_start: manifest.scope.f_start,
            f_end: manifest.scope.f_end,
            observation_start: manifest.scope.observation_start.clone(),
            terminal: ProducerTerminalReport {
                operation_outcome: manifest.terminal.operation_outcome,
                execution_outcome: manifest.terminal.execution_outcome,
                claimed_integrity: manifest.terminal.integrity,
                final_execution_state: manifest.terminal.final_execution_state,
                final_frame: manifest.terminal.final_frame,
                f_origin: manifest.scope.f_origin,
                counters: manifest.counters.clone(),
                loss: manifest.loss.clone(),
                cleanup: manifest.cleanup.clone(),
                stop_event: manifest.terminal.stop_event.clone(),
                reason: manifest.terminal.reason.clone(),
                event_classes: manifest.terminal.event_classes.clone(),
            },
        },
    })
    .map_err(|error| PublishedBundleError::Invalid(error.to_string()))?;
    let event_members: Vec<_> = manifest
        .members
        .iter()
        .filter(|member| member.role == MemberRole::Events)
        .collect();
    if event_members.len() != 1 || event_members[0].path != EVENTS_MEMBER {
        return Err(PublishedBundleError::Invalid(
            "unexpected recording member layout".into(),
        ));
    }
    let member = event_members[0];
    validate_input_movie_member(&bundle_path, &manifest.request)
        .map_err(|error| PublishedBundleError::Invalid(error.to_string()))?;
    super::publish_state::validate_initial_state_member(
        &bundle_path,
        &manifest.request,
        &manifest.runtime,
    )
    .map_err(|error| PublishedBundleError::Invalid(error.to_string()))?;
    let initial_snapshot_members =
        validate_initial_snapshot_members(&bundle_path, &manifest.request)
            .map_err(|error| PublishedBundleError::Invalid(error.to_string()))?;
    let snapshot_members = validate_terminal_snapshot_members(&bundle_path, &manifest.request)
        .map_err(|error| PublishedBundleError::Invalid(error.to_string()))?;
    let terminal_state_member = validate_terminal_state_member(&bundle_path, &manifest.request)
        .map_err(|error| PublishedBundleError::Invalid(error.to_string()))?;
    if let Some(identity) = &manifest.request.input_movie {
        let movie_members: Vec<_> = manifest
            .members
            .iter()
            .filter(|member| member.role == MemberRole::InputMovie)
            .collect();
        if movie_members.len() != 1
            || movie_members[0].path != INPUT_MOVIE_MEMBER
            || movie_members[0].sha256 != identity.sha256
            || movie_members[0].bytes != identity.bytes
            || movie_members[0].records != Some(identity.frames)
        {
            return Err(PublishedBundleError::Invalid(
                "input movie member descriptor mismatch".into(),
            ));
        }
    }
    if let Some(receipt) = &manifest.request.initial_state {
        if !super::publish_state::descriptor_matches(&manifest.members, receipt) {
            return Err(PublishedBundleError::Invalid(
                "initial state member descriptor mismatch".into(),
            ));
        }
    }
    let described_snapshots: Vec<_> = manifest
        .members
        .iter()
        .filter(|member| member.role == MemberRole::TerminalSnapshot)
        .collect();
    if described_snapshots != snapshot_members.iter().collect::<Vec<_>>() {
        return Err(PublishedBundleError::Invalid(
            "terminal snapshot member descriptor mismatch".into(),
        ));
    }
    let described_initial_snapshots: Vec<_> = manifest
        .members
        .iter()
        .filter(|member| member.role == MemberRole::InitialSnapshot)
        .collect();
    if described_initial_snapshots != initial_snapshot_members.iter().collect::<Vec<_>>() {
        return Err(PublishedBundleError::Invalid(
            "initial snapshot member descriptor mismatch".into(),
        ));
    }
    let described_terminal_state: Vec<_> = manifest
        .members
        .iter()
        .filter(|member| member.role == MemberRole::TerminalState)
        .collect();
    if described_terminal_state
        != terminal_state_member
            .as_ref()
            .into_iter()
            .collect::<Vec<_>>()
    {
        return Err(PublishedBundleError::Invalid(
            "terminal state member descriptor mismatch".into(),
        ));
    }
    let events_path = bundle_path.join(EVENTS_MEMBER);
    regular_owned_member(&events_path)
        .map_err(|error| PublishedBundleError::Invalid(error.to_string()))?;
    let validated = validate_recording(
        &events_path,
        registry,
        RecordingValidationInput {
            request: manifest.request.clone(),
            origin: manifest.scope.origin,
            f_start: manifest.scope.f_start,
            f_end: manifest.scope.f_end,
            observation_start: manifest.scope.observation_start.clone(),
            terminal: ProducerTerminalReport {
                operation_outcome: manifest.terminal.operation_outcome,
                execution_outcome: manifest.terminal.execution_outcome,
                claimed_integrity: manifest.terminal.integrity,
                final_execution_state: manifest.terminal.final_execution_state,
                final_frame: manifest.terminal.final_frame,
                f_origin: manifest.scope.f_origin,
                counters: manifest.counters.clone(),
                loss: manifest.loss.clone(),
                cleanup: manifest.cleanup.clone(),
                stop_event: manifest.terminal.stop_event.clone(),
                reason: manifest.terminal.reason.clone(),
                event_classes: manifest.terminal.event_classes.clone(),
            },
        },
    )?;
    let stream = validated.stream();
    if validated.integrity() != manifest.terminal.integrity
        || stream.sha256 != member.sha256
        || stream.physical_bytes != member.bytes
        || member.records != Some(stream.records)
        || manifest.counters.events != stream.records
        || manifest.counters.bytes != stream.physical_bytes
    {
        return Err(PublishedBundleError::Invalid(
            "recording member hash, counters, or integrity mismatch".into(),
        ));
    }
    Ok(VerifiedPublishedRecording {
        bundle_path,
        manifest_sha256: hex::encode(Sha256::digest(&manifest_bytes)),
        manifest,
    })
}
