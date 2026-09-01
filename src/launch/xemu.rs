//! Managed original Xbox launch through the pinned xemu fork.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{
    has_symlink_component_under, process_alive, spawn_detached, terminate_detached, LaunchSpec,
    RuntimeEnv,
};

mod build;
mod files;

pub use build::{
    build_metadata_path, local_build_candidates, read_build_metadata, require_compatible_build,
    resolve_binary, resolve_bridge, BuildMetadata, REQUIRED_HOST_API,
};
pub use files::{
    identity_for_regular_file, resolve_firmware, validate_firmware_root, FileIdentity,
    FirmwareInventory,
};

#[cfg(test)]
use build::lock_value;
use files::copy_verified;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedGeneration {
    pub runtime_home: PathBuf,
    pub log: PathBuf,
    pub settings: PathBuf,
    pub screenshots: PathBuf,
    pub mcpx: PathBuf,
    pub flash: PathBuf,
    pub hdd: PathBuf,
    pub eeprom: PathBuf,
    pub mcpx_identity: FileIdentity,
    pub flash_identity: FileIdentity,
    pub hdd_template_identity: FileIdentity,
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub bridge: &'a Path,
    pub content: &'a Path,
    pub firmware: &'a FirmwareInventory,
    pub host_build: &'a BuildMetadata,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    pub display: bool,
    pub sound: bool,
    pub start_frozen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub xemu_pid: u32,
    pub bridge_pid: u32,
    pub qmp_port: u16,
    pub gdb_port: u16,
    pub runtime_home: PathBuf,
    pub log: PathBuf,
    pub settings: PathBuf,
    pub hdd: PathBuf,
    pub eeprom: PathBuf,
    pub mcpx_identity: FileIdentity,
    pub flash_identity: FileIdentity,
    pub hdd_template_identity: FileIdentity,
    pub eeprom_initial_identity: FileIdentity,
}

fn validate_content(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Xbox content path must be absolute",
        ));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "iso" | "xiso") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Xbox managed launch accepts raw .iso or .xiso content only",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Xbox content must be a regular non-symlink file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn toml_string(path: &Path) -> io::Result<String> {
    serde_json::to_string(&path.to_string_lossy().into_owned())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn generation_name(runtime: Option<RuntimeEnv<'_>>) -> io::Result<String> {
    let name = runtime
        .map(|runtime| runtime.launch_id.to_string())
        .unwrap_or_else(|| format!("manual-{}", ulid::Ulid::generate()));
    if name.is_empty()
        || name.len() > 96
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "xemu launch generation id contains unsafe characters",
        ));
    }
    Ok(name)
}

pub fn prepare_generation(launch: &Launch<'_>) -> io::Result<PreparedGeneration> {
    prepare_generation_under(launch, &super::emu_home_base())
}

fn prepare_generation_under(
    launch: &Launch<'_>,
    emu_home_base: &Path,
) -> io::Result<PreparedGeneration> {
    validate_content(launch.content)?;
    let base = emu_home_base.join("xemu").join(launch.port.to_string());
    let generations = base.join("generations");
    fs::create_dir_all(&generations)?;
    if has_symlink_component_under(emu_home_base, &generations) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "xemu managed generation root contains a symlink",
        ));
    }
    let runtime_home = generations.join(generation_name(launch.runtime)?);
    fs::create_dir(&runtime_home)?;
    let machine = runtime_home.join("machine");
    let screenshots = runtime_home.join("screenshots");
    fs::create_dir(&machine)?;
    fs::create_dir(&screenshots)?;
    let mcpx = machine.join("mcpx.bin");
    let flash = machine.join("flash.bin");
    let hdd = machine.join("hdd.qcow2");
    let eeprom = machine.join("eeprom.bin");
    copy_verified(&launch.firmware.mcpx, &mcpx, &launch.firmware.mcpx_identity)?;
    copy_verified(
        &launch.firmware.flash,
        &flash,
        &launch.firmware.flash_identity,
    )?;
    copy_verified(
        &launch.firmware.hdd_template,
        &hdd,
        &launch.firmware.hdd_template_identity,
    )?;
    for path in [&mcpx, &flash] {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)?;
    }
    if let (Some(source), Some(identity)) = (
        launch.firmware.eeprom_template.as_deref(),
        launch.firmware.eeprom_template_identity.as_ref(),
    ) {
        copy_verified(source, &eeprom, identity)?;
    }
    let settings = runtime_home.join("xemu.toml");
    let log = runtime_home.join("xemu.log");
    let volume_limit = if launch.sound { 1.0 } else { 0.0 };
    let config = format!(
        "[general]\nshow_welcome = false\n\n[general.updates]\ncheck = false\n\n[audio]\nvolume_limit = {volume_limit:.1}\n\n[input.bindings]\nport1 = \"keyboard\"\n\n[sys.files]\nbootrom_path = {}\nflashrom_path = {}\neeprom_path = {}\nhdd_path = {}\n",
        toml_string(&mcpx)?,
        toml_string(&flash)?,
        toml_string(&eeprom)?,
        toml_string(&hdd)?,
    );
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&settings)?
        .write_all(config.as_bytes())?;
    Ok(PreparedGeneration {
        runtime_home,
        log,
        settings,
        screenshots,
        mcpx,
        flash,
        hdd,
        eeprom,
        mcpx_identity: launch.firmware.mcpx_identity.clone(),
        flash_identity: launch.firmware.flash_identity.clone(),
        hdd_template_identity: launch.firmware.hdd_template_identity.clone(),
    })
}

#[derive(Debug)]
struct BackendPorts {
    qmp_port: u16,
    gdb_port: u16,
    _qmp_reservation: TcpListener,
    _gdb_reservation: TcpListener,
}

fn reserve_backend_ports() -> io::Result<BackendPorts> {
    let qmp = TcpListener::bind("127.0.0.1:0")?;
    let gdb = TcpListener::bind("127.0.0.1:0")?;
    Ok(BackendPorts {
        qmp_port: qmp.local_addr()?.port(),
        gdb_port: gdb.local_addr()?.port(),
        _qmp_reservation: qmp,
        _gdb_reservation: gdb,
    })
}

fn emulator_spec(
    launch: &Launch<'_>,
    prepared: &PreparedGeneration,
    qmp_port: u16,
    gdb_port: u16,
) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.binary, &prepared.log)
        .arg("-config_path")
        .arg(prepared.settings.to_string_lossy())
        .arg("-dvd_path")
        .arg(launch.content.to_string_lossy())
        .arg("-gdb")
        .arg(format!("tcp:127.0.0.1:{gdb_port}"))
        .arg("-qmp")
        .arg(format!("tcp:127.0.0.1:{qmp_port},server=on,wait=off"))
        .arg("-S")
        .env(
            "EMUCAP_XEMU_SCREEN_ROOT",
            prepared.screenshots.to_string_lossy(),
        );
    if !launch.display {
        spec = spec.arg("-emucap-hidden");
    }
    spec
}

fn bridge_spec(
    launch: &Launch<'_>,
    prepared: &PreparedGeneration,
    qmp_port: u16,
    gdb_port: u16,
    eeprom_identity: &FileIdentity,
) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.bridge, &prepared.log)
        .arg(launch.port.to_string())
        .arg(format!("127.0.0.1:{qmp_port}"))
        .arg(format!("127.0.0.1:{gdb_port}"))
        .env("EMUCAP_CONTENT", launch.content.to_string_lossy())
        .env(
            "EMUCAP_XEMU_SCREEN_ROOT",
            prepared.screenshots.to_string_lossy(),
        )
        .env(
            "EMUCAP_START_FROZEN",
            if launch.start_frozen { "1" } else { "0" },
        )
        .env("EMUCAP_XEMU_MCPX_SHA256", &prepared.mcpx_identity.sha256)
        .env("EMUCAP_XEMU_FLASH_SHA256", &prepared.flash_identity.sha256)
        .env(
            "EMUCAP_XEMU_HDD_TEMPLATE_SHA256",
            &prepared.hdd_template_identity.sha256,
        )
        .env("EMUCAP_XEMU_EEPROM_INITIAL_SHA256", &eeprom_identity.sha256)
        .env("EMUCAP_XEMU_HDD_PATH", prepared.hdd.to_string_lossy())
        .env("EMUCAP_XEMU_EEPROM_PATH", prepared.eeprom.to_string_lossy())
        .env("EMUCAP_XEMU_HOST_UPSTREAM", &launch.host_build.upstream)
        .env("EMUCAP_XEMU_HOST_TAG", &launch.host_build.tag)
        .env("EMUCAP_XEMU_HOST_COMMIT", &launch.host_build.commit)
        .env(
            "EMUCAP_XEMU_HOST_API",
            launch.host_build.host_api.to_string(),
        )
        .env(
            "EMUCAP_XEMU_HOST_PATCHSET_SHA256",
            &launch.host_build.patchset_sha256,
        )
        .env(
            "EMUCAP_XEMU_HOST_BINARY_SHA256",
            &launch.host_build.binary_sha256,
        )
        .runtime_env(launch.runtime);
    if let Some(name) = launch.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = launch.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec
}

fn wait_for_eeprom(pid: u32, path: &Path, timeout: Duration) -> io::Result<FileIdentity> {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pid) {
            return Err(io::Error::other(
                "xemu exited before preparing its generation EEPROM; check the launch log",
            ));
        }
        match identity_for_regular_file(path) {
            Ok(identity) if identity.size == 256 => return Ok(identity),
            Ok(identity) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "xemu generation EEPROM must be 256 bytes, got {}",
                        identity.size
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "xemu did not generate its EEPROM within the startup deadline",
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

fn wait_survives(pid: u32, duration: Duration, message: &str) -> io::Result<()> {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return Err(io::Error::other(message.to_string()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn write_pidfile(runtime_home: &Path, name: &str, pid: u32) {
    let _ = fs::write(runtime_home.join(name), format!("{pid}\n"));
}

pub fn launch(launch: &Launch<'_>) -> io::Result<Launched> {
    let prepared = prepare_generation(launch)?;
    let ports = reserve_backend_ports()?;
    let qmp_port = ports.qmp_port;
    let gdb_port = ports.gdb_port;
    drop(ports);
    if launch.display {
        super::wake_display_before_gui_launch();
    }
    let xemu_pid = spawn_detached(&emulator_spec(launch, &prepared, qmp_port, gdb_port))?;
    write_pidfile(&prepared.runtime_home, "xemu.pid", xemu_pid);
    let eeprom_initial_identity =
        match wait_for_eeprom(xemu_pid, &prepared.eeprom, Duration::from_secs(10)) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = terminate_detached(xemu_pid);
                return Err(error);
            }
        };
    if launch.display {
        super::spawn_display_caffeinate(xemu_pid);
    }
    let bridge = match bridge_spec(
        launch,
        &prepared,
        qmp_port,
        gdb_port,
        &eeprom_initial_identity,
    )
    .emulator_dependency(xemu_pid)
    {
        Ok(spec) => spec,
        Err(error) => {
            let _ = terminate_detached(xemu_pid);
            return Err(error);
        }
    };
    let bridge_pid = match spawn_detached(&bridge) {
        Ok(pid) => pid,
        Err(error) => {
            let _ = terminate_detached(xemu_pid);
            return Err(error);
        }
    };
    if let Err(error) = wait_survives(
        bridge_pid,
        Duration::from_secs(2),
        "emucap-xemu-bridge exited during startup; check the launch log and pinned host build",
    ) {
        let _ = terminate_detached(bridge_pid);
        let _ = terminate_detached(xemu_pid);
        return Err(error);
    }
    write_pidfile(&prepared.runtime_home, "bridge.pid", bridge_pid);
    Ok(Launched {
        xemu_pid,
        bridge_pid,
        qmp_port,
        gdb_port,
        runtime_home: prepared.runtime_home,
        log: prepared.log,
        settings: prepared.settings,
        hdd: prepared.hdd,
        eeprom: prepared.eeprom,
        mcpx_identity: prepared.mcpx_identity,
        flash_identity: prepared.flash_identity,
        hdd_template_identity: prepared.hdd_template_identity,
        eeprom_initial_identity,
    })
}

#[cfg(test)]
#[path = "xemu_tests.rs"]
mod tests;
