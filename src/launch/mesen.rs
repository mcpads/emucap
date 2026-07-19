//! Mesen2 portable launch preparation, cross-platform.
//!
//! The emucap Lua's socket needs Mesen's script I/O + network access enabled, a script timeout large
//! enough for one big read (dump_memory reads a whole region in a single call), and SingleInstance off
//! so starting an emucap instance doesn't take over the user's other open ROM, and a controller
//! connected on the game port (emu.setInput reaches no device otherwise). A minimal settings.json
//! keeps every launch in the copied portable home instead of loading the user's config or native
//! libraries. Required values are also passed as CLI overrides with `--donotSaveSettings`; neither
//! path modifies the user's file.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const REQUIRED_HOST_API: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildMetadata {
    pub upstream: String,
    pub tag: String,
    pub commit: String,
    pub host_api: u32,
    pub patchset_sha256: String,
}

fn patch_required(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("mesen-patch-required: {}", message.into()),
    )
}

pub fn build_metadata_path(binary: &Path) -> PathBuf {
    binary
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("emucap-mesen-build.json")
}

pub fn read_build_metadata(binary: &Path) -> std::io::Result<BuildMetadata> {
    let path = build_metadata_path(binary);
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        patch_required(format!(
            "compatible build metadata is missing at {} ({e}); run adapters/mesen2/build.sh or build.ps1",
            path.display()
        ))
    })?;
    let metadata: BuildMetadata = serde_json::from_str(&raw).map_err(|e| {
        patch_required(format!("invalid build metadata at {}: {e}", path.display()))
    })?;
    if metadata.host_api != REQUIRED_HOST_API {
        return Err(patch_required(format!(
            "host API {} is incompatible; expected {}",
            metadata.host_api, REQUIRED_HOST_API
        )));
    }
    if metadata.commit.len() != 40 || !metadata.commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(patch_required(
            "metadata commit is not a full git object id",
        ));
    }
    if metadata.patchset_sha256.len() != 64
        || !metadata
            .patchset_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(patch_required("metadata patchset_sha256 is invalid"));
    }
    Ok(metadata)
}

fn lock_value(lock: &str, key: &str) -> Option<String> {
    lock.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

/// Validate the sidecar against the repository's pinned source lock. The sidecar is a fast
/// preflight; the Lua hello feature remains the authoritative runtime check after launch.
pub fn require_compatible_build(root: &Path, binary: &Path) -> std::io::Result<BuildMetadata> {
    let metadata = read_build_metadata(binary)?;
    let lock_path = root.join("adapters/mesen2/upstream.lock");
    let lock = std::fs::read_to_string(&lock_path)?;
    let expected_repo = lock_value(&lock, "MESEN_REPO")
        .ok_or_else(|| patch_required("MESEN_REPO missing from upstream.lock"))?;
    let expected_tag = lock_value(&lock, "MESEN_TAG")
        .ok_or_else(|| patch_required("MESEN_TAG missing from upstream.lock"))?;
    let expected_commit = lock_value(&lock, "MESEN_COMMIT")
        .ok_or_else(|| patch_required("MESEN_COMMIT missing from upstream.lock"))?;
    let expected_api = lock_value(&lock, "MESEN_HOST_API")
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| patch_required("MESEN_HOST_API invalid in upstream.lock"))?;
    let expected_patchset = lock_value(&lock, "MESEN_PATCHSET_SHA256")
        .ok_or_else(|| patch_required("MESEN_PATCHSET_SHA256 missing from upstream.lock"))?;
    if metadata.upstream != expected_repo
        || metadata.tag != expected_tag
        || metadata.commit != expected_commit
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

pub fn local_build_candidates(root: &Path) -> Vec<PathBuf> {
    let source = root.join("adapters/mesen2/work/mesen/bin");
    #[cfg(target_os = "macos")]
    let rid = if cfg!(target_arch = "aarch64") {
        "osx-arm64"
    } else {
        "osx-x64"
    };
    #[cfg(target_os = "linux")]
    let rid = if cfg!(target_arch = "aarch64") {
        "linux-arm64"
    } else {
        "linux-x64"
    };
    #[cfg(target_os = "macos")]
    {
        let publish = source.join(rid).join("Release").join(rid).join("publish");
        vec![
            publish.join("Mesen.app/Contents/MacOS/Mesen"),
            publish.join("Mesen"),
            source.join(rid).join("Release/Mesen"),
        ]
    }
    #[cfg(target_os = "linux")]
    {
        vec![
            source
                .join(rid)
                .join("Release")
                .join(rid)
                .join("publish/Mesen"),
            source.join(rid).join("Release/Mesen"),
        ]
    }
    #[cfg(windows)]
    {
        vec![source.join("win-x64").join("Release/Mesen.exe")]
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = source;
        Vec::new()
    }
}

/// Resolve the Mesen executable: explicit override, repository build, OS install, then PATH.
/// Compatibility is checked separately so an incompatible binary can be reported as patch-required rather
/// than being confused with a missing executable.
pub fn resolve_binary(root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("MESEN_BIN") {
        let p = PathBuf::from(explicit);
        if super::is_runnable_file(&p) {
            return Some(p);
        }
        if let Some(binary) = app_bundle_executable(&p) {
            if super::is_runnable_file(&binary) {
                return Some(binary);
            }
        }
    }
    if let Some(local) = super::first_existing_file(local_build_candidates(root)) {
        return Some(local);
    }
    if let Some(default) = super::first_existing_file(default_install_candidates()) {
        return Some(default);
    }
    let exe = if cfg!(windows) { "Mesen.exe" } else { "Mesen" };
    super::find_on_path(exe)
}

fn app_bundle_executable(path: &Path) -> Option<PathBuf> {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        .then(|| path.join("Contents/MacOS/Mesen"))
}

pub fn default_install_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from(
            "/Applications/Mesen.app/Contents/MacOS/Mesen",
        )]
    }
    #[cfg(windows)]
    {
        let mut candidates = Vec::new();
        for key in [
            "LOCALAPPDATA",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "USERPROFILE",
        ] {
            if let Some(base) = std::env::var_os(key).map(PathBuf::from) {
                candidates.push(base.join("Programs/Mesen/Mesen.exe"));
                candidates.push(base.join("Mesen/Mesen.exe"));
            }
        }
        candidates
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPortable {
    pub binary: PathBuf,
    pub settings: PathBuf,
    pub home: PathBuf,
}

fn app_bundle_root(binary: &Path) -> Option<(&Path, PathBuf)> {
    for ancestor in binary.ancestors() {
        if ancestor
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        {
            let rel = binary.strip_prefix(ancestor).ok()?.to_path_buf();
            return Some((ancestor, rel));
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn isolate_app_bundle_identity(app_root: &Path, port: u16) -> std::io::Result<()> {
    let info_plist = app_root.join("Contents/Info.plist");
    if super::has_symlink_component_under(app_root, &info_plist) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "portable Mesen Info.plist contains a symlink, refusing to modify it: {}",
                info_plist.display()
            ),
        ));
    }
    if !info_plist.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "portable Mesen app is missing Contents/Info.plist: {}",
                app_root.display()
            ),
        ));
    }

    // LaunchServices and macOS saved-state storage key off this identifier. Sharing upstream's
    // identifier can strand a rapid relaunch behind another port's or the user's open Mesen app.
    let identifier = format!("ca.mesen.emucap.p{port}");
    let output = std::process::Command::new("/usr/bin/plutil")
        .args(["-replace", "CFBundleIdentifier", "-string", &identifier])
        .arg(&info_plist)
        .output()
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "failed to run plutil for portable Mesen identity at {}: {e}",
                    info_plist.display()
                ),
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(std::io::Error::other(format!(
            "failed to set portable Mesen bundle identifier at {}: {}",
            info_plist.display(),
            stderr.trim()
        )));
    }
    Ok(())
}

/// Copy Mesen into an emucap-owned portable directory and locate its optional settings path. The
/// source binary/app is read-only input; system-specific launch preparation decides whether a
/// settings file is needed.
pub fn prepare_portable_binary(
    source_binary: &Path,
    port: u16,
) -> std::io::Result<PreparedPortable> {
    let home = super::emu_home_dir("mesen2", port);
    std::fs::create_dir_all(&home)?;

    let (binary, settings) = if source_binary.starts_with(&home) {
        if super::has_symlink_component_under(&home, source_binary) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "portable Mesen binary path contains a symlink, refusing to launch: {}",
                    source_binary.display()
                ),
            ));
        }
        let settings = source_binary
            .parent()
            .unwrap_or(&home)
            .join("settings.json");
        (source_binary.to_path_buf(), settings)
    } else if let Some((app_root, rel)) = app_bundle_root(source_binary) {
        let app_name = app_root.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Mesen app path: {}", app_root.display()),
            )
        })?;
        let dst_root = home.join(app_name);
        super::copy_dir_replace(app_root, &dst_root)?;
        let binary = dst_root.join(rel);
        let settings = binary.parent().unwrap_or(&dst_root).join("settings.json");
        (binary, settings)
    } else if build_metadata_path(source_binary).is_file() {
        let exe_name = source_binary.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Mesen binary path: {}", source_binary.display()),
            )
        })?;
        let source_dir = source_binary.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid Mesen publish directory: {}",
                    source_binary.display()
                ),
            )
        })?;
        let dst_dir = home.join("portable");
        if dst_dir.starts_with(source_dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "portable Mesen destination must not be inside its source publish directory: {}",
                    dst_dir.display()
                ),
            ));
        }
        super::copy_dir_replace(source_dir, &dst_dir)?;
        let binary = dst_dir.join(exe_name);
        let settings = dst_dir.join("settings.json");
        (binary, settings)
    } else {
        let exe_name = source_binary.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Mesen binary path: {}", source_binary.display()),
            )
        })?;
        let dst_dir = home.join("portable");
        let binary = dst_dir.join(exe_name);
        super::copy_file_replace(source_binary, &binary)?;
        let settings = dst_dir.join("settings.json");
        (binary, settings)
    };

    #[cfg(target_os = "macos")]
    if let Some((app_root, _)) = app_bundle_root(&binary) {
        isolate_app_bundle_identity(app_root, port)?;
    }

    // launch() installs the per-port portable marker after validating this
    // copied runtime, preserving any regular settings file bundled with it.
    Ok(PreparedPortable {
        binary,
        settings,
        home,
    })
}

/// Everything the launcher needs to bring up Mesen for emucap. `binary`/`lua` are resolved paths;
/// `content` is the ROM; `port` is the MCP's listening_port.
pub struct Launch<'a> {
    pub binary: &'a Path,
    pub content: &'a str,
    pub lua: &'a Path,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
}

/// Prepare an emucap-owned portable Mesen copy and launch it detached with the ROM + adapter Lua and
/// the emucap environment. Returns the child pid.
pub fn launch(l: &Launch) -> std::io::Result<u32> {
    let host_build = read_build_metadata(l.binary)?;
    let portable = prepare_portable_binary(l.binary, l.port)?;
    ensure_portable_settings(&portable)?;
    provision_gba_bios(l, &portable)?;
    let opts = crate::launch::spec::SpecOpts {
        content: l.content,
        port: l.port,
        name: l.name,
        session_token: l.session_token,
        runtime: l.runtime,
        headless: false, // Mesen renders a GUI window; there is no headless mode.
    };
    let spec = crate::launch::spec::mesen_spec(&portable.binary, l.log_path, l.lua, &opts)
        .env("EMUCAP_MESEN_UPSTREAM_COMMIT", &host_build.commit)
        .env("EMUCAP_MESEN_PATCHSET_SHA256", &host_build.patchset_sha256);
    let pid = crate::launch::spawn_detached(&spec)?;
    // Keep the macOS display awake for the HITL window and reap the helper (no-op off macOS).
    crate::launch::spawn_display_caffeinate(pid);
    Ok(pid)
}

const PORTABLE_SETTINGS: &str = r#"{
  "Debug": {
    "ScriptWindow": {
      "AllowIoOsAccess": true,
      "AllowNetworkAccess": true,
      "ScriptTimeout": 60
    }
  },
  "Preferences": {
    "SingleInstance": false
  }
}
"#;

fn is_gba_launch(l: &Launch) -> bool {
    l.lua.file_name().and_then(|name| name.to_str()) == Some("emucap-gba.lua")
        || Path::new(l.content)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gba"))
}

/// A settings.json beside the executable is Mesen's portable-data marker. It keeps configuration and
/// native-library lookup in the per-port copy; for GBA it also makes the staged Firmware directory
/// the lookup path. An existing regular file copied from the source app is preserved verbatim.
fn ensure_portable_settings(portable: &PreparedPortable) -> std::io::Result<()> {
    if super::has_symlink_component_under(&portable.home, &portable.settings) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "portable Mesen settings path contains a symlink, refusing to write: {}",
                portable.settings.display()
            ),
        ));
    }
    if portable.settings.is_file() {
        return Ok(());
    }
    if portable.settings.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "portable Mesen settings path is not a regular file: {}",
                portable.settings.display()
            ),
        ));
    }
    if let Some(parent) = portable.settings.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = super::unique_sibling_path(&portable.settings, "tmp");
    if let Err(err) = std::fs::write(&tmp, PORTABLE_SETTINGS.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    super::rename_file_tmp(&tmp, &portable.settings)
}

/// The default GBA BIOS source when `EMUCAP_GBA_BIOS` is unset: the emucap-owned firmware directory
/// `<emucap-home>/firmware/gba_bios.bin`. This is the shared firmware dir alongside the per-emulator
/// homes (matching the docs and legacy launcher's `$EMUCAP_MESEN_BASE/firmware/gba_bios.bin`), NOT
/// under `mesen2/<port>` — a per-port location would never be found at the documented path.
fn default_gba_bios_source() -> PathBuf {
    super::emu_home_base().join("firmware/gba_bios.bin")
}

/// GBA needs a real BIOS (gba_bios.bin), which Mesen looks for in its data folder's `Firmware`
/// directory; without it Mesen pops a firmware prompt that breaks the headless/agent flow, and a
/// freshly-copied portable starts with an empty Firmware. When launching the GBA entry script, stage
/// the BIOS into the portable Firmware directory first. Source order: explicit `EMUCAP_GBA_BIOS`,
/// else `default_gba_bios_source()`. When no explicit source is set and the destination BIOS is
/// already staged (a prior run), accept it rather than failing if the shared source has since gone.
/// A missing *explicitly-configured* source fails fast with a clear precondition instead of hanging
/// on the prompt.
fn provision_gba_bios(l: &Launch, portable: &PreparedPortable) -> std::io::Result<()> {
    if !is_gba_launch(l) {
        return Ok(());
    }
    let firmware = portable
        .binary
        .parent()
        .ok_or_else(|| {
            std::io::Error::other("cannot locate the portable Mesen Firmware directory")
        })?
        .join("Firmware");
    let dst = firmware.join("gba_bios.bin");

    let explicit = std::env::var_os("EMUCAP_GBA_BIOS");
    // No explicit source + an already-staged BIOS: honour the staged copy so a launch does not fail
    // when the shared firmware source has been moved/removed since the first run.
    if explicit.is_none()
        && dst
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0x4000)
    {
        return Ok(());
    }
    let src = match explicit {
        Some(p) => PathBuf::from(p),
        None => default_gba_bios_source(),
    };
    if !src.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "GBA needs a real BIOS (gba_bios.bin): set EMUCAP_GBA_BIOS to it, or place it at {}. \
                 Without it Mesen would pop a firmware prompt and break the headless launch.",
                src.display()
            ),
        ));
    }
    let size = std::fs::metadata(&src)?.len();
    if size != 0x4000 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "GBA BIOS must be exactly 16384 bytes, but {} is {size} bytes",
                src.display()
            ),
        ));
    }
    std::fs::create_dir_all(&firmware)?;
    super::copy_file_replace(&src, &dst)
}

#[cfg(test)]
#[path = "mesen_tests.rs"]
mod tests;
