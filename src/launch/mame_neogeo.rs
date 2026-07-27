//! Isolated MAME launcher for the Neo Geo MVS, AES, and CD adapters.

use super::{
    emu_home_dir, find_on_path, first_existing_file, spawn_detached, terminate_detached,
    LaunchSpec, RuntimeEnv,
};
use std::path::{Path, PathBuf};

const MAX_SOFTWARE_LIST_BYTES: u64 = 16 * 1024 * 1024;

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub bridge: &'a Path,
    pub repo_root: &'a Path,
    pub content: &'a Path,
    pub bios: &'a Path,
    pub system: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    pub display: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NeoGeoSystem {
    Mvs,
    Aes,
    Cd,
}

impl NeoGeoSystem {
    fn parse(system: &str) -> std::io::Result<Self> {
        match system {
            "neogeo_mvs" => Ok(Self::Mvs),
            "neogeo_aes" => Ok(Self::Aes),
            "neogeo_cd" => Ok(Self::Cd),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported Neo Geo system: {system}"),
            )),
        }
    }

    fn bios_name(self) -> &'static str {
        match self {
            Self::Mvs => "neogeo.zip",
            Self::Aes => "aes.zip",
            Self::Cd => "neocdz.zip",
        }
    }

    fn driver(self, content: &Path) -> std::io::Result<String> {
        match self {
            Self::Mvs => mvs_driver(content),
            Self::Aes => {
                aes_software_name(content)?;
                Ok("aes".into())
            }
            Self::Cd => {
                crate::cue::validate_graph(content)?;
                Ok("neocdz".into())
            }
        }
    }

    fn home_component(self) -> &'static str {
        match self {
            Self::Mvs => "mame-neogeo",
            Self::Aes => "mame-neogeo-aes",
            Self::Cd => "mame-neogeo-cd",
        }
    }

    fn plugin_profile(self) -> &'static str {
        match self {
            Self::Mvs => "neogeo_mvs",
            Self::Aes => "neogeo_aes",
            Self::Cd => "neogeo_cd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub mame_pid: u32,
    pub bridge_pid: u32,
    pub gdb_port: u16,
    pub driver: String,
}

pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    for key in ["EMUCAP_NEOGEO_MAME_BIN", "MAME_BIN"] {
        if let Some(explicit) = std::env::var_os(key).map(PathBuf::from) {
            if super::is_runnable_file(&explicit) {
                return Some(explicit);
            }
        }
    }
    if let Some(local) = repo_local_binary(repo_root) {
        return Some(local);
    }
    if let Some(default) = first_existing_file(super::mame::default_install_candidates()) {
        return Some(default);
    }
    find_on_path("mame")
}

fn repo_local_binary(repo_root: &Path) -> Option<PathBuf> {
    let work = repo_root.join("adapters/mame-neogeo/work");
    let name = if cfg!(windows) { "mame.exe" } else { "mame" };
    let path = work.join(name);
    super::is_runnable_file(&path).then_some(path)
}

pub fn resolve_bridge(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_NEOGEO_BRIDGE_BIN") {
        let path = PathBuf::from(explicit);
        return super::is_runnable_file(&path).then_some(path);
    }
    let name = if cfg!(windows) {
        "emucap-mame-neogeo-bridge.exe"
    } else {
        "emucap-mame-neogeo-bridge"
    };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join(name));
        }
    }
    candidates.push(repo_root.join("target/release").join(name));
    candidates.push(repo_root.join("target/debug").join(name));
    candidates
        .into_iter()
        .find(|path| super::is_runnable_file(path))
        .or_else(|| find_on_path(name))
}

pub fn default_bios_candidates(content: &Path, system: &str) -> Vec<PathBuf> {
    let Ok(system) = NeoGeoSystem::parse(system) else {
        return Vec::new();
    };
    let bios_name = system.bios_name();
    let mut candidates = Vec::new();
    if content.file_name().and_then(|v| v.to_str()) == Some(bios_name) {
        candidates.push(content.to_path_buf());
    }
    if let Some(parent) = content.parent() {
        candidates.push(parent.join(bios_name));
    }
    for home in [
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        candidates.push(home.join("mame/roms").join(bios_name));
        candidates.push(home.join(".config/retroarch/system").join(bios_name));
        candidates.push(
            home.join("Library/Application Support/RetroArch/system")
                .join(bios_name),
        );
    }
    candidates
}

pub fn resolve_bios(content: &Path, system: &str) -> Option<PathBuf> {
    let env_key = match NeoGeoSystem::parse(system).ok()? {
        NeoGeoSystem::Mvs => "EMUCAP_NEOGEO_BIOS",
        NeoGeoSystem::Aes => "EMUCAP_NEOGEO_AES_BIOS",
        NeoGeoSystem::Cd => "EMUCAP_NEOGEO_CD_BIOS",
    };
    if let Some(explicit) = std::env::var_os(env_key) {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    default_bios_candidates(content, system)
        .into_iter()
        .find(|path| path.is_file())
}

pub fn mvs_driver(content: &Path) -> std::io::Result<String> {
    zip_stem(content, "MVS ROM set", "MAME driver")
}

pub fn aes_software_name(content: &Path) -> std::io::Result<String> {
    zip_stem(content, "AES cartridge software-list set", "MAME software")
}

fn zip_stem(content: &Path, media_name: &str, stem_name: &str) -> std::io::Result<String> {
    if !content.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Neo Geo {media_name} not found: {}", content.display()),
        ));
    }
    if !content
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("zip"))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Neo Geo {media_name} must be a MAME .zip set"),
        ));
    }
    let driver = content
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if driver.is_empty()
        || !driver
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid {stem_name} name from content: {driver:?}"),
        ));
    }
    Ok(driver)
}

pub fn resolve_aes_hash_path(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_NEOGEO_HASH_PATH") {
        let path = PathBuf::from(explicit);
        return path.join("neogeo.xml").is_file().then_some(path);
    }
    [
        repo_root.join("adapters/mame-neogeo/work/hash"),
        repo_root.join("adapters/mame-neogeo/work/mame-src/hash"),
    ]
    .into_iter()
    .find(|path| path.join("neogeo.xml").is_file())
}

fn validate_aes_software(hash_path: &Path, software: &str) -> std::io::Result<()> {
    let list_path = hash_path.join("neogeo.xml");
    let metadata = std::fs::metadata(&list_path)?;
    if metadata.len() > MAX_SOFTWARE_LIST_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Neo Geo software list exceeds {} bytes: {}",
                MAX_SOFTWARE_LIST_BYTES,
                list_path.display()
            ),
        ));
    }
    let list = std::fs::read_to_string(&list_path)?;
    let entry_start = format!("<software name=\"{software}\"");
    let entry = list
        .split_once(&entry_start)
        .and_then(|(_, rest)| rest.split_once("</software>").map(|(entry, _)| entry))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Neo Geo software {software:?} is absent from {}",
                    list_path.display()
                ),
            )
        })?;
    let aes_compatible = entry.lines().any(|line| {
        let Some((_, after_name)) = line.split_once("name=\"compatibility\"") else {
            return false;
        };
        let Some((_, after_value)) = after_name.split_once("value=\"") else {
            return false;
        };
        let Some((value, _)) = after_value.split_once('"') else {
            return false;
        };
        value.split(',').any(|item| item.trim() == "AES")
    });
    if !aes_compatible {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Neo Geo software {software:?} is not marked AES-compatible in {}",
                list_path.display()
            ),
        ));
    }
    Ok(())
}

fn gdb_port(port: u16) -> std::io::Result<u16> {
    port.checked_add(1000).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("MAME GDB port would overflow for EMUCAP_PORT={port}"),
        )
    })
}

fn rompath(content: &Path, bios: &Path) -> std::io::Result<String> {
    let content_dir = content.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Neo Geo content has no parent directory",
        )
    })?;
    let bios_dir = bios.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Neo Geo BIOS has no parent directory",
        )
    })?;
    if content_dir == bios_dir {
        Ok(content_dir.to_string_lossy().into_owned())
    } else {
        Ok(format!(
            "{};{}",
            content_dir.to_string_lossy(),
            bios_dir.to_string_lossy()
        ))
    }
}

pub fn mame_spec(launch: &Launch<'_>, driver: &str, gdb_port: u16) -> std::io::Result<LaunchSpec> {
    let system = NeoGeoSystem::parse(launch.system)?;
    let home = emu_home_dir(system.home_component(), launch.port);
    let home_text = home.to_string_lossy();
    let pluginspath = launch.repo_root.join("adapters/mame-pc98/plugins");
    let mut args = vec![driver.into(), "-rompath".into()];
    args.push(match system {
        NeoGeoSystem::Mvs | NeoGeoSystem::Aes => rompath(launch.content, launch.bios)?,
        NeoGeoSystem::Cd => launch
            .bios
            .parent()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Neo Geo CD BIOS has no parent directory",
                )
            })?
            .to_string_lossy()
            .into_owned(),
    });
    if system == NeoGeoSystem::Cd {
        args.extend([
            "-bios".into(),
            "official".into(),
            "-cdrom".into(),
            launch.content.to_string_lossy().into_owned(),
        ]);
    } else if system == NeoGeoSystem::Aes {
        let hash_path = resolve_aes_hash_path(launch.repo_root).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Neo Geo AES software list neogeo.xml not found; build adapters/mame-neogeo or set EMUCAP_NEOGEO_HASH_PATH",
            )
        })?;
        let software = aes_software_name(launch.content)?;
        validate_aes_software(&hash_path, &software)?;
        args.extend([
            "-hashpath".into(),
            hash_path.to_string_lossy().into_owned(),
            "-bios".into(),
            "japan".into(),
            "-cart".into(),
            software,
        ]);
    }
    args.extend([
        "-homepath".into(),
        home_text.clone().into_owned(),
        "-cfg_directory".into(),
        format!("{home_text}/cfg"),
        "-nvram_directory".into(),
        format!("{home_text}/nvram"),
        "-input_directory".into(),
        format!("{home_text}/inp"),
        "-state_directory".into(),
        format!("{home_text}/sta"),
        "-snapshot_directory".into(),
        format!("{home_text}/snap"),
        "-diff_directory".into(),
        format!("{home_text}/diff"),
        "-comment_directory".into(),
        format!("{home_text}/comments"),
        "-skip_gameinfo".into(),
        "-debug".into(),
        "-debugger".into(),
        "none".into(),
        "-pluginspath".into(),
        pluginspath.to_string_lossy().into_owned(),
        "-plugins".into(),
        "-plugin".into(),
        "emucap_gdbstub".into(),
        "-noreadconfig".into(),
        "-window".into(),
        "-nomaximize".into(),
        "-sound".into(),
        "none".into(),
    ]);
    if !launch.display {
        args.extend(
            [
                "-video",
                "none",
                "-videodriver",
                "dummy",
                "-keyboardprovider",
                "none",
                "-mouseprovider",
                "none",
                "-output",
                "none",
            ]
            .map(String::from),
        );
    }
    let mut spec = LaunchSpec {
        program: launch.binary.into(),
        args,
        env: Vec::new(),
        log_path: launch.log_path.into(),
        cwd: None,
    }
    .env("MAME_GDB_PORT", gdb_port.to_string())
    .env("EMUCAP_MAME_PROFILE", system.plugin_profile())
    .env(
        "EMUCAP_CONTENT",
        launch.content.to_string_lossy().into_owned(),
    );
    if !launch.display {
        spec = spec.env("SDL_VIDEODRIVER", "dummy");
    } else {
        // The repo-local MAME binary is a fail-closed wrapper. Authorize its raw binary only for
        // an explicit display=true launch; otherwise it appends -video none after these arguments.
        spec = spec.env("MAME_ALLOW_VISIBLE", "1");
    }
    spec = spec.runtime_env(launch.runtime);
    Ok(spec)
}

fn bridge_spec(launch: &Launch<'_>, gdb_port: u16) -> std::io::Result<LaunchSpec> {
    let system = NeoGeoSystem::parse(launch.system)?;
    let adapter_home = launch
        .runtime
        .and_then(|runtime| runtime.adapter_failure_path.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| emu_home_dir(system.home_component(), launch.port));
    let mut spec = LaunchSpec::new(launch.bridge, launch.log_path)
        .arg(launch.port.to_string())
        .arg(launch.system)
        .arg(format!("127.0.0.1:{gdb_port}"))
        .env(
            "EMUCAP_ADAPTER_HOME",
            adapter_home.to_string_lossy().into_owned(),
        )
        .env(
            "EMUCAP_CONTENT",
            launch.content.to_string_lossy().into_owned(),
        );
    if let Some(name) = launch.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = launch.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    Ok(spec.runtime_env(launch.runtime))
}

pub fn launch(launch: &Launch<'_>) -> std::io::Result<Launched> {
    let system = NeoGeoSystem::parse(launch.system)?;
    let driver = system.driver(launch.content)?;
    if launch.bios.file_name().and_then(|v| v.to_str()) != Some(system.bios_name())
        || !launch.bios.is_file()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Neo Geo {} BIOS must be an existing {}: {}",
                match system {
                    NeoGeoSystem::Mvs => "MVS",
                    NeoGeoSystem::Aes => "AES",
                    NeoGeoSystem::Cd => "CDZ",
                },
                system.bios_name(),
                launch.bios.display()
            ),
        ));
    }
    let port = gdb_port(launch.port)?;
    let home = emu_home_dir(system.home_component(), launch.port);
    for dir in ["cfg", "nvram", "inp", "sta", "snap", "diff", "comments"] {
        std::fs::create_dir_all(home.join(dir))?;
    }
    let mame_pid = spawn_detached(&mame_spec(launch, &driver, port)?)?;
    let bridge = match bridge_spec(launch, port)?.emulator_dependency(mame_pid) {
        Ok(spec) => spec,
        Err(error) => {
            let _ = terminate_detached(mame_pid);
            return Err(error);
        }
    };
    let bridge_pid = match spawn_detached(&bridge) {
        Ok(pid) => pid,
        Err(error) => {
            let _ = terminate_detached(mame_pid);
            return Err(error);
        }
    };
    if launch.display {
        super::spawn_display_caffeinate(mame_pid);
    }
    Ok(Launched {
        mame_pid,
        bridge_pid,
        gdb_port: port,
        driver,
    })
}

#[cfg(test)]
#[path = "mame_neogeo_tests.rs"]
mod tests;
