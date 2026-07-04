use std::path::{Path, PathBuf};

use emucap::launch::{
    desmume_nds as desmume_nds_launch, flycast as flycast_launch, mame as mame_launch,
    mednafen as mednafen_launch, mesen as mesen_launch,
};
use emucap::live::link::{EmulatorIdentity, EmulatorLink};

use crate::args::{LaunchArgs, LaunchPlanArgs};
use crate::status::{
    button_hint_for_system, enrich_link_status, find_repo_root, make_bootstrap_value,
    runtime_paths, supported_systems_value,
};

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;

fn adapter_script_launcher(root: &Path, adapter: &str) -> PathBuf {
    let dir = match adapter {
        "mesen2" => "adapters/mesen2",
        "mednafen" => "adapters/mednafen",
        "mame_pc98" => "adapters/mame-pc98",
        "flycast" => "adapters/flycast",
        "desmume_nds" => "adapters/desmume-nds",
        _ => return root.join("adapters"),
    };
    let ps1 = root.join(dir).join("launch.ps1");
    if cfg!(windows) && ps1.exists() {
        ps1
    } else {
        root.join(dir).join("launch.sh")
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| path.to_string_lossy().replace('\\', "/");
        normalize(a).eq_ignore_ascii_case(&normalize(b))
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn path_matches_candidates(path: &Path, candidates: Vec<PathBuf>) -> bool {
    candidates
        .iter()
        .any(|candidate| same_path(path, candidate))
}

fn native_legacy_script(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    if cfg!(windows) {
        ext.is_some_and(|e| e.eq_ignore_ascii_case("ps1"))
    } else {
        ext == Some("sh")
    }
}

fn legacy_command(argv: &[String]) -> String {
    argv.iter()
        .map(|s| {
            if s.contains(' ') {
                format!("{s:?}")
            } else {
                s.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn legacy_fallback_details(launcher: &Path, argv: &[String]) -> serde_json::Value {
    let available = native_legacy_script(launcher);
    serde_json::json!({
        "available_on_this_host": available,
        "launcher": launcher.display().to_string(),
        "argv": if available {
            serde_json::json!(argv)
        } else {
            serde_json::Value::Null
        },
        "command": if available {
            serde_json::json!(legacy_command(argv))
        } else {
            serde_json::Value::Null
        },
        "reason": if available {
            "native script for this host"
        } else {
            "no native legacy script for this host; use preferred_launcher"
        },
    })
}

fn mednafen_binary_precondition_from(
    root: &Path,
    resolved: Option<(PathBuf, bool)>,
) -> serde_json::Value {
    match resolved {
        Some((path, explicit)) => {
            let repo_src = root.join("adapters/mednafen/work/mednafen/src");
            let source = if explicit {
                "MEDNAFEN_BIN"
            } else if path.starts_with(repo_src) {
                "repo_build"
            } else if path_matches_candidates(&path, mednafen_launch::default_install_candidates())
            {
                "default_install"
            } else {
                "PATH"
            };
            serde_json::json!({
                "available": true,
                "path": path.display().to_string(),
                "source": source,
                "explicit": explicit,
            })
        }
        None => serde_json::json!({
            "available": false,
            "source": null,
        }),
    }
}

fn mednafen_binary_precondition(root: &Path) -> serde_json::Value {
    mednafen_binary_precondition_from(root, mednafen_launch::resolve_binary(root))
}

fn simple_binary_precondition(
    resolved: Option<PathBuf>,
    source_for_path: impl Fn(&Path) -> &'static str,
) -> serde_json::Value {
    match resolved {
        Some(path) => serde_json::json!({
            "available": true,
            "path": path.display().to_string(),
            "source": source_for_path(&path),
        }),
        None => serde_json::json!({
            "available": false,
            "source": null,
        }),
    }
}

fn env_path_matches(key: &str, path: &Path) -> bool {
    std::env::var_os(key)
        .as_deref()
        .is_some_and(|p| same_path(Path::new(p), path))
}

fn env_path_or_app_matches(key: &str, path: &Path, exe_name: &str) -> bool {
    std::env::var_os(key).as_deref().is_some_and(|p| {
        let raw = Path::new(p);
        same_path(raw, path) || same_path(&raw.join("Contents/MacOS").join(exe_name), path)
    })
}

fn mesen_binary_precondition_from(resolved: Option<PathBuf>) -> serde_json::Value {
    simple_binary_precondition(resolved, |path| {
        if env_path_or_app_matches("MESEN_BIN", path, "Mesen") {
            "MESEN_BIN"
        } else if path_matches_candidates(path, mesen_launch::default_install_candidates()) {
            "default_install"
        } else {
            "PATH"
        }
    })
}

fn mesen_binary_precondition() -> serde_json::Value {
    mesen_binary_precondition_from(mesen_launch::resolve_binary())
}

fn flycast_binary_precondition_from(resolved: Option<PathBuf>) -> serde_json::Value {
    let build_home = flycast_launch::build_home();
    let legacy_home_build =
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("flycast/build"));
    simple_binary_precondition(resolved, |path| {
        if env_path_or_app_matches("FLYCAST_APP", path, "Flycast") {
            "FLYCAST_APP"
        } else if path.starts_with(&build_home) {
            "emucap_build"
        } else if legacy_home_build
            .as_ref()
            .is_some_and(|p| path.starts_with(p))
        {
            "legacy_home_build"
        } else if path_matches_candidates(path, flycast_launch::default_install_candidates()) {
            "default_install"
        } else {
            "PATH"
        }
    })
}

fn flycast_binary_precondition() -> serde_json::Value {
    flycast_binary_precondition_from(flycast_launch::resolve_binary())
}

fn mame_binary_precondition_from(root: &Path, resolved: Option<PathBuf>) -> serde_json::Value {
    let repo_work = root.join("adapters/mame-pc98/work");
    simple_binary_precondition(resolved, |path| {
        if env_path_matches("MAME_BIN", path) {
            "MAME_BIN"
        } else if path.starts_with(&repo_work) {
            "repo_build"
        } else if path_matches_candidates(path, mame_launch::default_install_candidates()) {
            "default_install"
        } else {
            "PATH"
        }
    })
}

fn mame_binary_precondition(root: &Path) -> serde_json::Value {
    mame_binary_precondition_from(root, mame_launch::resolve_binary(root))
}

/// DeSmuME/NDS needs two binaries — headless desmume-cli and the emucap NDS GDB bridge. Both must
/// resolve for the launcher to run, so the precondition is available only when both are present and
/// reports which one is missing otherwise.
fn desmume_nds_binary_precondition(root: &Path) -> serde_json::Value {
    let cli = desmume_nds_launch::resolve_binary(root);
    let bridge = desmume_nds_launch::resolve_bridge(root);
    match (cli, bridge) {
        (Some(cli), Some(bridge)) => serde_json::json!({
            "available": true,
            "path": cli.display().to_string(),
            "bridge": bridge.display().to_string(),
            "source": if std::env::var_os("EMUCAP_DESMUME_BIN").is_some() {
                "EMUCAP_DESMUME_BIN"
            } else {
                "repo_build"
            },
        }),
        (cli, bridge) => serde_json::json!({
            "available": false,
            "source": null,
            "desmume_cli_available": cli.is_some(),
            "bridge_available": bridge.is_some(),
        }),
    }
}

fn mame_bridge_precondition(root: &Path) -> serde_json::Value {
    match mame_launch::resolve_bridge_runtime(root) {
        Ok(runtime) => serde_json::json!({
            "available": true,
            "kind": runtime.kind,
            "program": runtime.program.display().to_string(),
            "script": runtime.script.map(|p| p.display().to_string()),
        }),
        Err(e) => serde_json::json!({
            "available": false,
            "error": e.to_string(),
            "source": "EMUCAP_PC98_BRIDGE / EMUCAP_PC98_BRIDGE_BIN / PATH",
        }),
    }
}

fn adapter_binary_precondition(adapter: &str, root: &Path) -> serde_json::Value {
    match adapter {
        "mesen2" => mesen_binary_precondition(),
        "mednafen" => mednafen_binary_precondition(root),
        "flycast" => flycast_binary_precondition(),
        "mame_pc98" => mame_binary_precondition(root),
        "desmume_nds" => desmume_nds_binary_precondition(root),
        _ => serde_json::Value::Null,
    }
}

fn build_required_precondition(
    adapter: &str,
    paths: &serde_json::Value,
    adapter_binary: &serde_json::Value,
) -> serde_json::Value {
    if adapter_binary["available"].as_bool().unwrap_or(false) {
        return serde_json::Value::Null;
    }
    match adapter {
        "mesen2" => serde_json::json!(format!(
            "{} 또는 MESEN_BIN/default install/PATH의 Mesen 실행파일 필요(macOS는 Mesen.app도 가능) — 미충족이면 launcher가 binary-not-found로 실패",
            paths["adapters"][adapter]["launch"]
                .as_str()
                .unwrap_or("adapter launcher")
        )),
        "mednafen" => serde_json::json!(format!(
            "{} 선행 빌드 또는 MEDNAFEN_BIN/default install/PATH의 Mednafen 바이너리 필요 — 미충족이면 launcher가 binary-not-found로 실패",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        "flycast" => serde_json::json!(format!(
            "{} 선행 빌드 또는 FLYCAST_APP/default install/PATH의 Flycast 바이너리 필요 — 미충족이면 launcher가 binary-not-found로 실패",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        "mame_pc98" => serde_json::json!(format!(
            "{} 선행 빌드 또는 MAME_BIN/default install/PATH의 MAME 바이너리 필요 — 미충족이면 launcher가 binary-not-found로 실패",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        "desmume_nds" => serde_json::json!(format!(
            "{} 선행 빌드(desmume-cli) + emucap-desmume-nds-bridge(cargo build --release) 필요 — 미충족이면 launcher가 binary-not-found로 실패",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        _ => serde_json::Value::Null,
    }
}

fn push_unavailable_precondition(
    blockers: &mut Vec<String>,
    label: &str,
    precondition: &serde_json::Value,
) {
    if !precondition.is_null() && !precondition["available"].as_bool().unwrap_or(false) {
        blockers.push(format!("{label} is unavailable"));
    }
}

fn launch_blockers(content_exists: bool, adapter_binary: &serde_json::Value) -> Vec<String> {
    let mut blockers = Vec::new();
    if !content_exists {
        blockers.push("content_path does not exist".to_string());
    }
    if !adapter_binary["available"].as_bool().unwrap_or(false) {
        blockers.push("adapter binary is unavailable".to_string());
    }
    blockers
}

fn missing_adapter_binary_response(
    adapter: &str,
    system: &str,
    port: u16,
    root: &Path,
    adapter_binary: serde_json::Value,
) -> serde_json::Value {
    let paths = runtime_paths(Some(port));
    let build_required = build_required_precondition(adapter, &paths, &adapter_binary);
    serde_json::json!({
        "launched": false,
        "reason": "adapter binary is unavailable",
        "system": system,
        "adapter": adapter,
        "preconditions": {
            "adapter_binary": adapter_binary,
            "build_required": build_required,
        },
        "runtime_paths": paths,
        "repo_root": root.display().to_string(),
        "next_action": "adapter binary precondition을 충족한 뒤 launch_plan(content_path, system)을 다시 호출하라",
    })
}

fn missing_mame_bridge_response(
    system: &str,
    port: u16,
    root: &Path,
    adapter_binary: serde_json::Value,
    bridge: serde_json::Value,
) -> serde_json::Value {
    let paths = runtime_paths(Some(port));
    let build_required = build_required_precondition("mame_pc98", &paths, &adapter_binary);
    serde_json::json!({
        "launched": false,
        "reason": "mame_pc98 bridge is unavailable",
        "system": system,
        "adapter": "mame_pc98",
        "preconditions": {
            "adapter_binary": adapter_binary,
            "bridge": bridge,
            "build_required": build_required,
        },
        "runtime_paths": paths,
        "repo_root": root.display().to_string(),
        "next_action": "PC-98 bridge precondition을 충족한 뒤 launch_plan(content_path, system)을 다시 호출하라",
    })
}

fn normalize_system(system: &str) -> Option<&'static str> {
    match system.trim().to_ascii_lowercase().as_str() {
        "snes" | "super-famicom" | "super-nintendo" | "mesen" | "mesen2" => Some("snes"),
        "gamegear" | "gg" | "game-gear" | "sms" | "mastersystem" | "master-system"
        | "sega-mastersystem" => Some("gamegear"),
        "gb" | "gameboy" | "game-boy" | "dmg" => Some("gb"),
        "gbc" | "gbcolor" | "gameboycolor" | "game-boy-color" | "cgb" => Some("gbc"),
        "gba" | "gameboyadvance" | "game-boy-advance" | "agb" => Some("gba"),
        "nes" | "nintendo" | "famicom" | "fc" => Some("nes"),
        "saturn" | "ss" | "sega-saturn" => Some("saturn"),
        "psx" | "ps1" | "playstation" | "playstation1" => Some("psx"),
        "pce" | "pcengine" | "pc-engine" | "pce-cd" | "pc-engine-cd" => Some("pce"),
        "md" | "genesis" | "megadrive" | "mega-drive" | "sega-genesis" | "sega-megadrive" => {
            Some("md")
        }
        "pc98" | "pc-98" | "mame-pc98" | "pc9801" | "pc9821" => Some("pc98"),
        "dc" | "dreamcast" | "flycast" | "sega-dreamcast" => Some("dc"),
        "nds" | "ds" | "nintendo-ds" | "nintendods" | "desmume" => Some("nds"),
        _ => None,
    }
}

fn ext_lower(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

fn read_prefix(path: &Path, max: usize) -> Option<Vec<u8>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0; max];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| {
        w.iter()
            .zip(needle.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

fn cue_file_refs(path: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    text.lines()
        .filter_map(|line| {
            let t = line.trim_start();
            if !t.to_ascii_uppercase().starts_with("FILE ") {
                return None;
            }
            let rest = t.get(5..)?.trim_start();
            let file_name = if let Some(after_quote) = rest.strip_prefix('"') {
                after_quote.split('"').next()
            } else {
                rest.split_whitespace().next()
            }?;
            Some(base.join(file_name))
        })
        .collect()
}

fn content_markers(path: Option<&str>) -> serde_json::Value {
    let Some(path) = path else {
        return serde_json::json!({"available": false});
    };
    let p = Path::new(path);
    let exists = p.exists();
    let mut markers = Vec::new();
    let mut scanned_files = Vec::new();
    let mut candidates = Vec::new();

    if ext_lower(path).as_deref() == Some("cue") {
        candidates.extend(cue_file_refs(p));
    } else {
        candidates.push(p.to_path_buf());
    }

    for candidate in candidates.into_iter().take(4) {
        if let Some(bytes) = read_prefix(&candidate, 1024 * 1024) {
            scanned_files.push(candidate.display().to_string());
            if contains_ascii_case_insensitive(&bytes, b"SEGA SEGASATURN") {
                markers.push("sega_saturn_header");
            }
            if contains_ascii_case_insensitive(&bytes, b"PLAYSTATION")
                || contains_ascii_case_insensitive(&bytes, b"SYSTEM.CNF")
            {
                markers.push("playstation_marker");
            }
            if contains_ascii_case_insensitive(&bytes, b"PC Engine") {
                markers.push("pc_engine_marker");
            }
            if contains_ascii_case_insensitive(&bytes, b"SEGA MEGA DRIVE")
                || contains_ascii_case_insensitive(&bytes, b"SEGA GENESIS")
            {
                markers.push("sega_megadrive_header");
            }
        }
    }

    markers.sort_unstable();
    markers.dedup();
    serde_json::json!({
        "available": exists,
        "scanned_files": scanned_files,
        "markers": markers,
    })
}

fn infer_system(content_path: Option<&str>, requested_system: Option<&str>) -> serde_json::Value {
    if let Some(system) = requested_system {
        if let Some(normalized) = normalize_system(system) {
            return serde_json::json!({
                "system": normalized,
                "confidence": "explicit",
                "reason": format!("system={system:?} was provided"),
                "needs_user_input": false,
            });
        }
        return serde_json::json!({
            "system": null,
            "confidence": "none",
            "reason": format!("unsupported system={system:?}"),
            "needs_user_input": true,
            "required_user_input": "지원 시스템 중 하나를 지정하라: snes, gamegear, saturn, psx, pce, md, pc98, dc"
        });
    }

    let Some(path) = content_path else {
        return serde_json::json!({
            "system": null,
            "confidence": "none",
            "reason": "content_path가 없어 media 기반 추론을 할 수 없다",
            "needs_user_input": true,
            "required_user_input": "실행할 ROM/disc/disk 경로와 시스템(snes/saturn/psx/pce/md/pc98/dc)을 알려줘야 한다"
        });
    };

    let markers = content_markers(Some(path));
    let has_marker = |name: &str| {
        markers
            .get("markers")
            .and_then(|v| v.as_array())
            .is_some_and(|items| items.iter().any(|v| v.as_str() == Some(name)))
    };

    if has_marker("sega_saturn_header") {
        return serde_json::json!({
            "system": "saturn",
            "confidence": "header",
            "reason": "media prefix contains SEGA SEGASATURN",
            "needs_user_input": false,
            "markers": markers,
        });
    }
    if has_marker("playstation_marker") {
        return serde_json::json!({
            "system": "psx",
            "confidence": "header",
            "reason": "media prefix contains PLAYSTATION/SYSTEM.CNF marker",
            "needs_user_input": false,
            "markers": markers,
        });
    }
    if has_marker("pc_engine_marker") {
        return serde_json::json!({
            "system": "pce",
            "confidence": "header",
            "reason": "media prefix contains PC Engine marker",
            "needs_user_input": false,
            "markers": markers,
        });
    }
    if has_marker("sega_megadrive_header") {
        return serde_json::json!({
            "system": "md",
            "confidence": "header",
            "reason": "media prefix contains SEGA MEGA DRIVE/SEGA GENESIS marker",
            "needs_user_input": false,
            "markers": markers,
        });
    }

    match ext_lower(path).as_deref() {
        Some("sfc" | "smc") => serde_json::json!({
            "system": "snes",
            "confidence": "extension",
            "reason": "SNES ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("gg" | "sms") => serde_json::json!({
            "system": "gamegear",
            "confidence": "extension",
            "reason": "Game Gear / Master System ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("gb") => serde_json::json!({
            "system": "gb",
            "confidence": "extension",
            "reason": "Game Boy ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("gbc") => serde_json::json!({
            "system": "gbc",
            "confidence": "extension",
            "reason": "Game Boy Color ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("gba") => serde_json::json!({
            "system": "gba",
            "confidence": "extension",
            "reason": "Game Boy Advance ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("nes") => serde_json::json!({
            "system": "nes",
            "confidence": "extension",
            "reason": "NES/Famicom ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("hdi" | "hdm" | "d88") => serde_json::json!({
            "system": "pc98",
            "confidence": "extension",
            "reason": "PC-98 disk extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("pce") => serde_json::json!({
            "system": "pce",
            "confidence": "extension",
            "reason": "PC Engine ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("md" | "gen" | "smd") => serde_json::json!({
            "system": "md",
            "confidence": "extension",
            "reason": "Mega Drive/Genesis ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("gdi" | "cdi") => serde_json::json!({
            "system": "dc",
            "confidence": "extension",
            "reason": "Dreamcast disc extension (.gdi/.cdi are DC-specific)",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("nds") => serde_json::json!({
            "system": "nds",
            "confidence": "extension",
            "reason": "Nintendo DS ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("cue" | "chd" | "bin" | "iso" | "img" | "ccd") => serde_json::json!({
            "system": null,
            "confidence": "ambiguous_media",
            "reason": "disc/binary image extension can map to Saturn, PSX, PCE, MD, or Dreamcast; do not guess without header evidence",
            "needs_user_input": true,
            "required_user_input": "이 image가 saturn, psx, pce, md, dc 중 무엇인지 지정하라",
            "candidates": ["saturn", "psx", "pce", "md", "dc"],
            "markers": markers,
        }),
        other => serde_json::json!({
            "system": null,
            "confidence": "unknown_extension",
            "reason": format!("unsupported or unknown extension: {other:?}"),
            "needs_user_input": true,
            "required_user_input": "content_path의 실제 시스템을 snes/saturn/psx/pce/md/pc98/dc 중 하나로 지정하라",
            "markers": markers,
        }),
    }
}

fn adapter_for_system(system: &str) -> (&'static str, Option<&'static str>) {
    match system {
        "snes" => ("mesen2", None),
        "gamegear" => ("mesen2", None),
        "gb" => ("mesen2", None),
        "gbc" => ("mesen2", None),
        "gba" => ("mesen2", None),
        "nes" => ("mesen2", None),
        "saturn" => ("mednafen", Some("ss")),
        "psx" => ("mednafen", Some("psx")),
        "pce" => ("mednafen", Some("pce")),
        "md" => ("mednafen", Some("md")),
        "pc98" => ("mame_pc98", None),
        "dc" => ("flycast", None),
        "nds" => ("desmume_nds", None),
        _ => ("", None),
    }
}

fn adapter_log_path(adapter: &str, port: u16, filename: &str) -> PathBuf {
    emucap::launch::emu_home_dir(adapter, port).join(filename)
}

pub(crate) fn make_launch_plan(port: Option<u16>, args: &LaunchPlanArgs) -> serde_json::Value {
    let inference = infer_system(args.content_path.as_deref(), args.system.as_deref());
    let paths = runtime_paths(port);
    let Some(system) = inference.get("system").and_then(|v| v.as_str()) else {
        return serde_json::json!({
            "ok": false,
            "ready_to_launch": false,
            "inference": inference,
            "listening_port": port,
            "runtime_paths": paths,
            "supported_systems": supported_systems_value(),
            "next_action": "사용자에게 required_user_input을 물은 뒤 launch_plan(content_path, system)을 다시 호출하라"
        });
    };
    let Some(content_path) = args.content_path.as_deref() else {
        return serde_json::json!({
            "ok": false,
            "ready_to_launch": false,
            "inference": inference,
            "listening_port": port,
            "runtime_paths": paths,
            "supported_systems": supported_systems_value(),
            "next_action": "실행할 content_path를 사용자에게 물은 뒤 launch_plan(content_path, system)을 다시 호출하라"
        });
    };
    let Some(root) = find_repo_root() else {
        return serde_json::json!({
            "ok": false,
            "ready_to_launch": false,
            "inference": inference,
            "listening_port": port,
            "runtime_paths": paths,
            "error": "repo root not found; set EMUCAP_REPO_ROOT"
        });
    };
    let Some(p) = port else {
        return serde_json::json!({
            "ok": false,
            "ready_to_launch": false,
            "inference": inference,
            "runtime_paths": paths,
            "error": "listening_port unavailable; call bootstrap/status again"
        });
    };

    let (adapter, force_module) = adapter_for_system(system);
    let fallback_launcher = adapter_script_launcher(&root, adapter);
    let is_ps1 = fallback_launcher.extension().and_then(|e| e.to_str()) == Some("ps1");
    let mut argv: Vec<String> = if is_ps1 {
        vec![
            "powershell".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
        ]
    } else {
        Vec::new()
    };
    argv.push(fallback_launcher.display().to_string());
    argv.push(content_path.to_string());
    argv.push(p.to_string());
    argv.push(format!("{system}_session"));
    if adapter == "mednafen" {
        if let Some(module) = force_module {
            argv.push(module.to_string());
        }
    } else if adapter == "mame_pc98" {
        argv.push("pc9801rs".to_string());
    }

    let environment_defaults = if adapter == "mame_pc98" {
        serde_json::json!({
            "MAME_CBUS0": {
                "default": "",
                "applies_when": "machine is pc9801rs and MAME_CBUS0 is unset",
                "reason": "local pc9801rs headless set lacks the default pc9801_26 sound-card ROM"
            }
        })
    } else if adapter == "flycast" {
        serde_json::json!({
            "EMUCAP_MUTE": {
                "default": "1",
                "applies_when": "unset",
                "reason": "1=mute (default, for debugging); set EMUCAP_MUTE=0 to keep sound. The launcher applies this to the emucap-owned isolated config copy."
            }
        })
    } else {
        serde_json::json!({})
    };

    // 선행조건(ready_to_launch=true는 *인자 검증*만 — 빌드·BIOS는 별도다. 미충족 시 launcher가
    // binary-not-found나 BIOS-missing으로 실패하므로 미리 알린다).
    let adapter_binary = adapter_binary_precondition(adapter, &root);
    let adapter_built = if adapter == "mednafen" {
        serde_json::json!(adapter_binary["available"].as_bool().unwrap_or(false))
    } else {
        serde_json::Value::Null // mesen2=외부 설치, flycast/mame=빌드경로 다양 → null(build_required로 안내)
    };
    let build_required = build_required_precondition(adapter, &paths, &adapter_binary);
    let content_exists = Path::new(content_path).exists();
    let bridge = if adapter == "mame_pc98" {
        mame_bridge_precondition(&root)
    } else {
        serde_json::Value::Null
    };
    let mut launch_blockers = launch_blockers(content_exists, &adapter_binary);
    push_unavailable_precondition(&mut launch_blockers, "mame_pc98 bridge", &bridge);
    let ready_to_launch = launch_blockers.is_empty();
    let next_action = if ready_to_launch {
        "preconditions(빌드·BIOS)를 먼저 확인하라 — ready_to_launch는 로컬 content와 adapter binary 확인까지 포함한다. 충족되면 preferred_launcher.args로 launch 도구를 호출한 뒤 status로 connected=true와 system을 확인하라(미연결이면 BIOS/romset/빌드 로그를 먼저 의심)"
            .to_string()
    } else {
        format!(
            "launch_blockers를 해결한 뒤 launch_plan(content_path, system)을 다시 호출하라: {}",
            launch_blockers.join("; ")
        )
    };
    let bios_required = match system {
        "saturn" => serde_json::json!("~/.mednafen/firmware/ 에 Saturn BIOS(sega_101.bin JP / mpr-17933.bin US) — 없으면 부팅 실패"),
        "psx" => serde_json::json!("~/.mednafen/firmware/ 에 PSX BIOS(scph5500.bin JP·scph5501.bin US·scph5502.bin EU)"),
        "pce" => serde_json::json!("CD-ROM이면 ~/.mednafen/firmware/syscard3.pce (HuCard ROM은 불요)"),
        "pc98" => serde_json::json!("MAME pc9801rs 머신 romset 필요 — 미제공 시 launch가 조용히 실패할 수 있다(build.sh가 배치)"),
        _ => serde_json::Value::Null, // snes·md·dc는 BIOS 불요(DC는 Flycast HLE 부팅)
    };

    serde_json::json!({
        "ok": true,
        "ready_to_launch": ready_to_launch,
        "launch_blockers": launch_blockers,
        "preconditions": {
            "adapter_built": adapter_built,
            "adapter_binary": adapter_binary,
            "bridge": bridge,
            "build_required": build_required,
            "bios_required": bios_required,
        },
        "system": system,
        "adapter": adapter,
        "force_module": force_module,
        "content_path": content_path,
        "content_exists": content_exists,
        "listening_port": p,
        "preferred_launcher": {
            "kind": "mcp_tool",
            "tool": "launch",
            "args": {
                "content_path": content_path,
                "system": system,
                "name": format!("{system}_session"),
            },
            "reason": "cross-platform Rust launch path; uses emucap-owned config/data roots"
        },
        "legacy_fallback_launcher": fallback_launcher.display().to_string(),
        "legacy_fallback_argv": argv,
        "legacy_fallback": legacy_fallback_details(&fallback_launcher, &argv),
        "environment_defaults": environment_defaults,
        "legacy_fallback_command": legacy_command(&argv),
        "inference": inference,
        "runtime_paths": paths,
        "button_hint": button_hint_for_system(Some(system)),
        "headless_contract": if adapter == "mame_pc98" {
            "PC-98 Rust launch uses repo-local safe headless MAME wrapper by default and disables pc9801rs cbus:0 unless MAME_CBUS0 is explicitly set; do not run work/mame.raw or system mame directly."
        } else if adapter == "mednafen" {
            "Mednafen Rust launch is the supported detached path; do not hand-roll raw nohup."
        } else if adapter == "flycast" {
            "Flycast renders a GUI window and needs the display awake. Rust launch uses an emucap-owned isolated config copy and forces the interpreter when needed; do not run Flycast.app directly."
        } else {
            "Use the Rust launch tool from this plan."
        },
        "next_action": next_action
    })
}

/// Actually launch an emulator (the `launch` tool): ensure the listener, capture this session's port +
/// token, pick the adapter from the system/extension, and dispatch to that adapter's Rust orchestrator.
/// Returns a JSON outcome. A system without a Rust orchestrator yet points back at launch_plan, so no
/// existing flow breaks. The per-adapter spawn logic lives in emucap::launch::<adapter>, not here.
pub(crate) fn make_launch(
    link: &mut (dyn EmulatorLink + Send),
    a: &LaunchArgs,
) -> serde_json::Value {
    let bootstrap = match make_bootstrap_value(link) {
        Ok(b) => b,
        Err(e) => return serde_json::json!({ "launched": false, "error": e.to_string() }),
    };
    let status = bootstrap
        .get("status")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let port_occupied = status
        .get("occupied_by_foreign")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || status
            .get("stale_own_token")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    if port_occupied {
        return serde_json::json!({
            "launched": false,
            "reason": "listening_port is already occupied; not launching another emulator on the same port",
            "status": status,
            "bootstrap": bootstrap,
            "next_action": status.get("recovery").cloned().unwrap_or_else(|| serde_json::json!("call status/bootstrap and resolve the occupied port before launch")),
        });
    }
    // A live emulator is already connected on this session's port. Launching again would spawn a
    // second emulator that, sharing the session token, takes over the connection and leaves the
    // first one orphaned. Refuse and let the agent tear down the current one deliberately.
    let already_connected = status
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if already_connected {
        return serde_json::json!({
            "launched": false,
            "reason": "an emulator is already connected on this session's listening_port; not launching another (it would orphan the current one)",
            "connected_emulator": status.get("emulator_identity").cloned().unwrap_or(serde_json::Value::Null),
            "status": status,
            "next_action": "교체하려면 기존 에뮬을 정리한 뒤 다시 launch하라(save_state 후 connected_emulator를 참조해 그 PID만 종료; 광역 kill 금지). 연결이 이미 죽었으면 status가 connected=false가 된 뒤 재시도하면 새 연결로 자동 채택된다.",
        });
    }
    let Some(port) = bootstrap
        .get("listening_port")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok())
    else {
        return serde_json::json!({ "launched": false, "reason": "listening_port 미확정 — status를 먼저 호출하라" });
    };
    let token = link.session_token().map(str::to_string);

    if !Path::new(&a.content_path).exists() {
        return serde_json::json!({
            "launched": false,
            "reason": "content_path does not exist",
            "content_path": &a.content_path,
            "next_action": "content_path를 확인한 뒤 launch_plan(content_path, system)을 다시 호출하라",
        });
    }

    let inference = infer_system(Some(&a.content_path), a.system.as_deref());
    let Some(system) = inference.get("system").and_then(|v| v.as_str()) else {
        return serde_json::json!({
            "launched": false,
            "reason": "시스템이 애매하다(CUE/CHD/BIN 등) — system을 지정해 다시 호출하라",
            "inference": inference,
        });
    };
    let (adapter, module) = adapter_for_system(system);
    if let Some(root) = find_repo_root() {
        let adapter_binary = adapter_binary_precondition(adapter, &root);
        if !adapter_binary["available"].as_bool().unwrap_or(false) {
            return missing_adapter_binary_response(adapter, system, port, &root, adapter_binary);
        }
        if adapter == "mame_pc98" {
            let bridge = mame_bridge_precondition(&root);
            if !bridge["available"].as_bool().unwrap_or(false) {
                return missing_mame_bridge_response(system, port, &root, adapter_binary, bridge);
            }
        }
    }
    match adapter {
        "mesen2" => launch_mesen(port, token.as_deref(), system, a),
        "mednafen" => launch_mednafen(port, token.as_deref(), module, a),
        "flycast" => launch_flycast(port, token.as_deref(), a),
        "mame_pc98" => launch_mame(port, token.as_deref(), a),
        "desmume_nds" => launch_desmume_nds(port, token.as_deref(), a),
        _ => serde_json::json!({
            "launched": false,
            "reason": format!("{system} 시스템은 Rust 런처 대상이 아니다"),
        }),
    }
}

/// MAME/PC-98 leg of `make_launch`: spawn MAME + the Python GDB bridge; defaults the machine to pc9801rs.
fn launch_mame(port: u16, token: Option<&str>, a: &LaunchArgs) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let Some(binary) = emucap::launch::mame::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "MAME 바이너리 미발견 — adapters/mame-pc98/build.sh로 빌드하거나 MAME_BIN을 설정하라" });
    };
    let log = adapter_log_path("mame-pc98", port, "mame-pc98.log");
    let spec = emucap::launch::mame::Launch {
        binary: &binary,
        repo_root: &root,
        content: &a.content_path,
        flop2: a.content_path2.as_deref(),
        machine: "pc9801rs",
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        headless: true,
    };
    match emucap::launch::mame::launch(&spec) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "mame_pc98",
            "pid": launched.mame_pid,
            "mame_pid": launched.mame_pid,
            "bridge_pid": launched.bridge_pid,
            "bridge": launched.bridge_kind,
            "gdb_port": launched.gdb_port,
            "port": port,
            "binary": binary.display().to_string(),
            "log": log.display().to_string(),
            "note": "MAME + GDB bridge 2-process launch. If MAME spawn fails after bridge spawn, the Rust launcher terminates that bridge.",
            "next_action": "5~8초 뒤 status로 connected=true를 확인하라(미연결이면 romset/bridge 로그를 의심)",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// DeSmuME/NDS leg of `make_launch`: spawn headless desmume-cli (ARM9/ARM7 GDB stubs) + the NDS GDB
/// bridge; a 2-process launch like MAME PC-98. Mirrors adapters/desmume-nds/launch.sh.
fn launch_desmume_nds(port: u16, token: Option<&str>, a: &LaunchArgs) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let Some(binary) = desmume_nds_launch::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "desmume-cli 바이너리 미발견 — adapters/desmume-nds/build.sh로 빌드하거나 EMUCAP_DESMUME_BIN을 설정하라" });
    };
    let Some(bridge) = desmume_nds_launch::resolve_bridge(&root) else {
        return serde_json::json!({ "launched": false, "reason": "NDS bridge 바이너리 미발견 — cargo build --release --bin emucap-desmume-nds-bridge 하거나 EMUCAP_NDS_BRIDGE_BIN을 설정하라" });
    };
    let log = adapter_log_path("desmume-nds", port, "desmume-nds.log");
    let display = a.display.unwrap_or(false);
    let spec = desmume_nds_launch::Launch {
        binary: &binary,
        bridge: &bridge,
        content: &a.content_path,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        display,
    };
    match desmume_nds_launch::launch(&spec) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "desmume_nds",
            "pid": launched.desmume_pid,
            "desmume_pid": launched.desmume_pid,
            "bridge_pid": launched.bridge_pid,
            "arm9_gdb_port": launched.arm9_gdb_port,
            "arm7_gdb_port": launched.arm7_gdb_port,
            "display": display,
            "port": port,
            "binary": binary.display().to_string(),
            "bridge": bridge.display().to_string(),
            "log": log.display().to_string(),
            "note": "DeSmuME + NDS GDB bridge 2-process launch. If the bridge spawn fails after DeSmuME spawn, the Rust launcher terminates DeSmuME.",
            "next_action": "5~8초 뒤 status로 connected=true를 확인하라(미연결이면 desmume-nds.log의 GDB/bridge 연결을 의심)",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// Flycast leg of `make_launch` (Dreamcast): resolve the built app and hand off with the isolated
/// config seeding. Mute defaults on and the GDB stub off (the exec-BP path enables it explicitly).
fn launch_flycast(port: u16, token: Option<&str>, a: &LaunchArgs) -> serde_json::Value {
    let Some(binary) = emucap::launch::flycast::resolve_binary() else {
        return serde_json::json!({ "launched": false, "reason": "Flycast 바이너리 미발견 — adapters/flycast/build.sh로 빌드하거나 FLYCAST_APP을 실행파일 또는 macOS Flycast.app 경로로 설정하라" });
    };
    let log = adapter_log_path("flycast", port, "flycast.log");
    let spec = emucap::launch::flycast::Launch {
        binary: &binary,
        content: &a.content_path,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        mute: true,
        gdb: false,
    };
    match emucap::launch::flycast::launch(&spec) {
        Ok(pid) => serde_json::json!({
            "launched": true,
            "adapter": "flycast",
            "pid": pid,
            "port": port,
            "binary": binary.display().to_string(),
            "log": log.display().to_string(),
            "next_action": "5~8초 뒤 status로 connected=true를 확인하라(미연결이면 dc_boot.bin/디스크를 의심)",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// SNES/Mesen leg of `make_launch`: resolve the binary + adapter Lua and hand off to the orchestrator.
fn launch_mesen(port: u16, token: Option<&str>, system: &str, a: &LaunchArgs) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let Some(binary) = emucap::launch::mesen::resolve_binary() else {
        return serde_json::json!({ "launched": false, "reason": "Mesen 바이너리 미발견 — MESEN_BIN을 Mesen 실행파일 또는 macOS Mesen.app 경로로 설정하라" });
    };
    // 시스템별 얇은 엔트리 스크립트(SYS config 설정 후 emucap-core.lua를 require). Mesen은 SNES/GG/GB(+GBC)/GBA/NES 처리.
    let entry = match system {
        "gamegear" => "adapters/mesen2/emucap-sms.lua",
        "gb" | "gbc" => "adapters/mesen2/emucap-gb.lua",
        "gba" => "adapters/mesen2/emucap-gba.lua",
        "nes" => "adapters/mesen2/emucap-nes.lua",
        _ => "adapters/mesen2/emucap-snes.lua",
    };
    let lua = root.join(entry);
    let log = adapter_log_path("mesen2", port, "mesen.log");
    let spec = emucap::launch::mesen::Launch {
        binary: &binary,
        content: &a.content_path,
        lua: &lua,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
    };
    match emucap::launch::mesen::launch(&spec) {
        Ok(pid) => serde_json::json!({
            "launched": true,
            "adapter": "mesen2",
            "pid": pid,
            "port": port,
            "binary": binary.display().to_string(),
            "log": log.display().to_string(),
            "emucap_home": emucap::launch::emu_home_dir("mesen2", port).display().to_string(),
            "isolation": "Mesen runs from an emucap-owned portable copy; user settings.json is not edited.",
            "next_action": "5~8초 뒤 status로 connected=true와 system을 확인하라(미연결이면 로그를 보라)",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// Mednafen leg of `make_launch` (Saturn/PSX/PCE/MD): resolve the built fork (per-port copy unless
/// MEDNAFEN_BIN is pinned) and hand off with the force_module.
fn launch_mednafen(
    port: u16,
    token: Option<&str>,
    module: Option<&'static str>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let Some((binary, explicit)) = emucap::launch::mednafen::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "Mednafen 바이너리 미발견 — adapters/mednafen/build.sh로 빌드하거나 MEDNAFEN_BIN을 설정하라" });
    };
    let log = adapter_log_path("mednafen", port, "mednafen.log");
    let spec = emucap::launch::mednafen::Launch {
        binary: &binary,
        explicit_binary: explicit,
        content: &a.content_path,
        module,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        headless: false,
    };
    match emucap::launch::mednafen::launch(&spec) {
        Ok(pid) => serde_json::json!({
            "launched": true,
            "adapter": "mednafen",
            "module": module,
            "pid": pid,
            "port": port,
            "binary": binary.display().to_string(),
            "log": log.display().to_string(),
            "next_action": "5~8초 뒤 status로 connected=true를 확인하라(미연결이면 BIOS/force_module을 의심)",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// 진입점이 IdentityMismatch(포트를 다른 세션 에뮬이 점유)일 때 하드에러 대신 줄 graceful 응답.
/// 계약: 미연결처럼 connected=false + listening_port + runtime_paths를 주고, 점유자 진단·복구절차를 더한다.
/// 그래야 새 세션이 잠기지 않고 자기 에뮬을 올바른 포트로 띄우거나 orphan을 정리할 수 있다.
pub(crate) fn occupied_graceful(
    occupant: &EmulatorIdentity,
    port: Option<u16>,
    token: Option<&str>,
) -> serde_json::Value {
    // 점유자가 *이 세션 소유*(echo 토큰의 cwd_hash 일치)인데도 mismatch면, 토큰파일 유실/스윕으로
    // 서버 토큰만 새로 발급된 경우다 — foreign이 아니라 stale-own. 재연결로는 못 고치고(파일이 이미
    // 새 토큰) save_state 후 relaunch가 복구다. foreign과 다르게 안내해야 무한 재연결 루프를 막는다.
    let stale_own = occupant
        .session_token
        .as_deref()
        .map(emucap::live::tcp::session_token_is_own)
        .unwrap_or(false);
    let recovery = if stale_own {
        "이 포트의 에뮬레이터는 *이 세션 소유*인데 토큰이 어긋났다(토큰파일 유실/스윕 추정). 재연결로는 안 고쳐진다 — 필요하면 save_state 후 launch 도구로 같은 포트에 재기동하면 새 토큰파일을 읽어 매칭된다."
    } else {
        "이 포트를 다른 세션의 에뮬레이터가 점유 중이다(occupant 참조). 같은 세션의 stale 연결이면 /mcp 재연결 시 토큰이 재사용돼 자동 reclaim된다. 무관한 orphan이면 occupant.content/system을 확인해 그 PID만 종료(pgrep -f <content> → kill; 광역 kill 금지) 후 재시도하거나, 이 세션 에뮬을 다른 포트로 띄운다."
    };
    let mut v = serde_json::json!({
        "connected": false,
        "occupied_by_foreign": !stale_own,
        "stale_own_token": stale_own,
        "listening_port": port,
        "first_tool_if_unknown": "bootstrap",
        "occupant": {
            "system": occupant.system,
            "adapter": occupant.adapter,
            "name": occupant.name,
            "content": occupant.content,
        },
        "recovery": recovery
    });
    enrich_link_status(&mut v, port, token, None);
    v
}
