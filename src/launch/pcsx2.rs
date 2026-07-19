//! PCSX2 launch preparation for PlayStation 2.
//!
//! The supported binary is built from the pinned fork under `adapters/pcsx2`. The fork extends
//! PINE with terminally acknowledged debugger operations and accepts an emucap-owned data root.
//! Each launch therefore uses private settings, memory cards, caches, logs, and PINE endpoint while
//! referring to the operator-supplied BIOS in place.

use serde::{Deserialize, Serialize};
use std::io;
use std::net::TcpListener;
#[cfg(windows)]
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{
    find_on_path, is_runnable_file, process_alive, spawn_detached, terminate_detached, LaunchSpec,
    RuntimeEnv,
};

pub const REQUIRED_HOST_API: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildMetadata {
    pub upstream: String,
    pub commit: String,
    pub patches_upstream: String,
    pub patches_commit: String,
    pub patches_tree: String,
    pub patches_archive_sha256: String,
    pub host_api: u32,
    pub patchset_sha256: String,
}

fn patch_required(message: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("pcsx2-patch-required: {}", message.into()),
    )
}

fn lock_value(lock: &str, key: &str) -> Option<String> {
    lock.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

pub fn build_metadata_path(binary: &Path) -> PathBuf {
    binary
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("emucap-pcsx2-build.json")
}

pub fn read_build_metadata(binary: &Path) -> io::Result<BuildMetadata> {
    let path = build_metadata_path(binary);
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        patch_required(format!(
            "compatible build metadata is missing at {} ({error}); run adapters/pcsx2/build.sh",
            path.display()
        ))
    })?;
    let metadata: BuildMetadata = serde_json::from_str(&raw).map_err(|error| {
        patch_required(format!(
            "invalid build metadata at {}: {error}",
            path.display()
        ))
    })?;
    if metadata.host_api != REQUIRED_HOST_API {
        return Err(patch_required(format!(
            "host API {} is incompatible; expected {}",
            metadata.host_api, REQUIRED_HOST_API
        )));
    }
    if metadata.commit.len() != 40 || !metadata.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(patch_required(
            "metadata commit is not a full git object id",
        ));
    }
    if metadata.patches_commit.len() != 40
        || !metadata
            .patches_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(patch_required(
            "metadata patches_commit is not a full git object id",
        ));
    }
    if metadata.patches_tree.len() != 40
        || !metadata
            .patches_tree
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(patch_required(
            "metadata patches_tree is not a full git object id",
        ));
    }
    if metadata.patches_archive_sha256.len() != 64
        || !metadata
            .patches_archive_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(patch_required("metadata patches_archive_sha256 is invalid"));
    }
    if metadata.patchset_sha256.len() != 64
        || !metadata
            .patchset_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(patch_required("metadata patchset_sha256 is invalid"));
    }
    Ok(metadata)
}

pub fn require_compatible_build(root: &Path, binary: &Path) -> io::Result<BuildMetadata> {
    let metadata = read_build_metadata(binary)?;
    let lock_path = root.join("adapters/pcsx2/upstream.lock");
    let lock = std::fs::read_to_string(&lock_path)?;
    let expected_repo = lock_value(&lock, "PCSX2_REPO")
        .ok_or_else(|| patch_required("PCSX2_REPO missing from upstream.lock"))?;
    let expected_commit = lock_value(&lock, "PCSX2_COMMIT")
        .ok_or_else(|| patch_required("PCSX2_COMMIT missing from upstream.lock"))?;
    let expected_patches_repo = lock_value(&lock, "PCSX2_PATCHES_REPO")
        .ok_or_else(|| patch_required("PCSX2_PATCHES_REPO missing from upstream.lock"))?;
    let expected_patches_commit = lock_value(&lock, "PCSX2_PATCHES_COMMIT")
        .ok_or_else(|| patch_required("PCSX2_PATCHES_COMMIT missing from upstream.lock"))?;
    let expected_patches_tree = lock_value(&lock, "PCSX2_PATCHES_TREE")
        .ok_or_else(|| patch_required("PCSX2_PATCHES_TREE missing from upstream.lock"))?;
    let expected_patches_archive = lock_value(&lock, "PCSX2_PATCHES_ARCHIVE_SHA256")
        .ok_or_else(|| patch_required("PCSX2_PATCHES_ARCHIVE_SHA256 missing from upstream.lock"))?;
    let expected_api = lock_value(&lock, "PCSX2_HOST_API")
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| patch_required("PCSX2_HOST_API invalid in upstream.lock"))?;
    let expected_patchset = lock_value(&lock, "PCSX2_PATCHSET_SHA256")
        .ok_or_else(|| patch_required("PCSX2_PATCHSET_SHA256 missing from upstream.lock"))?;
    if metadata.upstream != expected_repo
        || metadata.commit != expected_commit
        || metadata.patches_upstream != expected_patches_repo
        || metadata.patches_commit != expected_patches_commit
        || metadata.patches_tree != expected_patches_tree
        || metadata.patches_archive_sha256 != expected_patches_archive
        || metadata.host_api != expected_api
        || metadata.patchset_sha256 != expected_patchset
    {
        return Err(patch_required(format!(
            "build sidecar does not match {}",
            lock_path.display()
        )));
    }
    Ok(metadata)
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "pcsx2-qt.exe"
    } else {
        "pcsx2-qt"
    }
}

pub fn local_build_candidates(root: &Path) -> Vec<PathBuf> {
    let build = root.join("adapters/pcsx2/work/pcsx2/build-emucap");
    vec![
        build.join("pcsx2-qt/PCSX2.app/Contents/MacOS/PCSX2"),
        build.join("pcsx2-qt/pcsx2-qt.app/Contents/MacOS/pcsx2-qt"),
        build.join("pcsx2-qt/PCSX2-Qt.app/Contents/MacOS/PCSX2-Qt"),
        build.join("pcsx2-qt/pcsx2-qt"),
        build.join("bin/pcsx2-qt"),
        build.join("bin/pcsx2-qt.exe"),
    ]
}

pub fn resolve_binary(root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PCSX2_BIN") {
        let path = PathBuf::from(explicit);
        return is_runnable_file(&path).then_some(path);
    }
    local_build_candidates(root)
        .into_iter()
        .find(|path| is_runnable_file(path))
        .or_else(|| find_on_path(binary_name()))
}

fn bridge_binary_name() -> &'static str {
    if cfg!(windows) {
        "emucap-pcsx2-bridge.exe"
    } else {
        "emucap-pcsx2-bridge"
    }
}

pub fn resolve_bridge(root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PCSX2_BRIDGE_BIN") {
        let path = PathBuf::from(explicit);
        return is_runnable_file(&path).then_some(path);
    }
    let name = bridge_binary_name();
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(name));
        }
    }
    candidates.push(root.join("target/release").join(name));
    candidates.push(root.join("target/debug").join(name));
    if let Some(path) = find_on_path(name) {
        candidates.push(path);
    }
    candidates.into_iter().find(|path| is_runnable_file(path))
}

pub fn resolve_bios() -> io::Result<PathBuf> {
    let raw = std::env::var_os("EMUCAP_PCSX2_BIOS").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "EMUCAP_PCSX2_BIOS must name an operator-supplied PS2 BIOS file",
        )
    })?;
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "EMUCAP_PCSX2_BIOS must be an absolute path",
        ));
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() || !(4 * 1024 * 1024..=8 * 1024 * 1024).contains(&metadata.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "EMUCAP_PCSX2_BIOS must be a 4-8 MiB file, got {} bytes at {}",
                metadata.len(),
                path.display()
            ),
        ));
    }
    Ok(path)
}

#[derive(Debug)]
struct PineSlot {
    slot: u16,
    reservation: Option<TcpListener>,
}

fn resolve_pine_slot() -> io::Result<PineSlot> {
    if let Some(raw) = std::env::var_os("EMUCAP_PCSX2_PINE_SLOT") {
        let raw = raw.to_string_lossy();
        let slot = raw.parse::<u16>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("EMUCAP_PCSX2_PINE_SLOT must be a decimal port, got {raw:?}"),
            )
        })?;
        if slot == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "EMUCAP_PCSX2_PINE_SLOT must be in 1..=65535",
            ));
        }
        return Ok(PineSlot {
            slot,
            reservation: None,
        });
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(PineSlot {
        slot: listener.local_addr()?.port(),
        reservation: Some(listener),
    })
}

fn pine_socket_path(runtime_dir: &Path, slot: u16) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let suffix = if slot == 28011 {
            String::new()
        } else {
            format!(".{slot}")
        };
        Some(runtime_dir.join(format!("pcsx2.sock{suffix}")))
    }
    #[cfg(not(unix))]
    {
        let _ = (runtime_dir, slot);
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSession {
    pub home: PathBuf,
    pub data_root: PathBuf,
    pub pine_runtime: PathBuf,
    pub pine_socket: Option<PathBuf>,
}

fn prepare_session(port: u16, bios: &Path, pine_slot: u16) -> io::Result<PreparedSession> {
    let home = super::emu_home_dir("pcsx2", port);
    let data_root = home.join("data");
    let settings = data_root.join("inis");
    let pine_runtime = home.join("pine");
    let base = super::emu_home_base();
    for path in [&home, &data_root, &settings, &pine_runtime] {
        if super::has_symlink_component_under(&base, path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "PCSX2 session directory contains a symlink, refusing to launch: {}",
                    path.display()
                ),
            ));
        }
    }
    std::fs::create_dir_all(&settings)?;
    std::fs::create_dir_all(&pine_runtime)?;
    let bios_directory = bios.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "EMUCAP_PCSX2_BIOS has no parent directory",
        )
    })?;
    let bios_name = bios
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "EMUCAP_PCSX2_BIOS filename must be valid UTF-8",
            )
        })?;
    let ini = format!(
        "[UI]\nSettingsVersion = 1\nSetupWizardIncomplete = false\nConfirmShutdown = false\nLanguage = en-US\n\n\
         [Folders]\nBios = {}\n\n\
         [Filenames]\nBIOS = {}\n\n\
         [EmuCore]\nEnablePINE = true\nPINESlot = {}\nEnableDiscordPresence = false\n\n\
         [SPU2/Output]\nOutputMuted = true\n",
        bios_directory.display(),
        bios_name,
        pine_slot
    );
    std::fs::write(settings.join("PCSX2.ini"), ini)?;
    Ok(PreparedSession {
        pine_socket: pine_socket_path(&pine_runtime, pine_slot),
        home,
        data_root,
        pine_runtime,
    })
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub bridge: &'a Path,
    pub bios: &'a Path,
    pub content: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub pcsx2_pid: u32,
    pub bridge_pid: u32,
    pub pine_slot: u16,
    pub pine_socket: Option<PathBuf>,
    pub data_root: PathBuf,
}

fn emulator_spec(launch: &Launch<'_>, prepared: &PreparedSession, slot: u16) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.binary, launch.log_path)
        .arg("-batch")
        .arg("-fastboot")
        .arg("-nofullscreen")
        .env(
            "EMUCAP_PCSX2_DATAROOT",
            prepared.data_root.to_string_lossy().into_owned(),
        )
        .env("HOME", prepared.home.to_string_lossy().into_owned());
    if !launch.display {
        spec = spec.arg("-nogui");
    }
    spec = spec.arg("--").arg(launch.content);
    #[cfg(target_os = "macos")]
    {
        spec = spec.env(
            "TMPDIR",
            prepared.pine_runtime.to_string_lossy().into_owned(),
        );
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        spec = spec.env(
            "XDG_RUNTIME_DIR",
            prepared.pine_runtime.to_string_lossy().into_owned(),
        );
    }
    let _ = slot;
    spec
}

fn bridge_spec(launch: &Launch<'_>, prepared: &PreparedSession, pine_slot: u16) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.bridge, launch.log_path)
        .arg(launch.port.to_string())
        .arg(pine_slot.to_string())
        .env("EMUCAP_CONTENT", launch.content)
        .env(
            "EMUCAP_PCSX2_CAPTURE_DIR",
            prepared
                .data_root
                .join("captures")
                .to_string_lossy()
                .into_owned(),
        );
    if let Some(path) = prepared.pine_socket.as_deref() {
        spec = spec.arg(path.to_string_lossy().into_owned());
    }
    if let Some(name) = launch.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = launch.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec.runtime_env(launch.runtime)
}

fn wait_pine_ready(
    pcsx2_pid: u32,
    slot: u16,
    socket_path: Option<&Path>,
    timeout: Duration,
) -> io::Result<()> {
    #[cfg(windows)]
    let _ = socket_path;
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pcsx2_pid) {
            return Err(io::Error::other(
                "PCSX2 exited before opening PINE; check the launch log and BIOS path",
            ));
        }
        #[cfg(unix)]
        let ready = socket_path.is_some_and(|path| path.exists());
        #[cfg(windows)]
        let ready = TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], slot)),
            Duration::from_millis(200),
        )
        .is_ok();
        #[cfg(not(any(unix, windows)))]
        let ready = false;
        if ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "PCSX2 did not open PINE slot {slot} within {timeout:?}; check the launch log and BIOS configuration"
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_survives(pid: u32, settle: Duration, message: &str) -> io::Result<()> {
    let deadline = Instant::now() + settle;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return Err(io::Error::other(message.to_string()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn write_pidfile(log_path: &Path, name: &str, pid: u32) {
    if let Some(directory) = log_path.parent() {
        let _ = std::fs::write(directory.join(name), format!("{pid}\n"));
    }
}

pub fn launch(launch: &Launch<'_>) -> io::Result<Launched> {
    let pine = resolve_pine_slot()?;
    let prepared = prepare_session(launch.port, launch.bios, pine.slot)?;
    if let Some(path) = prepared.pine_socket.as_deref() {
        let _ = std::fs::remove_file(path);
    }
    drop(pine.reservation);

    let pcsx2_pid = spawn_detached(&emulator_spec(launch, &prepared, pine.slot))?;
    write_pidfile(launch.log_path, "pcsx2.pid", pcsx2_pid);
    if let Err(error) = wait_pine_ready(
        pcsx2_pid,
        pine.slot,
        prepared.pine_socket.as_deref(),
        Duration::from_secs(30),
    ) {
        let _ = terminate_detached(pcsx2_pid);
        return Err(error);
    }
    if launch.display {
        super::spawn_display_caffeinate(pcsx2_pid);
    }

    let bridge_pid = match spawn_detached(&bridge_spec(launch, &prepared, pine.slot)) {
        Ok(pid) => pid,
        Err(error) => {
            let _ = terminate_detached(pcsx2_pid);
            return Err(error);
        }
    };
    if let Err(error) = wait_survives(
        bridge_pid,
        Duration::from_secs(2),
        "emucap-pcsx2-bridge exited during startup; check the launch log and host API version",
    ) {
        let _ = terminate_detached(bridge_pid);
        let _ = terminate_detached(pcsx2_pid);
        return Err(error);
    }
    write_pidfile(launch.log_path, "bridge.pid", bridge_pid);
    Ok(Launched {
        pcsx2_pid,
        bridge_pid,
        pine_slot: pine.slot,
        pine_socket: prepared.pine_socket,
        data_root: prepared.data_root,
    })
}

#[cfg(test)]
#[path = "pcsx2_tests.rs"]
mod tests;
