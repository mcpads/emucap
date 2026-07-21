//! Isolated MAME launcher for the Neo Geo MVS adapter.

use super::{
    emu_home_dir, find_on_path, spawn_detached, terminate_detached, LaunchSpec, RuntimeEnv,
};
use std::path::{Path, PathBuf};

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub bridge: &'a Path,
    pub repo_root: &'a Path,
    pub content: &'a Path,
    pub bios: &'a Path,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub mame_pid: u32,
    pub bridge_pid: u32,
    pub gdb_port: u16,
    pub driver: String,
}

pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    super::mame::resolve_binary(repo_root)
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

pub fn default_bios_candidates(content: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if content.file_name().and_then(|v| v.to_str()) == Some("neogeo.zip") {
        candidates.push(content.to_path_buf());
    }
    if let Some(parent) = content.parent() {
        candidates.push(parent.join("neogeo.zip"));
    }
    for home in [
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        candidates.push(home.join("mame/roms/neogeo.zip"));
        candidates.push(home.join(".config/retroarch/system/neogeo.zip"));
        candidates.push(home.join("Library/Application Support/RetroArch/system/neogeo.zip"));
    }
    candidates
}

pub fn resolve_bios(content: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_NEOGEO_BIOS") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    default_bios_candidates(content)
        .into_iter()
        .find(|path| path.is_file())
}

pub fn mvs_driver(content: &Path) -> std::io::Result<String> {
    if !content.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Neo Geo MVS ROM set not found: {}", content.display()),
        ));
    }
    if !content
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("zip"))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Neo Geo MVS content must be a MAME .zip ROM set",
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
            format!("invalid MAME driver name from content: {driver:?}"),
        ));
    }
    Ok(driver)
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
    let home = emu_home_dir("mame-neogeo", launch.port);
    let home_text = home.to_string_lossy();
    let pluginspath = launch.repo_root.join("adapters/mame-pc98/plugins");
    let mut args = vec![
        driver.into(),
        "-rompath".into(),
        rompath(launch.content, launch.bios)?,
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
    ];
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
    .env("EMUCAP_MAME_PROFILE", "neogeo")
    .env(
        "EMUCAP_CONTENT",
        launch.content.to_string_lossy().into_owned(),
    );
    if !launch.display {
        spec = spec.env("SDL_VIDEODRIVER", "dummy");
    }
    spec = spec.runtime_env(launch.runtime);
    Ok(spec)
}

fn bridge_spec(launch: &Launch<'_>, gdb_port: u16) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.bridge, launch.log_path)
        .arg(launch.port.to_string())
        .arg("neogeo_mvs")
        .arg(format!("127.0.0.1:{gdb_port}"))
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
    spec.runtime_env(launch.runtime)
}

pub fn launch(launch: &Launch<'_>) -> std::io::Result<Launched> {
    let driver = mvs_driver(launch.content)?;
    if launch.bios.file_name().and_then(|v| v.to_str()) != Some("neogeo.zip")
        || !launch.bios.is_file()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Neo Geo MVS BIOS must be an existing neogeo.zip: {}",
                launch.bios.display()
            ),
        ));
    }
    let port = gdb_port(launch.port)?;
    let home = emu_home_dir("mame-neogeo", launch.port);
    for dir in ["cfg", "nvram", "inp", "sta", "snap", "diff", "comments"] {
        std::fs::create_dir_all(home.join(dir))?;
    }
    let mame_pid = spawn_detached(&mame_spec(launch, &driver, port)?)?;
    let bridge = match bridge_spec(launch, port).emulator_dependency(mame_pid) {
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
