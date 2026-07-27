use super::media::{content_markers, ext_lower};
use super::*;

pub(super) use super::system::{adapter_for_system, normalize_system};

pub(super) fn adapter_script_launcher(root: &Path, adapter: &str) -> PathBuf {
    let dir = match adapter {
        "mesen2" => "adapters/mesen2",
        "mednafen" => "adapters/mednafen",
        "mame_pc98" => "adapters/mame-pc98",
        "mame_neogeo" => "adapters/mame-neogeo",
        "mupen64plus" => "adapters/mupen64plus",
        "openmsx" => "adapters/openmsx",
        "flycast" => "adapters/flycast",
        "desmume_nds" => "adapters/desmume-nds",
        "ppsspp" => "adapters/ppsspp",
        "pcsx2" => "adapters/pcsx2",
        "dolphin" => {
            return root.join("adapters/dolphin/launch-native.ps1");
        }
        _ => return root.join("adapters"),
    };
    let ps1 = root.join(dir).join("launch.ps1");
    if cfg!(windows) && ps1.exists() {
        ps1
    } else {
        root.join(dir).join("launch.sh")
    }
}

pub(super) fn same_path(a: &Path, b: &Path) -> bool {
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

pub(super) fn path_matches_candidates(path: &Path, candidates: Vec<PathBuf>) -> bool {
    candidates
        .iter()
        .any(|candidate| same_path(path, candidate))
}

pub(super) fn native_legacy_script(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    path.is_file()
        && if cfg!(windows) {
            ext.is_some_and(|e| e.eq_ignore_ascii_case("ps1"))
        } else {
            ext == Some("sh")
        }
}

pub(super) fn legacy_command(argv: &[String]) -> String {
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

pub(super) fn legacy_fallback_details(launcher: &Path, argv: &[String]) -> serde_json::Value {
    let available = native_legacy_script(launcher);
    serde_json::json!({
        "available_on_this_host": available,
        "launcher": if available {
            serde_json::json!(launcher.display().to_string())
        } else {
            serde_json::Value::Null
        },
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

pub(super) fn mednafen_binary_precondition_from(
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

pub(super) fn mednafen_binary_precondition(root: &Path) -> serde_json::Value {
    mednafen_binary_precondition_from(root, mednafen_launch::resolve_binary(root))
}

pub(super) fn simple_binary_precondition(
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

pub(super) fn env_path_matches(key: &str, path: &Path) -> bool {
    std::env::var_os(key)
        .as_deref()
        .is_some_and(|p| same_path(Path::new(p), path))
}

pub(super) fn env_path_or_app_matches(key: &str, path: &Path, exe_name: &str) -> bool {
    std::env::var_os(key).as_deref().is_some_and(|p| {
        let raw = Path::new(p);
        same_path(raw, path) || same_path(&raw.join("Contents/MacOS").join(exe_name), path)
    })
}

pub(super) fn mesen_binary_precondition_from(
    root: &Path,
    resolved: Option<PathBuf>,
) -> serde_json::Value {
    let Some(path) = resolved else {
        return serde_json::json!({
            "available": false,
            "source": null,
            "kind": "binary-not-found",
        });
    };
    let source = if env_path_or_app_matches("MESEN_BIN", &path, "Mesen") {
        "MESEN_BIN"
    } else if path_matches_candidates(&path, mesen_launch::local_build_candidates(root)) {
        "repo_build"
    } else if path_matches_candidates(&path, mesen_launch::default_install_candidates()) {
        "default_install"
    } else {
        "PATH"
    };
    match mesen_launch::require_compatible_build(root, &path) {
        Ok(build) => serde_json::json!({
            "available": true,
            "path": path.display().to_string(),
            "source": source,
            "host_api": build.host_api,
            "upstream_commit": build.commit,
            "patchset_sha256": build.patchset_sha256,
        }),
        Err(error) => serde_json::json!({
            "available": false,
            "path": path.display().to_string(),
            "source": source,
            "kind": "mesen-patch-required",
            "error": error.to_string(),
        }),
    }
}

pub(super) fn mesen_binary_precondition(root: &Path) -> serde_json::Value {
    mesen_binary_precondition_from(root, mesen_launch::resolve_binary(root))
}

pub(super) fn flycast_binary_precondition_from(resolved: Option<PathBuf>) -> serde_json::Value {
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

pub(super) fn flycast_binary_precondition() -> serde_json::Value {
    flycast_binary_precondition_from(flycast_launch::resolve_binary())
}

pub(super) fn dolphin_binary_precondition_from(
    root: &Path,
    display: bool,
    resolved: Option<PathBuf>,
) -> serde_json::Value {
    let Some(path) = resolved else {
        return serde_json::json!({
            "available": false,
            "source": null,
            "kind": "binary-not-found",
            "display": display,
        });
    };
    let source = if env_path_or_app_matches("EMUCAP_DOLPHIN_BIN", &path, "DolphinQt")
        || env_path_or_app_matches(
            if display {
                "EMUCAP_DOLPHIN_GUI_BIN"
            } else {
                "EMUCAP_DOLPHIN_HEADLESS_BIN"
            },
            &path,
            "DolphinQt",
        ) {
        "environment"
    } else if path_matches_candidates(&path, dolphin_launch::local_build_candidates(root, display))
    {
        "repo_build"
    } else if path_matches_candidates(&path, dolphin_launch::default_install_candidates(display)) {
        "default_install"
    } else {
        "PATH"
    };
    match dolphin_launch::require_compatible_build(root, &path) {
        Ok(build) => serde_json::json!({
            "available": true,
            "path": path.display().to_string(),
            "source": source,
            "display": display,
            "host_api": build.host_api,
            "upstream_commit": build.commit,
            "patchset_sha256": build.patchset_sha256,
        }),
        Err(error) => serde_json::json!({
            "available": false,
            "path": path.display().to_string(),
            "source": source,
            "display": display,
            "kind": "dolphin-patch-required",
            "error": error.to_string(),
        }),
    }
}

pub(super) fn dolphin_binary_precondition(root: &Path, display: bool) -> serde_json::Value {
    dolphin_binary_precondition_from(root, display, dolphin_launch::resolve_binary(root, display))
}

pub(super) fn mame_binary_precondition_from(
    root: &Path,
    resolved: Option<PathBuf>,
) -> serde_json::Value {
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

pub(super) fn mame_binary_precondition(root: &Path) -> serde_json::Value {
    mame_binary_precondition_from(root, mame_launch::resolve_binary(root))
}

pub(super) fn mame_neogeo_binary_precondition(root: &Path) -> serde_json::Value {
    let repo_work = root.join("adapters/mame-neogeo/work");
    simple_binary_precondition(mame_neogeo_launch::resolve_binary(root), |path| {
        if env_path_matches("EMUCAP_NEOGEO_MAME_BIN", path) {
            "EMUCAP_NEOGEO_MAME_BIN"
        } else if env_path_matches("MAME_BIN", path) {
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

/// DeSmuME/NDS needs two binaries — headless desmume-cli and the emucap NDS GDB bridge. Both must
/// resolve for the launcher to run, so the precondition is available only when both are present and
/// reports which one is missing otherwise.
pub(super) fn desmume_nds_binary_precondition(root: &Path) -> serde_json::Value {
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

/// PPSSPP/PSP needs two binaries — headless PPSSPPHeadless and the emucap PSP WebSocket bridge.
/// Both must resolve for the launcher to run, so the precondition is available only when both are
/// present and reports which one is missing otherwise (mirrors `desmume_nds_binary_precondition`).
pub(super) fn ppsspp_binary_precondition(root: &Path) -> serde_json::Value {
    let headless = ppsspp_launch::resolve_binary(root);
    let bridge = ppsspp_launch::resolve_bridge(root);
    match (headless, bridge) {
        (Some(headless), Some(bridge)) => serde_json::json!({
            "available": true,
            "path": headless.display().to_string(),
            "bridge": bridge.display().to_string(),
            "source": if std::env::var_os("EMUCAP_PPSSPP_BIN").is_some() {
                "EMUCAP_PPSSPP_BIN"
            } else {
                "repo_build"
            },
        }),
        (headless, bridge) => serde_json::json!({
            "available": false,
            "source": null,
            "ppsspp_headless_available": headless.is_some(),
            "bridge_available": bridge.is_some(),
        }),
    }
}

pub(super) fn pcsx2_binary_precondition_from(
    root: &Path,
    resolved: Option<PathBuf>,
    bridge: Option<PathBuf>,
    bios: std::io::Result<PathBuf>,
) -> serde_json::Value {
    let Some(path) = resolved else {
        return serde_json::json!({
            "available": false,
            "source": null,
            "kind": "binary-not-found",
            "bridge_available": bridge.is_some(),
            "bios_available": bios.is_ok(),
        });
    };
    let source = if env_path_matches("EMUCAP_PCSX2_BIN", &path) {
        "EMUCAP_PCSX2_BIN"
    } else if path_matches_candidates(&path, pcsx2_launch::local_build_candidates(root)) {
        "repo_build"
    } else {
        "PATH"
    };
    match pcsx2_launch::require_compatible_build(root, &path) {
        Ok(build) if bridge.is_some() && bios.is_ok() => serde_json::json!({
            "available": true,
            "path": path.display().to_string(),
            "source": source,
            "bridge": bridge.map(|path| path.display().to_string()),
            "bridge_available": true,
            "bios": bios.ok().map(|path| path.display().to_string()),
            "bios_available": true,
            "host_api": build.host_api,
            "upstream_commit": build.commit,
            "patchset_sha256": build.patchset_sha256,
        }),
        Ok(build) => serde_json::json!({
            "available": false,
            "path": path.display().to_string(),
            "source": source,
            "kind": if bridge.is_none() { "bridge-not-found" } else { "bios-not-configured" },
            "bridge_available": bridge.is_some(),
            "bios_available": bios.is_ok(),
            "host_api": build.host_api,
            "upstream_commit": build.commit,
            "patchset_sha256": build.patchset_sha256,
        }),
        Err(error) => serde_json::json!({
            "available": false,
            "path": path.display().to_string(),
            "source": source,
            "kind": "pcsx2-patch-required",
            "bridge_available": bridge.is_some(),
            "bios_available": bios.is_ok(),
            "error": error.to_string(),
        }),
    }
}

pub(super) fn pcsx2_binary_precondition(root: &Path) -> serde_json::Value {
    pcsx2_binary_precondition_from(
        root,
        pcsx2_launch::resolve_binary(root),
        pcsx2_launch::resolve_bridge(root),
        pcsx2_launch::resolve_bios(),
    )
}

pub(super) fn mame_bridge_precondition(root: &Path) -> serde_json::Value {
    match mame_launch::resolve_bridge_runtime(root) {
        Ok(runtime) => serde_json::json!({
            "available": true,
            "kind": runtime.kind,
            "program": runtime.program.display().to_string(),
        }),
        Err(e) => serde_json::json!({
            "available": false,
            "error": e.to_string(),
            "source": "EMUCAP_PC98_BRIDGE_BIN / installed emucap-mame-pc98-bridge",
        }),
    }
}

pub(super) fn neogeo_bridge_precondition(root: &Path) -> serde_json::Value {
    match mame_neogeo_launch::resolve_bridge(root) {
        Some(program) => serde_json::json!({
            "available": true,
            "program": program.display().to_string(),
        }),
        None => serde_json::json!({
            "available": false,
            "source": "EMUCAP_NEOGEO_BRIDGE_BIN / installed emucap-mame-neogeo-bridge",
        }),
    }
}

pub(super) fn mupen64plus_precondition(root: &Path, display: bool) -> serde_json::Value {
    let binary = mupen64plus_launch::resolve_binary(root);
    let plugin_root = std::env::var_os("EMUCAP_M64P_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| mupen64plus_launch::default_root(root));
    let build = mupen64plus_launch::require_compatible_root(root, &plugin_root, display);
    serde_json::json!({
        "available": binary.is_some() && build.is_ok(),
        "path": binary.map(|path| path.display().to_string()),
        "plugin_root": plugin_root.display().to_string(),
        "display": display,
        "host_build": build.as_ref().ok(),
        "error": build.err().map(|error| error.to_string()),
        "source": "EMUCAP_M64P_BIN / installed emucap-mupen64plus; EMUCAP_M64P_ROOT / pinned repo build"
    })
}

pub(super) fn openmsx_precondition(root: &Path) -> serde_json::Value {
    let binary = openmsx_launch::resolve_binary(root);
    let bridge = openmsx_launch::resolve_bridge(root);
    let build = binary
        .as_deref()
        .map(|path| openmsx_launch::require_compatible_build(root, path));
    serde_json::json!({
        "available": binary.is_some()
            && bridge.is_some()
            && build.as_ref().is_some_and(Result::is_ok),
        "path": binary.as_ref().map(|path| path.display().to_string()),
        "bridge": bridge.as_ref().map(|path| path.display().to_string()),
        "bridge_available": bridge.is_some(),
        "host_build": build.as_ref().and_then(|result| result.as_ref().ok()),
        "error": build.and_then(Result::err).map(|error| error.to_string()),
        "source": "EMUCAP_OPENMSX_BIN / pinned repo build; EMUCAP_OPENMSX_BRIDGE_BIN / installed emucap-openmsx-bridge",
    })
}

pub(super) fn adapter_binary_precondition_for(
    adapter: &str,
    root: &Path,
    display: bool,
) -> serde_json::Value {
    match adapter {
        "mesen2" => mesen_binary_precondition(root),
        "mednafen" => mednafen_binary_precondition(root),
        "flycast" => flycast_binary_precondition(),
        "dolphin" => dolphin_binary_precondition(root, display),
        "mame_pc98" => mame_binary_precondition(root),
        "mame_neogeo" => mame_neogeo_binary_precondition(root),
        "mupen64plus" => mupen64plus_precondition(root, display),
        "openmsx" => openmsx_precondition(root),
        "desmume_nds" => desmume_nds_binary_precondition(root),
        "ppsspp" => ppsspp_binary_precondition(root),
        "pcsx2" => pcsx2_binary_precondition(root),
        _ => serde_json::Value::Null,
    }
}

pub(super) fn adapter_binary_precondition(adapter: &str, root: &Path) -> serde_json::Value {
    adapter_binary_precondition_for(adapter, root, false)
}

pub(super) fn build_required_precondition(
    adapter: &str,
    paths: &serde_json::Value,
    adapter_binary: &serde_json::Value,
) -> serde_json::Value {
    if adapter_binary["available"].as_bool().unwrap_or(false) {
        return serde_json::Value::Null;
    }
    match adapter {
        "mesen2" => serde_json::json!(format!(
            "Build the pinned compatible Mesen first with {}. A MESEN_BIN override also requires a matching sidecar and runtime codeBreakIdle support.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapters/mesen2/build.sh")
        )),
        "mednafen" => serde_json::json!(format!(
            "Build Mednafen first with {}, or provide a binary through MEDNAFEN_BIN, the default install location, or PATH. Otherwise launch fails with binary-not-found.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        "flycast" => serde_json::json!(format!(
            "Build Flycast first with {}, or provide a binary through FLYCAST_APP, the default install location, or PATH. Otherwise launch fails with binary-not-found.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        "mame_pc98" => serde_json::json!(format!(
            "Build MAME first with {}, or provide a binary through MAME_BIN, the default install location, or PATH. Otherwise launch fails with binary-not-found.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        "mame_neogeo" => serde_json::json!(format!(
            "Build the pinned MAME with {} and build emucap-mame-neogeo-bridge.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapters/mame-neogeo/build.sh")
        )),
        "mupen64plus" => serde_json::json!(format!(
            "Build the pinned debugger-enabled Mupen64Plus bundle with {} and build emucap-mupen64plus.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapters/mupen64plus/build.sh")
        )),
        "openmsx" => serde_json::json!(format!(
            "Build the pinned openMSX host with {} and build emucap-openmsx-bridge with cargo build --release.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapters/openmsx/build.sh")
        )),
        "desmume_nds" => serde_json::json!(format!(
            "Build desmume-cli with {} and build emucap-desmume-nds-bridge with cargo build --release. Otherwise launch fails with binary-not-found.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        "ppsspp" => serde_json::json!(format!(
            "Build PPSSPPHeadless with {} and build emucap-ppsspp-bridge with cargo build --release. Otherwise launch fails with binary-not-found.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapter build.sh")
        )),
        "pcsx2" => serde_json::json!(format!(
            "Build the pinned compatible PCSX2 fork with {}, build emucap-pcsx2-bridge, and set EMUCAP_PCSX2_BIOS.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapters/pcsx2/build.sh")
        )),
        "dolphin" => serde_json::json!(format!(
            "Build the pinned compatible Dolphin native fork first with {}. An override also requires a matching sidecar.",
            paths["adapters"][adapter]["build"]
                .as_str()
                .unwrap_or("adapters/dolphin/build.sh")
        )),
        _ => serde_json::Value::Null,
    }
}

pub(super) fn push_unavailable_precondition(
    blockers: &mut Vec<String>,
    label: &str,
    precondition: &serde_json::Value,
) {
    if !precondition.is_null() && !precondition["available"].as_bool().unwrap_or(false) {
        blockers.push(format!("{label} is unavailable"));
    }
}

pub(super) fn launch_blockers(
    content_exists: bool,
    adapter_binary: &serde_json::Value,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if !content_exists {
        blockers.push("content_path does not exist".to_string());
    }
    if !adapter_binary["available"].as_bool().unwrap_or(false) {
        blockers.push("adapter binary is unavailable".to_string());
    }
    blockers
}

pub(super) fn missing_adapter_binary_response(
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
        "next_action": "Satisfy the adapter binary precondition, then call launch_plan(content_path, system) again.",
    })
}

pub(super) fn missing_mame_bridge_response(
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
        "next_action": "Satisfy the PC-98 bridge precondition, then call launch_plan(content_path, system) again.",
    })
}

pub(super) fn infer_system(
    content_path: Option<&str>,
    requested_system: Option<&str>,
) -> serde_json::Value {
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
            "required_user_input": "Specify one supported system ID. Use neogeo_mvs for Neo Geo MVS, neogeo_aes for Neo Geo AES, or neogeo_cd for Neo Geo CD; generic neogeo is not accepted."
        });
    }

    let Some(path) = content_path else {
        return serde_json::json!({
            "system": null,
            "confidence": "none",
            "reason": "content_path is absent, so the system cannot be inferred from media",
            "needs_user_input": true,
            "required_user_input": "Provide the ROM, disc, or disk path and one system ID from supported_systems."
        });
    };

    let markers = content_markers(Some(path));
    let has_marker = |name: &str| {
        markers
            .get("markers")
            .and_then(|v| v.as_array())
            .is_some_and(|items| items.iter().any(|v| v.as_str() == Some(name)))
    };

    if has_marker("psp_game_marker") {
        return serde_json::json!({
            "system": "psp",
            "confidence": "header",
            "reason": "media prefix contains a PSP GAME marker (ISO9660 System Identifier)",
            "needs_user_input": false,
            "markers": markers,
        });
    }
    if has_marker("ps2_system_cnf") {
        return serde_json::json!({
            "system": "ps2",
            "confidence": "filesystem",
            "reason": "ISO9660 SYSTEM.CNF contains a PS2 BOOT2 entry",
            "needs_user_input": false,
            "markers": markers,
        });
    }
    if has_marker("sega_saturn_header") {
        return serde_json::json!({
            "system": "saturn",
            "confidence": "header",
            "reason": "media prefix contains SEGA SEGASATURN",
            "needs_user_input": false,
            "markers": markers,
        });
    }
    if has_marker("playstation_marker") || has_marker("psx_system_cnf") {
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
    if has_marker("gamecube_disc_magic") {
        return serde_json::json!({
            "system": "gamecube",
            "confidence": "header",
            "reason": "media header contains the GameCube disc magic",
            "needs_user_input": false,
            "markers": markers,
        });
    }
    if has_marker("wii_disc_magic") {
        return serde_json::json!({
            "system": "wii",
            "confidence": "header",
            "reason": "media header contains the Wii disc magic",
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
        Some("z64" | "n64" | "v64") => serde_json::json!({
            "system": "n64",
            "confidence": "extension",
            "reason": "Nintendo 64 cartridge ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("mx1" | "mx2") => serde_json::json!({
            "system": "msx",
            "confidence": "extension",
            "reason": "MSX cartridge ROM extension",
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
        Some("ws" | "wsc" | "wsr") => serde_json::json!({
            "system": "wswan",
            "confidence": "extension",
            "reason": "WonderSwan / WonderSwan Color ROM extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("ngp" | "ngpc" | "ngc" | "npc") => serde_json::json!({
            "system": "ngp",
            "confidence": "extension",
            "reason": "Neo Geo Pocket / Neo Geo Pocket Color ROM extension",
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
        Some("cso" | "pbp") => serde_json::json!({
            "system": "psp",
            "confidence": "extension",
            "reason": "PSP compressed ISO (.cso) / EBOOT (.pbp) extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("gcm") => serde_json::json!({
            "system": "gamecube",
            "confidence": "extension",
            "reason": "GameCube disc image extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("wbfs") => serde_json::json!({
            "system": "wii",
            "confidence": "extension",
            "reason": "Wii Backup File System image extension",
            "needs_user_input": false,
            "markers": markers,
        }),
        Some("cue" | "chd" | "bin" | "iso" | "img" | "ccd" | "rvz" | "wia" | "gcz") => {
            serde_json::json!({
                "system": null,
                "confidence": "ambiguous_media",
                "reason": "disc/binary image extension can map to multiple systems; do not guess without header evidence",
                "needs_user_input": true,
                "required_user_input": "Specify the system for this image explicitly.",
                "candidates": ["saturn", "psx", "ps2", "pce", "pcfx", "md", "psp", "dc", "gamecube", "wii"],
                "markers": markers,
            })
        }
        other => serde_json::json!({
            "system": null,
            "confidence": "unknown_extension",
            "reason": format!("unsupported or unknown extension: {other:?}"),
            "needs_user_input": true,
            "required_user_input": "Specify the actual system for content_path using one system ID from supported_systems.",
            "markers": markers,
        }),
    }
}

pub(super) fn adapter_log_path(adapter: &str, port: u16, filename: &str) -> PathBuf {
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
            "next_action": "Ask the user for required_user_input, then call launch_plan(content_path, system) again."
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
            "next_action": "Ask the user for content_path, then call launch_plan(content_path, system) again."
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
    let mut preferred_launcher_args = serde_json::json!({
        "content_path": content_path,
        "system": system,
        "name": format!("{system}_session"),
    });
    if adapter == "mednafen" {
        preferred_launcher_args["sound"] = serde_json::json!(false);
    }
    let fallback_launcher = adapter_script_launcher(&root, adapter);
    let legacy_available = native_legacy_script(&fallback_launcher);
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
    if adapter == "mesen2" {
        argv.push(system.to_string());
    } else if adapter == "mednafen" {
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
    } else if adapter == "mesen2" {
        serde_json::json!({
            "EMUCAP_MESEN_LUA": {
                "default": null,
                "applies_when": "explicitly set",
                "reason": "optional per-system Lua entry override; otherwise the fallback launcher uses its SYSTEM argument or an unambiguous ROM extension"
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
    let bridge = match adapter {
        "mame_pc98" => mame_bridge_precondition(&root),
        "mame_neogeo" => neogeo_bridge_precondition(&root),
        _ => serde_json::Value::Null,
    };
    let mut launch_blockers = launch_blockers(content_exists, &adapter_binary);
    let bridge_label = if adapter == "mame_neogeo" {
        "mame_neogeo bridge"
    } else {
        "mame_pc98 bridge"
    };
    push_unavailable_precondition(&mut launch_blockers, bridge_label, &bridge);
    let ready_to_launch = launch_blockers.is_empty();
    let next_action = if ready_to_launch {
        "Check build and BIOS preconditions first. ready_to_launch includes local content and adapter-binary checks. When ready, call launch with preferred_launcher.args, then verify connected=true and system with status. If connection fails, inspect BIOS, ROM-set, and build logs first."
            .to_string()
    } else {
        format!(
            "Resolve launch_blockers, then call launch_plan(content_path, system) again: {}",
            launch_blockers.join("; ")
        )
    };
    let bios_required = match system {
        "saturn" => serde_json::json!("Saturn BIOS under ~/.mednafen/firmware/: sega_101.bin for JP or mpr-17933.bin for US. Boot fails when the required BIOS is absent."),
        "psx" => serde_json::json!("PSX BIOS under ~/.mednafen/firmware/: scph5500.bin for JP, scph5501.bin for US, or scph5502.bin for EU."),
        "pce" => serde_json::json!("CD-ROM content requires ~/.mednafen/firmware/syscard3.pce. HuCard ROMs do not."),
        "pcfx" => serde_json::json!("Requires the PC-FX BIOS version 1.00. Set EMUCAP_PCFX_BIOS to an absolute pcfx.rom path or place it under the emucap-owned firmware directory."),
        "pc98" => serde_json::json!("Requires the MAME pc9801rs machine ROM set. The build script installs it; launch may otherwise fail before connection."),
        "neogeo_mvs" => serde_json::json!("Requires MAME neogeo.zip BIOS and a game-specific MVS .zip ROM set. Set EMUCAP_NEOGEO_BIOS or place neogeo.zip beside the game set."),
        "neogeo_aes" => serde_json::json!("Requires MAME aes.zip and an AES-compatible cartridge .zip whose stem matches the pinned Neo Geo software list. Set EMUCAP_NEOGEO_AES_BIOS or place aes.zip beside the cartridge set."),
        "neogeo_cd" => serde_json::json!("Requires MAME neocdz.zip and a CUE whose referenced files all exist. Set EMUCAP_NEOGEO_CD_BIOS or place neocdz.zip beside the CUE."),
        "msx1" | "msx2" | "msx2p" => serde_json::json!("Requires the exact system ROMs for the pinned real-machine profile. Set EMUCAP_OPENMSX_FIRMWARE to an absolute directory, or place them under <emucap-data>/firmware/openmsx. The launcher verifies the pinned SHA-1 manifest before spawn."),
        "n64" => serde_json::Value::Null,
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
            "args": preferred_launcher_args,
            "reason": "cross-platform Rust launch path; uses emucap-owned config/data roots"
        },
        "legacy_fallback_launcher": if legacy_available {
            serde_json::json!(fallback_launcher.display().to_string())
        } else {
            serde_json::Value::Null
        },
        "legacy_fallback_argv": if legacy_available {
            serde_json::json!(argv)
        } else {
            serde_json::Value::Null
        },
        "legacy_fallback": legacy_fallback_details(&fallback_launcher, &argv),
        "environment_defaults": environment_defaults,
        "legacy_fallback_command": if legacy_available {
            serde_json::json!(legacy_command(&argv))
        } else {
            serde_json::Value::Null
        },
        "inference": inference,
        "runtime_paths": paths,
        "button_hint": button_hint_for_system(Some(system)),
        "headless_contract": if adapter == "mame_pc98" {
            "PC-98 Rust launch is headless by default; launch(display:true) explicitly authorizes the repo-local safe MAME wrapper to open a window. It disables pc9801rs cbus:0 unless MAME_CBUS0 is explicitly set; do not run work/mame.raw or system mame directly."
        } else if adapter == "mame_neogeo" {
            "Neo Geo MVS, AES, and CD launch headless by default and use profile-specific emucap-owned MAME homes. launch(display:true) opens a window without reading or changing the user's MAME configuration."
        } else if adapter == "mupen64plus" {
            "Nintendo 64 launch is headless by default and uses an emucap-owned Mupen64Plus configuration. launch(display:true) explicitly loads the pinned Rice video plugin."
        } else if adapter == "mednafen" {
            "Mednafen Rust launch is the supported detached path; do not hand-roll raw nohup."
        } else if adapter == "flycast" {
            "Flycast renders a GUI window and needs the display awake. Rust launch uses an emucap-owned isolated config copy and forces the interpreter when needed; do not run Flycast.app directly."
        } else if adapter == "ppsspp" {
            "Headless PPSSPP boots the content positionally (not -m/--mount, which only mounts a second image) and is never passed --timeout (that flag aborts the run on a wall-clock deadline regardless of debugger activity); the Rust launch path manages the process lifecycle instead."
        } else if adapter == "dolphin" {
            "Dolphin is headless by default. launch(display:true) selects the compatible DolphinQt build and opens its render window. Both modes use a per-port portable runtime and --user directory."
        } else {
            "Use the Rust launch tool from this plan."
        },
        "sound_contract": if adapter == "mednafen" {
            serde_json::json!({
                "supported": true,
                "default": false,
                "enable_with": "launch(..., sound:true)",
                "independent_of_display": true
            })
        } else {
            serde_json::Value::Null
        },
        "next_action": next_action
    })
}
