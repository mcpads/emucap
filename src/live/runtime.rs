use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(all(unix, not(target_os = "linux")))]
use std::process::Command;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u32 = 1;
pub const MAX_CAPSULE_FILE_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone)]
pub struct RuntimeStore {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PreparedGeneration {
    store: RuntimeStore,
    port: u16,
    launch_id: String,
    reclaim_token: String,
    expected_current_launch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CurrentManifest {
    pub schema_version: u32,
    pub launch_id: String,
    pub port: u16,
    pub adapter: String,
    pub system: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub emulator: ProcessIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge: Option<ProcessIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_endpoint: Option<String>,
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ManifestSpec {
    pub adapter: String,
    pub system: String,
    pub content: String,
    pub emulator_pid: u32,
    pub bridge_pid: Option<u32>,
    pub backend_endpoint: Option<String>,
    pub build: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Alive,
    Exited,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseRecord {
    /// Stable, one-way key for the control session. The raw runtime session id is never persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_session_key: Option<String>,
    pub holder: ProcessIdentity,
    pub acquired_at_unix_ms: u64,
    pub refreshed_at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Held,
    Available,
    Occupied,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseView {
    pub state: LeaseState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_pid: Option<u32>,
}

impl LeaseView {
    pub fn unknown() -> Self {
        Self {
            state: LeaseState::Unknown,
            holder_pid: None,
        }
    }
}

impl Default for LeaseView {
    fn default() -> Self {
        Self::unknown()
    }
}

impl RuntimeStore {
    pub fn discover() -> Self {
        Self::new(crate::launch::emu_home_base().join("sessions"))
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_dir(&self, port: u16) -> PathBuf {
        self.root.join(port.to_string())
    }

    pub fn current_path(&self, port: u16) -> PathBuf {
        self.session_dir(port).join("current.json")
    }

    pub fn generation_dir(&self, port: u16, launch_id: &str) -> PathBuf {
        self.session_dir(port).join("generations").join(launch_id)
    }

    pub fn auth_path(&self, port: u16, launch_id: &str) -> PathBuf {
        self.generation_dir(port, launch_id).join("auth")
    }

    pub fn link_path(&self, port: u16, launch_id: &str) -> PathBuf {
        self.generation_dir(port, launch_id).join("link.json")
    }

    pub fn adapter_failure_path(&self, port: u16, launch_id: &str) -> PathBuf {
        self.generation_dir(port, launch_id)
            .join("adapter-failure.json")
    }

    pub fn compatibility_token_path(&self, port: u16) -> PathBuf {
        self.root
            .join("compatibility")
            .join(format!("session-token-{port}"))
    }

    pub fn persisted_port_path(&self, identity: &str, base: u16) -> io::Result<PathBuf> {
        validate_compatibility_identity(identity)?;
        if base == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot persist an ephemeral base port",
            ));
        }
        Ok(self
            .root
            .join("compatibility")
            .join(format!("listener-port-{identity}-{base}")))
    }

    pub fn write_compatibility_token(&self, port: u16, token: &str) -> io::Result<()> {
        if token.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to write an empty compatibility token",
            ));
        }
        let path = self.compatibility_token_path(port);
        validate_existing_private_file(&path)?;
        write_atomic_bytes(&path, token.as_bytes())
    }

    pub fn read_compatibility_token(&self, port: u16) -> io::Result<Option<String>> {
        let path = self.compatibility_token_path(port);
        validate_existing_private_file(&path)?;
        let Some(bytes) = read_bounded_if_exists(&path)? else {
            return Ok(None);
        };
        let token = String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let token = token.trim();
        if token.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("empty compatibility token file: {}", path.display()),
            ));
        }
        Ok(Some(token.to_string()))
    }

    pub fn write_persisted_port(&self, identity: &str, base: u16, port: u16) -> io::Result<()> {
        let path = self.persisted_port_path(identity, base)?;
        validate_existing_private_file(&path)?;
        write_atomic_bytes(&path, port.to_string().as_bytes())
    }

    pub fn read_persisted_port(&self, identity: &str, base: u16) -> io::Result<Option<u16>> {
        let path = self.persisted_port_path(identity, base)?;
        validate_existing_private_file(&path)?;
        let Some(bytes) = read_bounded_if_exists(&path)? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        text.trim().parse::<u16>().map(Some).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid persisted listener port in {}: {error}",
                    path.display()
                ),
            )
        })
    }

    pub fn prepare(&self, port: u16) -> io::Result<PreparedGeneration> {
        let expected_current_launch_id = self.read_current(port)?.map(|current| current.launch_id);
        let launch_id = format!("launch-{}", ulid::Ulid::new().to_string().to_lowercase());
        let reclaim_token = format!(
            "reclaim-{}{}",
            ulid::Ulid::new().to_string().to_lowercase(),
            ulid::Ulid::new().to_string().to_lowercase()
        );
        let generation = self.generation_dir(port, &launch_id);
        self.create_managed_dir(&generation)?;
        write_atomic_bytes(&self.auth_path(port, &launch_id), reclaim_token.as_bytes())?;
        Ok(PreparedGeneration {
            store: self.clone(),
            port,
            launch_id,
            reclaim_token,
            expected_current_launch_id,
        })
    }

    pub fn read_current(&self, port: u16) -> io::Result<Option<CurrentManifest>> {
        let current: Option<CurrentManifest> = read_json_if_exists(&self.current_path(port))?;
        if let Some(current) = current.as_ref() {
            validate_launch_id(&current.launch_id)?;
            if current.schema_version != SCHEMA_VERSION || current.port != port {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "runtime current manifest schema/port mismatch",
                ));
            }
        }
        Ok(current)
    }

    pub fn read_auth(&self, port: u16, launch_id: &str) -> io::Result<Option<String>> {
        validate_launch_id(launch_id)?;
        let path = self.auth_path(port, launch_id);
        let Some(bytes) = read_bounded_if_exists(&path)? else {
            return Ok(None);
        };
        let token =
            String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let token = token.trim();
        if token.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("empty runtime auth file: {}", path.display()),
            ));
        }
        Ok(Some(token.to_string()))
    }

    pub fn read_link_json<T: DeserializeOwned>(
        &self,
        port: u16,
        launch_id: &str,
    ) -> io::Result<Option<T>> {
        validate_launch_id(launch_id)?;
        read_json_if_exists(&self.link_path(port, launch_id))
    }

    pub fn update_link_json<T, F>(&self, port: u16, launch_id: &str, update: F) -> io::Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(Option<T>) -> io::Result<T>,
    {
        validate_launch_id(launch_id)?;
        let generation = self.generation_dir(port, launch_id);
        self.create_managed_dir(&generation)?;
        let lock_path = generation.join(".link.lock");
        let lock = open_private_lock(&lock_path)?;
        lock_with_deadline(&lock, std::time::Duration::from_millis(250))?;
        let result = (|| {
            let current = read_json_if_exists(&self.link_path(port, launch_id))?;
            let next = update(current)?;
            write_atomic_json(&self.link_path(port, launch_id), &next)?;
            Ok(next)
        })();
        let _ = fs2::FileExt::unlock(&lock);
        result
    }

    pub fn read_adapter_failure(
        &self,
        port: u16,
        launch_id: &str,
    ) -> io::Result<Option<serde_json::Value>> {
        validate_launch_id(launch_id)?;
        read_json_if_exists(&self.adapter_failure_path(port, launch_id))
    }

    pub fn live_current_with_auth(
        &self,
        port: u16,
    ) -> io::Result<Option<(CurrentManifest, String)>> {
        let Some(current) = self.read_current(port)? else {
            return Ok(None);
        };
        if current.process_state() != ProcessState::Alive {
            return Ok(None);
        }
        let Some(token) = self.read_auth(port, &current.launch_id)? else {
            return Ok(None);
        };
        Ok(Some((current, token)))
    }

    fn cleanup_other_generations(&self, port: u16, keep: &str) -> io::Result<()> {
        validate_launch_id(keep)?;
        let generations = self.session_dir(port).join("generations");
        let entries = match fs::read_dir(&generations) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_name() != keep {
                remove_path(&entry.path())?;
            }
        }
        Ok(())
    }

    fn create_managed_dir(&self, path: &Path) -> io::Result<()> {
        if !path.starts_with(&self.root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime managed path escapes store root",
            ));
        }
        create_private_dir(&self.root)?;
        reject_symlink(&self.root)?;
        let relative = path.strip_prefix(&self.root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime path is outside store root",
            )
        })?;
        let mut current = self.root.clone();
        for component in relative.components() {
            use std::path::Component;
            let Component::Normal(component) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "runtime managed path contains a non-normal component",
                ));
            };
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("runtime managed path is a symlink: {}", current.display()),
                    ));
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "runtime managed path is not a directory: {}",
                            current.display()
                        ),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    create_private_dir(&current)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

impl PreparedGeneration {
    pub fn launch_id(&self) -> &str {
        &self.launch_id
    }

    pub fn reclaim_token(&self) -> &str {
        &self.reclaim_token
    }

    pub fn adapter_failure_path(&self) -> PathBuf {
        self.store.adapter_failure_path(self.port, &self.launch_id)
    }

    pub fn manifest(&self, spec: ManifestSpec) -> CurrentManifest {
        CurrentManifest {
            schema_version: SCHEMA_VERSION,
            launch_id: self.launch_id.clone(),
            port: self.port,
            adapter: spec.adapter,
            system: spec.system,
            content: spec.content,
            build: spec.build,
            emulator: capture_process(spec.emulator_pid),
            bridge: spec.bridge_pid.map(capture_process),
            backend_endpoint: spec.backend_endpoint,
            created_at_unix_ms: now_unix_ms(),
        }
    }

    pub fn commit(&self, manifest: &CurrentManifest) -> io::Result<()> {
        validate_launch_id(&manifest.launch_id)?;
        if manifest.launch_id != self.launch_id || manifest.port != self.port {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime manifest does not match prepared generation",
            ));
        }
        let current_lock = self.store.session_dir(self.port).join(".current.lock");
        let lock = open_private_lock(&current_lock)?;
        lock_with_deadline(&lock, std::time::Duration::from_millis(250))?;
        let result = (|| {
            let observed_current = self
                .store
                .read_current(self.port)?
                .map(|current| current.launch_id);
            if observed_current != self.expected_current_launch_id {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "runtime current generation changed after launch preparation",
                ));
            }
            write_atomic_json(&self.store.current_path(self.port), manifest)?;
            let _ = self
                .store
                .cleanup_other_generations(self.port, &self.launch_id);
            Ok(())
        })();
        let _ = fs2::FileExt::unlock(&lock);
        result
    }

    pub fn abort(&self) -> io::Result<()> {
        if self
            .store
            .read_current(self.port)?
            .is_some_and(|m| m.launch_id == self.launch_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot abort the current runtime generation",
            ));
        }
        remove_path(&self.store.generation_dir(self.port, &self.launch_id))
    }
}

impl CurrentManifest {
    pub fn process_state(&self) -> ProcessState {
        process_state(&self.emulator)
    }

    pub fn bridge_process_state(&self) -> Option<ProcessState> {
        self.bridge.as_ref().map(process_state)
    }

    pub fn public_value(&self) -> serde_json::Value {
        self.public_value_with_lease(&LeaseView::unknown())
    }

    pub fn public_value_with_lease(&self, lease: &LeaseView) -> serde_json::Value {
        let emulator_state = self.process_state();
        let bridge_state = self.bridge_process_state();
        serde_json::json!({
            "launch_id": self.launch_id,
            "port": self.port,
            "adapter": self.adapter,
            "system": self.system,
            "content": self.content,
            "build": self.build,
            "process_state": emulator_state,
            "emulator_pid": self.emulator.pid,
            "bridge_pid": self.bridge.as_ref().map(|p| p.pid),
            "bridge_process_state": bridge_state,
            "backend_endpoint": self.backend_endpoint,
            "lease": lease,
            "next_safe_action": next_safe_action(emulator_state, bridge_state, lease.state),
        })
    }

    pub fn terminate_owned_processes(&self) -> io::Result<()> {
        if self.process_state() == ProcessState::Unknown {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "emulator process identity is unknown",
            ));
        }
        if let Some(bridge) = self.bridge.as_ref() {
            match process_state(bridge) {
                ProcessState::Alive => crate::launch::terminate_detached(bridge.pid)?,
                ProcessState::Exited => {}
                ProcessState::Unknown => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "bridge process identity is unknown",
                    ))
                }
            }
        }
        match self.process_state() {
            ProcessState::Alive => crate::launch::terminate_detached(self.emulator.pid)?,
            ProcessState::Exited => {}
            ProcessState::Unknown => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "emulator process identity became unknown before termination",
                ))
            }
        }
        Ok(())
    }
}

fn next_safe_action(
    emulator_state: ProcessState,
    bridge_state: Option<ProcessState>,
    lease_state: LeaseState,
) -> &'static str {
    if lease_state == LeaseState::Occupied {
        return "coordinate_with_current_controller";
    }
    if emulator_state == ProcessState::Exited && lease_state == LeaseState::Unknown {
        return "inspect_lease_before_generation_transition";
    }
    match (emulator_state, bridge_state) {
        (ProcessState::Alive, Some(ProcessState::Exited)) => "recover_bridge_or_replace",
        (ProcessState::Alive, Some(ProcessState::Unknown)) => {
            "inspect_bridge_identity_before_recovery"
        }
        (ProcessState::Alive, _) => "reattach_or_inspect",
        (ProcessState::Exited, Some(ProcessState::Alive)) => "cleanup_owned_bridge_then_launch",
        (ProcessState::Exited, Some(ProcessState::Unknown)) => {
            "inspect_bridge_identity_before_launch"
        }
        (ProcessState::Exited, _) => "launch_allowed",
        (ProcessState::Unknown, _) => "inspect_process_identity_before_replace",
    }
}

pub fn capture_process(pid: u32) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        start_identity: process_start_identity(pid),
    }
}

pub fn process_state(process: &ProcessIdentity) -> ProcessState {
    if !crate::launch::process_alive(process.pid) {
        return ProcessState::Exited;
    }
    let Some(expected) = process.start_identity.as_deref() else {
        return ProcessState::Unknown;
    };
    match process_start_identity_matches(process.pid, expected) {
        Some(true) => ProcessState::Alive,
        Some(false) => ProcessState::Exited,
        None => ProcessState::Unknown,
    }
}

fn process_start_identity_matches(pid: u32, expected: &str) -> Option<bool> {
    #[cfg(target_os = "macos")]
    if !expected.starts_with("macos-bsdinfo:") {
        return legacy_macos_process_start_identity(pid).map(|actual| actual == expected);
    }
    process_start_identity(pid).map(|actual| actual == expected)
}

#[cfg(target_os = "linux")]
fn process_start_identity(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let start_ticks = stat[close + 1..].split_whitespace().nth(19)?;
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(format!("{}:{start_ticks}", boot_id.trim()))
}

#[cfg(target_os = "macos")]
fn process_start_identity(pid: u32) -> Option<String> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let buffer_size = libc::c_int::try_from(size).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            buffer_size,
        )
    };
    if read != buffer_size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid as u32 || info.pbi_start_tvsec == 0 || info.pbi_start_tvusec >= 1_000_000
    {
        return None;
    }
    Some(format!(
        "macos-bsdinfo:{}:{:06}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(target_os = "macos")]
fn legacy_macos_process_start_identity(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_start_identity(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(windows)]
fn process_start_identity(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        None
    } else {
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        Some(format!("windows-filetime:{ticks}"))
    }
}

#[cfg(not(any(unix, windows)))]
fn process_start_identity(_pid: u32) -> Option<String> {
    None
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Stable opaque control-session key. The selected environment value is hashed before it reaches
/// disk so capsule files never expose product-specific raw session ids.
pub fn control_session_key() -> Option<String> {
    for key in [
        "EMUCAP_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
    ] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                let mut digest = Sha256::new();
                digest.update(b"emucap-control-session-v1\0");
                digest.update(value.as_bytes());
                let hex = hex::encode(digest.finalize());
                return Some(format!("control-{}", &hex[..24]));
            }
        }
    }
    None
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refusing symlink runtime directory: {}", path.display()),
        ));
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn validate_compatibility_identity(identity: &str) -> io::Result<()> {
    if identity.is_empty()
        || identity.len() > 128
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid compatibility session identity",
        ));
    }
    Ok(())
}

fn validate_existing_private_file(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refusing symlink runtime file: {}", path.display()),
        ));
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("runtime path is not a regular file: {}", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = metadata.uid();
        let current = unsafe { libc::geteuid() };
        if owner != current {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "runtime file is owned by uid {owner}, current uid is {current}: {}",
                    path.display()
                ),
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "runtime file is accessible by group or other users: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn write_atomic_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_atomic_bytes(path, &bytes)
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CAPSULE_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("runtime capsule file exceeds {MAX_CAPSULE_FILE_BYTES} bytes"),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime path has no parent"))?;
    create_private_dir(parent)?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("capsule"),
        ulid::Ulid::new().to_string().to_lowercase()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        atomic_replace(&tmp, path)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn open_private_lock(path: &Path) -> io::Result<File> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn lock_with_deadline(file: &File, timeout: std::time::Duration) -> io::Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match fs2::FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "runtime link capsule writer is busy",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
}

fn read_json_if_exists<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let Some(bytes) = read_bounded_if_exists(path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn read_bounded_if_exists(path: &Path) -> io::Result<Option<Vec<u8>>> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    if len > MAX_CAPSULE_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("runtime capsule file exceeds {MAX_CAPSULE_FILE_BYTES} bytes"),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(MAX_CAPSULE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CAPSULE_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("runtime capsule file exceeds {MAX_CAPSULE_FILE_BYTES} bytes"),
        ));
    }
    Ok(Some(bytes))
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refusing symlink runtime path: {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_launch_id(launch_id: &str) -> io::Result<()> {
    let valid = launch_id.starts_with("launch-")
        && launch_id.len() <= 128
        && launch_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid runtime launch_id",
        ))
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(not(windows))]
fn atomic_replace(src: &Path, dst: &Path) -> io::Result<()> {
    fs::rename(src, dst)
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;

#[cfg(windows)]
fn atomic_replace(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let src: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
    let dst: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        MoveFileExW(
            src.as_ptr(),
            dst.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
