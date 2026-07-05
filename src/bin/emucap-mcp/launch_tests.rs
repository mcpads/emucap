use super::*;
use emucap::live::link::{Capabilities, LinkError};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn path_ends_with_parts(path: &str, parts: &[&str]) -> bool {
    let mut suffix = PathBuf::new();
    for part in parts {
        suffix.push(part);
    }
    Path::new(path).ends_with(suffix)
}

#[test]
fn adapter_logs_live_under_per_port_emucap_home() {
    let cases = [
        ("mesen2", "mesen.log"),
        ("flycast", "flycast.log"),
        ("mame-pc98", "mame-pc98.log"),
        ("mednafen", "mednafen.log"),
        ("ppsspp", "ppsspp.log"),
    ];
    for (adapter, file) in cases {
        let path = adapter_log_path(adapter, 47911, file);
        assert!(
            path.ends_with(Path::new(adapter).join("47911").join(file)),
            "{}",
            path.display()
        );
    }
}

#[test]
fn mednafen_precondition_accepts_platform_native_repo_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("adapters/mednafen/work/mednafen/src");
    std::fs::create_dir_all(&src).unwrap();
    let binary_name = if cfg!(windows) {
        "mednafen.exe"
    } else {
        "mednafen"
    };
    let binary = src.join(binary_name);
    std::fs::write(&binary, b"fake mednafen").unwrap();

    let precondition = mednafen_binary_precondition_from(tmp.path(), Some((binary, false)));

    assert_eq!(precondition["available"], true);
    assert_eq!(precondition["source"], "repo_build");
    assert!(precondition["path"]
        .as_str()
        .is_some_and(|p| Path::new(p).ends_with(Path::new("src").join(binary_name))));
}

#[test]
fn mednafen_build_required_respects_resolved_binary() {
    let paths = serde_json::json!({
        "adapters": {
            "mednafen": {
                "build": "/repo/adapters/mednafen/build.sh"
            },
            "flycast": {
                "build": "/repo/adapters/flycast/build.sh"
            },
            "mame_pc98": {
                "build": "/repo/adapters/mame-pc98/build.sh"
            }
        }
    });

    let available =
        build_required_precondition("mednafen", &paths, &serde_json::json!({"available": true}));
    let missing =
        build_required_precondition("mednafen", &paths, &serde_json::json!({"available": false}));

    assert!(available.is_null());
    assert!(missing
        .as_str()
        .is_some_and(|s| s.contains("MEDNAFEN_BIN/default install/PATH")));
}

#[test]
fn build_required_respects_existing_flycast_and_mame_binaries() {
    let paths = serde_json::json!({
        "adapters": {
            "mesen2": {
                "launch": "/repo/adapters/mesen2/launch.sh"
            },
            "flycast": {
                "build": "/repo/adapters/flycast/build.sh"
            },
            "mame_pc98": {
                "build": "/repo/adapters/mame-pc98/build.sh"
            }
        }
    });

    for adapter in ["mesen2", "flycast", "mame_pc98"] {
        let available =
            build_required_precondition(adapter, &paths, &serde_json::json!({"available": true}));
        assert!(available.is_null(), "{adapter}");
    }

    let mesen_missing =
        build_required_precondition("mesen2", &paths, &serde_json::json!({"available": false}));
    assert!(mesen_missing
        .as_str()
        .is_some_and(|s| s.contains("MESEN_BIN/default install/PATH")));

    let flycast_missing =
        build_required_precondition("flycast", &paths, &serde_json::json!({"available": false}));
    assert!(flycast_missing
        .as_str()
        .is_some_and(|s| s.contains("FLYCAST_APP/default install/PATH")));

    let mame_missing = build_required_precondition(
        "mame_pc98",
        &paths,
        &serde_json::json!({"available": false}),
    );
    assert!(mame_missing
        .as_str()
        .is_some_and(|s| s.contains("MAME_BIN/default install/PATH")));
}

#[test]
fn default_install_preconditions_report_default_source() {
    if let Some(path) = mesen_launch::default_install_candidates()
        .into_iter()
        .next()
    {
        let precondition = mesen_binary_precondition_from(Some(path));
        assert_eq!(precondition["source"], "default_install");
    }

    if let Some(path) = flycast_launch::default_install_candidates()
        .into_iter()
        .next()
    {
        let precondition = flycast_binary_precondition_from(Some(path));
        assert_eq!(precondition["source"], "default_install");
    }

    if let Some(path) = mame_launch::default_install_candidates().into_iter().next() {
        let precondition = mame_binary_precondition_from(Path::new("/repo"), Some(path));
        assert_eq!(precondition["source"], "default_install");
    }
}

#[test]
fn app_bundle_env_preconditions_report_env_source() {
    let _guard = env_lock();
    let dir = tempfile::tempdir().unwrap();
    let mesen_app = dir.path().join("Mesen.app");
    let mesen_bin = mesen_app.join("Contents/MacOS/Mesen");
    let flycast_app = dir.path().join("Flycast.app");
    let flycast_bin = flycast_app.join("Contents/MacOS/Flycast");
    std::fs::create_dir_all(mesen_bin.parent().unwrap()).unwrap();
    std::fs::create_dir_all(flycast_bin.parent().unwrap()).unwrap();

    let old_mesen = std::env::var_os("MESEN_BIN");
    let old_flycast = std::env::var_os("FLYCAST_APP");
    std::env::set_var("MESEN_BIN", &mesen_app);
    std::env::set_var("FLYCAST_APP", &flycast_app);

    let mesen = mesen_binary_precondition_from(Some(mesen_bin));
    let flycast = flycast_binary_precondition_from(Some(flycast_bin));

    match old_mesen {
        Some(v) => std::env::set_var("MESEN_BIN", v),
        None => std::env::remove_var("MESEN_BIN"),
    }
    match old_flycast {
        Some(v) => std::env::set_var("FLYCAST_APP", v),
        None => std::env::remove_var("FLYCAST_APP"),
    }

    assert_eq!(mesen["source"], "MESEN_BIN");
    assert_eq!(flycast["source"], "FLYCAST_APP");
}

#[test]
fn legacy_fallback_availability_follows_host_script_type() {
    let sh = Path::new("launch.sh");
    let ps1 = Path::new("launch.ps1");

    assert_eq!(native_legacy_script(sh), !cfg!(windows));
    assert_eq!(native_legacy_script(ps1), cfg!(windows));

    let non_native = if cfg!(windows) { sh } else { ps1 };
    let details = legacy_fallback_details(non_native, &["launch".into()]);
    assert_eq!(details["available_on_this_host"], false);
    assert_eq!(details["argv"], serde_json::Value::Null);
}

#[test]
fn mame_bridge_precondition_reports_selected_bridge_errors() {
    let _guard = env_lock();
    let old = std::env::var_os("EMUCAP_PC98_BRIDGE");
    std::env::set_var("EMUCAP_PC98_BRIDGE", "bogus");

    let precondition = mame_bridge_precondition(Path::new("/repo"));

    match old {
        Some(v) => std::env::set_var("EMUCAP_PC98_BRIDGE", v),
        None => std::env::remove_var("EMUCAP_PC98_BRIDGE"),
    }

    assert_eq!(precondition["available"], false);
    assert!(precondition["error"]
        .as_str()
        .is_some_and(|s| s.contains("EMUCAP_PC98_BRIDGE")));
}

#[test]
fn unavailable_bridge_becomes_launch_blocker() {
    let mut blockers = launch_blockers(true, &serde_json::json!({"available": true}));
    push_unavailable_precondition(
        &mut blockers,
        "mame_pc98 bridge",
        &serde_json::json!({"available": false}),
    );

    assert_eq!(blockers, vec!["mame_pc98 bridge is unavailable"]);
}

#[test]
fn infer_system_does_not_guess_ambiguous_disc_media() {
    let tmp = tempfile::tempdir().unwrap();
    let cue = tmp.path().join("game.cue");
    std::fs::write(&cue, "FILE \"track01.bin\" BINARY\n  TRACK 01 MODE1/2352\n").unwrap();
    let inferred = infer_system(cue.to_str(), None);
    assert_eq!(inferred["system"], serde_json::Value::Null);
    assert_eq!(inferred["confidence"], "ambiguous_media");
    assert_eq!(inferred["needs_user_input"], true);
    assert!(inferred["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("pce")));
    // PSP also boots from .iso — registering it must not silently drop it from the ambiguous set
    // (guessing it without header evidence would be just as wrong as guessing saturn/psx/pce/md).
    assert!(inferred["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("psp")));
}

#[test]
fn infer_system_uses_psp_game_header_in_iso() {
    // Real PSP UMD ISOs are ISO9660 with a "PSP GAME" System Identifier at the Primary Volume
    // Descriptor (LBA 16 = byte offset 0x8000, field offset 8) — verified against a real retail
    // ISO in `.superpowers/sdd/task-11-report.md`. A plain .iso extension alone is ambiguous
    // (shared with Saturn/PSX/PCE/MD/Dreamcast), so this header disambiguates it like the existing
    // Saturn/PSX/PCE/MD marker checks.
    let tmp = tempfile::tempdir().unwrap();
    let iso = tmp.path().join("game.iso");
    let mut data = vec![0u8; 0x8100];
    data[0x8008..0x8008 + 8].copy_from_slice(b"PSP GAME");
    std::fs::write(&iso, data).unwrap();

    let inferred = infer_system(iso.to_str(), None);
    assert_eq!(inferred["system"], "psp");
    assert_eq!(inferred["confidence"], "header");
    assert_eq!(inferred["needs_user_input"], false);
}

#[test]
fn infer_system_maps_psp_cso_and_pbp_extensions() {
    for ext in ["cso", "pbp"] {
        let inferred = infer_system(Some(&format!("/tmp/game.{ext}")), None);
        assert_eq!(inferred["system"], "psp", "extension .{ext}");
        assert_eq!(inferred["confidence"], "extension", "extension .{ext}");
        assert_eq!(inferred["needs_user_input"], false, "extension .{ext}");
    }
}

#[test]
fn normalize_system_accepts_psp_aliases() {
    for alias in ["psp", "ppsspp", "playstation-portable"] {
        let inferred = infer_system(None, Some(alias));
        assert_eq!(inferred["system"], "psp", "alias {alias}");
        assert_eq!(inferred["confidence"], "explicit", "alias {alias}");
    }
}

#[test]
fn infer_system_uses_cue_referenced_saturn_header() {
    let tmp = tempfile::tempdir().unwrap();
    let cue = tmp.path().join("game.cue");
    let bin = tmp.path().join("track01.bin");
    let mut data = vec![0; 0x40];
    data[0x10..0x1f].copy_from_slice(b"SEGA SEGASATURN");
    std::fs::write(&bin, data).unwrap();
    std::fs::write(&cue, "FILE \"track01.bin\" BINARY\n  TRACK 01 MODE1/2352\n").unwrap();

    let inferred = infer_system(cue.to_str(), None);
    assert_eq!(inferred["system"], "saturn");
    assert_eq!(inferred["confidence"], "header");
    assert_eq!(inferred["needs_user_input"], false);
}

#[test]
fn infer_system_uses_megadrive_header_in_bin() {
    let tmp = tempfile::tempdir().unwrap();
    let rom = tmp.path().join("game.bin");
    let mut data = vec![0; 0x140];
    data[0x100..0x10f].copy_from_slice(b"SEGA MEGA DRIVE");
    std::fs::write(&rom, data).unwrap();

    let inferred = infer_system(rom.to_str(), None);
    assert_eq!(inferred["system"], "md");
    assert_eq!(inferred["confidence"], "header");
    assert_eq!(inferred["needs_user_input"], false);
}

#[test]
fn infer_system_maps_gameboy_family_extensions() {
    for (ext, expected) in [("gb", "gb"), ("gbc", "gbc"), ("gba", "gba")] {
        let inferred = infer_system(Some(&format!("/tmp/game.{ext}")), None);
        assert_eq!(inferred["system"], expected, "extension .{ext}");
        assert_eq!(inferred["confidence"], "extension", "extension .{ext}");
        assert_eq!(inferred["needs_user_input"], false, "extension .{ext}");
    }
}

#[test]
fn infer_system_maps_nes_extension() {
    let inferred = infer_system(Some("/tmp/game.nes"), None);
    assert_eq!(inferred["system"], "nes");
    assert_eq!(inferred["confidence"], "extension");
    assert_eq!(inferred["needs_user_input"], false);
}

#[test]
fn normalize_system_accepts_nes_aliases() {
    for alias in ["nes", "nintendo", "famicom", "fc"] {
        let inferred = infer_system(None, Some(alias));
        assert_eq!(inferred["system"], "nes", "alias {alias}");
        assert_eq!(inferred["confidence"], "explicit", "alias {alias}");
    }
}

#[test]
fn launch_plan_for_nes_uses_mesen2_and_nes_entry() {
    // NES routes to emucap-nes.lua (6502/2A03) on the mesen2 adapter with no force_module.
    // Extension inference needs no binary/header evidence.
    let plan = make_launch_plan(
        Some(47804),
        &LaunchPlanArgs {
            content_path: Some("/tmp/game.nes".into()),
            system: None,
        },
    );
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["system"], "nes");
    assert_eq!(plan["adapter"], "mesen2");
    assert_eq!(plan["force_module"], serde_json::Value::Null);
    assert_eq!(plan["preferred_launcher"]["tool"], "launch");
    assert_eq!(plan["preferred_launcher"]["args"]["system"], "nes");
    assert_eq!(plan["button_hint"]["system"], "nes");
}

#[test]
fn launch_plan_for_gameboy_family_uses_mesen2_and_gb_entry() {
    // GB/GBC route to the shared emucap-gb.lua (SM83); GBA to emucap-gba.lua (ARM7). All three ride
    // the mesen2 adapter with no force_module. Extension inference needs no binary/header evidence.
    for (ext, expected) in [("gb", "gb"), ("gbc", "gbc"), ("gba", "gba")] {
        let plan = make_launch_plan(
            Some(47804),
            &LaunchPlanArgs {
                content_path: Some(format!("/tmp/game.{ext}")),
                system: None,
            },
        );
        assert_eq!(plan["ok"], true, ".{ext}");
        assert_eq!(plan["system"], expected, ".{ext}");
        assert_eq!(plan["adapter"], "mesen2", ".{ext}");
        assert_eq!(plan["force_module"], serde_json::Value::Null, ".{ext}");
        assert_eq!(plan["preferred_launcher"]["args"]["system"], expected, ".{ext}");
        assert_eq!(plan["button_hint"]["system"], expected_button_system(expected), ".{ext}");
    }
}

fn expected_button_system(system: &str) -> &'static str {
    match system {
        "gba" => "gba",
        _ => "gb", // gb and gbc share the gb button hint
    }
}

#[test]
fn launch_plan_for_md_uses_mednafen_force_module() {
    let plan = make_launch_plan(
        Some(47804),
        &LaunchPlanArgs {
            content_path: Some("/tmp/game.md".into()),
            system: None,
        },
    );
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["system"], "md");
    assert_eq!(plan["adapter"], "mednafen");
    assert_eq!(plan["force_module"], "md");
    assert_eq!(plan["preferred_launcher"]["tool"], "launch");
    assert_eq!(plan["preferred_launcher"]["args"]["system"], "md");
    assert!(plan["legacy_fallback_launcher"]
        .as_str()
        .is_some_and(|p| path_ends_with_parts(p, &["adapters", "mednafen", "launch.sh"])));
    assert!(plan["legacy_fallback_command"]
        .as_str()
        .unwrap()
        .contains("md_session"));
    assert!(plan["legacy_fallback_command"]
        .as_str()
        .unwrap()
        .ends_with(" md"));
    assert_eq!(
        plan["legacy_fallback"]["available_on_this_host"],
        serde_json::json!(!cfg!(windows))
    );
    assert_eq!(plan["button_hint"]["system"], "md");
}

#[test]
fn launch_plan_blocks_missing_content_even_with_binary() {
    let _guard = env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let fake_mesen = tmp
        .path()
        .join(if cfg!(windows) { "Mesen.exe" } else { "Mesen" });
    std::fs::write(&fake_mesen, b"fake").unwrap();
    make_executable(&fake_mesen);
    let old = std::env::var_os("MESEN_BIN");
    std::env::set_var("MESEN_BIN", &fake_mesen);

    let missing = tmp.path().join("missing.sfc");
    let plan = make_launch_plan(
        Some(47804),
        &LaunchPlanArgs {
            content_path: Some(missing.display().to_string()),
            system: None,
        },
    );

    match old {
        Some(v) => std::env::set_var("MESEN_BIN", v),
        None => std::env::remove_var("MESEN_BIN"),
    }

    assert_eq!(plan["ok"], true);
    assert_eq!(plan["content_exists"], false);
    assert_eq!(plan["ready_to_launch"], false);
    assert!(plan["launch_blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("content_path does not exist")));
}

#[test]
fn launch_plan_ready_when_content_and_binary_exist() {
    let _guard = env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let fake_mesen = tmp
        .path()
        .join(if cfg!(windows) { "Mesen.exe" } else { "Mesen" });
    let rom = tmp.path().join("game.sfc");
    std::fs::write(&fake_mesen, b"fake").unwrap();
    make_executable(&fake_mesen);
    std::fs::write(&rom, b"fake snes rom").unwrap();
    let old = std::env::var_os("MESEN_BIN");
    std::env::set_var("MESEN_BIN", &fake_mesen);

    let plan = make_launch_plan(
        Some(47804),
        &LaunchPlanArgs {
            content_path: Some(rom.display().to_string()),
            system: None,
        },
    );

    match old {
        Some(v) => std::env::set_var("MESEN_BIN", v),
        None => std::env::remove_var("MESEN_BIN"),
    }

    assert_eq!(plan["ok"], true);
    assert_eq!(plan["content_exists"], true);
    assert_eq!(plan["preconditions"]["adapter_binary"]["available"], true);
    assert_eq!(plan["ready_to_launch"], true);
    assert!(plan["launch_blockers"].as_array().unwrap().is_empty());
}

#[test]
fn launch_plan_for_explicit_md_accepts_ambiguous_bin() {
    let plan = make_launch_plan(
        Some(47804),
        &LaunchPlanArgs {
            content_path: Some("/tmp/game.bin".into()),
            system: Some("genesis".into()),
        },
    );
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["system"], "md");
    assert_eq!(plan["force_module"], "md");
    assert_eq!(plan["inference"]["confidence"], "explicit");
}

#[test]
fn launch_plan_for_pc98_uses_repo_launcher_and_headless_contract() {
    let plan = make_launch_plan(
        Some(47803),
        &LaunchPlanArgs {
            content_path: Some("/tmp/game.hdi".into()),
            system: None,
        },
    );
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["system"], "pc98");
    assert_eq!(plan["adapter"], "mame_pc98");
    assert_eq!(plan["preferred_launcher"]["tool"], "launch");
    assert_eq!(plan["preferred_launcher"]["args"]["system"], "pc98");
    assert!(plan["legacy_fallback_launcher"]
        .as_str()
        .is_some_and(|p| path_ends_with_parts(p, &["adapters", "mame-pc98", "launch.sh"])));
    assert!(plan["legacy_fallback_command"]
        .as_str()
        .unwrap()
        .contains("pc9801rs"));
    assert_eq!(
        plan["legacy_fallback"]["available_on_this_host"],
        serde_json::json!(!cfg!(windows))
    );
    assert_eq!(plan["environment_defaults"]["MAME_CBUS0"]["default"], "");
    assert!(plan["headless_contract"]
        .as_str()
        .unwrap()
        .contains("headless MAME wrapper"));
    assert!(plan["headless_contract"]
        .as_str()
        .unwrap()
        .contains("cbus:0"));
}

#[test]
fn launch_plan_for_nds_uses_desmume_adapter_and_mcp_launcher() {
    // .nds routes to the desmume_nds adapter (headless desmume-cli + NDS GDB bridge) with no
    // force_module; extension inference needs no header evidence. Preferred launcher is the MCP
    // launch tool; the legacy fallback points at adapters/desmume-nds/launch.sh.
    let plan = make_launch_plan(
        Some(47804),
        &LaunchPlanArgs {
            content_path: Some("/tmp/game.nds".into()),
            system: None,
        },
    );
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["system"], "nds");
    assert_eq!(plan["adapter"], "desmume_nds");
    assert_eq!(plan["force_module"], serde_json::Value::Null);
    assert_eq!(plan["preferred_launcher"]["tool"], "launch");
    assert_eq!(plan["preferred_launcher"]["args"]["system"], "nds");
    assert!(plan["legacy_fallback_launcher"]
        .as_str()
        .is_some_and(|p| path_ends_with_parts(p, &["adapters", "desmume-nds", "launch.sh"])));
    assert!(plan["legacy_fallback_command"]
        .as_str()
        .unwrap()
        .contains("nds_session"));
    assert_eq!(
        plan["legacy_fallback"]["available_on_this_host"],
        serde_json::json!(!cfg!(windows))
    );
    assert_eq!(plan["button_hint"]["system"], "nds");
}

#[test]
fn desmume_nds_precondition_reports_missing_binaries() {
    let _guard = env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old_desmume = std::env::var_os("EMUCAP_DESMUME_BIN");
    let old_bridge = std::env::var_os("EMUCAP_NDS_BRIDGE_BIN");
    // Point both overrides at nonexistent files so neither binary resolves regardless of the host.
    std::env::set_var("EMUCAP_DESMUME_BIN", tmp.path().join("missing-desmume"));
    std::env::set_var("EMUCAP_NDS_BRIDGE_BIN", tmp.path().join("missing-bridge"));

    let precondition = desmume_nds_binary_precondition(tmp.path());

    match old_desmume {
        Some(v) => std::env::set_var("EMUCAP_DESMUME_BIN", v),
        None => std::env::remove_var("EMUCAP_DESMUME_BIN"),
    }
    match old_bridge {
        Some(v) => std::env::set_var("EMUCAP_NDS_BRIDGE_BIN", v),
        None => std::env::remove_var("EMUCAP_NDS_BRIDGE_BIN"),
    }

    assert_eq!(precondition["available"], serde_json::json!(false));
    assert_eq!(
        precondition["desmume_cli_available"],
        serde_json::json!(false)
    );
    assert_eq!(precondition["bridge_available"], serde_json::json!(false));

    let paths = serde_json::json!({
        "adapters": { "desmume_nds": { "build": "/repo/adapters/desmume-nds/build.sh" } }
    });
    let build_required = build_required_precondition("desmume_nds", &paths, &precondition);
    assert!(build_required
        .as_str()
        .is_some_and(|s| s.contains("emucap-desmume-nds-bridge")));
}

#[test]
fn launch_plan_for_psp_uses_ppsspp_adapter_and_mcp_launcher() {
    // .cso is unambiguously PSP (extension inference, no header evidence needed — mirrors the .nds
    // case), routing to the ppsspp adapter with no force_module. Preferred launcher is the MCP
    // launch tool; the legacy fallback points at adapters/ppsspp/launch.sh.
    let plan = make_launch_plan(
        Some(47805),
        &LaunchPlanArgs {
            content_path: Some("/tmp/game.cso".into()),
            system: None,
        },
    );
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["system"], "psp");
    assert_eq!(plan["adapter"], "ppsspp");
    assert_eq!(plan["force_module"], serde_json::Value::Null);
    assert_eq!(plan["preferred_launcher"]["tool"], "launch");
    assert_eq!(plan["preferred_launcher"]["args"]["system"], "psp");
    assert!(plan["legacy_fallback_launcher"]
        .as_str()
        .is_some_and(|p| path_ends_with_parts(p, &["adapters", "ppsspp", "launch.sh"])));
    assert!(plan["legacy_fallback_command"]
        .as_str()
        .unwrap()
        .contains("psp_session"));
    assert_eq!(
        plan["legacy_fallback"]["available_on_this_host"],
        serde_json::json!(!cfg!(windows))
    );
}

#[test]
fn ppsspp_precondition_reports_missing_binaries() {
    let _guard = env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old_headless = std::env::var_os("EMUCAP_PPSSPP_BIN");
    let old_bridge = std::env::var_os("EMUCAP_PSP_BRIDGE_BIN");
    // Point both overrides at nonexistent files so neither binary resolves regardless of the host.
    std::env::set_var("EMUCAP_PPSSPP_BIN", tmp.path().join("missing-ppsspp"));
    std::env::set_var("EMUCAP_PSP_BRIDGE_BIN", tmp.path().join("missing-bridge"));

    let precondition = ppsspp_binary_precondition(tmp.path());

    match old_headless {
        Some(v) => std::env::set_var("EMUCAP_PPSSPP_BIN", v),
        None => std::env::remove_var("EMUCAP_PPSSPP_BIN"),
    }
    match old_bridge {
        Some(v) => std::env::set_var("EMUCAP_PSP_BRIDGE_BIN", v),
        None => std::env::remove_var("EMUCAP_PSP_BRIDGE_BIN"),
    }

    assert_eq!(precondition["available"], serde_json::json!(false));
    assert_eq!(
        precondition["ppsspp_headless_available"],
        serde_json::json!(false)
    );
    assert_eq!(precondition["bridge_available"], serde_json::json!(false));

    let paths = serde_json::json!({
        "adapters": { "ppsspp": { "build": "/repo/adapters/ppsspp/build.sh" } }
    });
    let build_required = build_required_precondition("ppsspp", &paths, &precondition);
    assert!(build_required
        .as_str()
        .is_some_and(|s| s.contains("emucap-ppsspp-bridge")));
}

#[test]
fn occupied_graceful_returns_diagnostic_not_error() {
    let occupant = EmulatorIdentity {
        system: Some("md".into()),
        adapter: Some("mednafen".into()),
        name: Some("md_session".into()),
        session_token: Some("37a5cd55-x-y".into()),
        content: Some("/x/game_poc.md".into()),
        ..Default::default()
    };
    let v = occupied_graceful(&occupant, Some(47801), None);
    // 진입점 계약: 에러가 아니라 connected=false + port + runtime_paths + 점유 진단
    assert_eq!(v["connected"], serde_json::json!(false));
    assert_eq!(v["occupied_by_foreign"], serde_json::json!(true));
    assert_eq!(v["stale_own_token"], serde_json::json!(false));
    assert_eq!(v["listening_port"], serde_json::json!(47801));
    assert_eq!(v["occupant"]["system"], serde_json::json!("md"));
    assert_eq!(
        v["occupant"]["content"],
        serde_json::json!("/x/game_poc.md")
    );
    assert!(
        v.get("runtime_paths").is_some(),
        "graceful 응답은 runtime_paths를 포함해야"
    );
    assert!(v.get("recovery").is_some(), "복구 절차 안내가 있어야");
    // 비밀 누출 없음: occupant에 session_token 미포함
    assert!(
        v["occupant"].get("session_token").is_none(),
        "occupant는 session_token을 노출하면 안 됨"
    );
}

#[test]
fn occupied_graceful_own_stale_token_labeled_not_foreign() {
    // 토큰파일 유실로 자기 에뮬이 mismatch난 경우: foreign 오라벨 금지(무한 재연결 루프 방지).
    let occupant = EmulatorIdentity {
        system: Some("ss".into()),
        session_token: Some(emucap::live::tcp::new_session_token()), // 현재 cwd = own
        ..Default::default()
    };
    let v = occupied_graceful(&occupant, Some(47801), None);
    assert_eq!(
        v["occupied_by_foreign"],
        serde_json::json!(false),
        "own stale 토큰은 foreign 아님"
    );
    assert_eq!(v["stale_own_token"], serde_json::json!(true));
    assert!(
        v["recovery"].as_str().unwrap_or("").contains("재기동"),
        "own stale이면 relaunch 복구를 안내해야(재연결 reclaim 아님)"
    );
}

struct MismatchLink {
    caps: Capabilities,
    token: String,
    calls: usize,
}

impl MismatchLink {
    fn new() -> Self {
        Self {
            caps: Capabilities::empty(),
            token: "expected-token".into(),
            calls: 0,
        }
    }
}

impl EmulatorLink for MismatchLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        self.calls += 1;
        Err(LinkError::IdentityMismatch {
            expected: self.token.clone(),
            actual: Some("foreign-token".into()),
            identity: Box::new(EmulatorIdentity {
                system: Some("md".into()),
                adapter: Some("mednafen".into()),
                name: Some("foreign".into()),
                session_token: Some("foreign-token".into()),
                content: Some("/foreign/game.md".into()),
                ..Default::default()
            }),
        })
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(47809)
    }

    fn session_token(&self) -> Option<&str> {
        Some(&self.token)
    }
}

struct NotConnectedPortLink {
    caps: Capabilities,
    calls: usize,
}

impl NotConnectedPortLink {
    fn new() -> Self {
        Self {
            caps: Capabilities::empty(),
            calls: 0,
        }
    }
}

impl EmulatorLink for NotConnectedPortLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        self.calls += 1;
        Err(LinkError::NotConnected)
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(47810)
    }

    fn session_token(&self) -> Option<&str> {
        Some("test-token")
    }
}

#[test]
fn launch_refuses_missing_content_before_binary_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing.sfc");
    let mut link = NotConnectedPortLink::new();

    let out = make_launch(
        &mut link,
        &LaunchArgs {
            content_path: missing.display().to_string(),
            content_path2: None,
            system: Some("snes".into()),
            name: None,
            display: None,
        },
    );

    assert_eq!(out["launched"], serde_json::json!(false));
    assert_eq!(
        out["reason"],
        serde_json::json!("content_path does not exist")
    );
    assert_eq!(link.calls, 1);
}

#[test]
fn launch_refuses_missing_adapter_binary_with_precondition() {
    let _guard = env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let disc = tmp.path().join("disc.gdi");
    std::fs::write(&disc, b"fake gdi").unwrap();
    let empty_path = tmp.path().join("empty-path");
    let home = tmp.path().join("home");
    let build_home = tmp.path().join("flycast-build");
    std::fs::create_dir_all(&empty_path).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&build_home).unwrap();

    let old_flycast_app = std::env::var_os("FLYCAST_APP");
    let old_emu_home = std::env::var_os("EMUCAP_EMU_HOME");
    let old_build_home = std::env::var_os("EMUCAP_FLYCAST_BUILD_HOME");
    let old_home = std::env::var_os("HOME");
    let old_path = std::env::var_os("PATH");

    std::env::set_var("FLYCAST_APP", tmp.path().join("missing-flycast"));
    std::env::set_var("EMUCAP_EMU_HOME", tmp.path().join("emu-home"));
    std::env::set_var("EMUCAP_FLYCAST_BUILD_HOME", &build_home);
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", &empty_path);

    let mut link = NotConnectedPortLink::new();
    let out = make_launch(
        &mut link,
        &LaunchArgs {
            content_path: disc.display().to_string(),
            content_path2: None,
            system: Some("dc".into()),
            name: None,
            display: None,
        },
    );

    match old_flycast_app {
        Some(v) => std::env::set_var("FLYCAST_APP", v),
        None => std::env::remove_var("FLYCAST_APP"),
    }
    match old_emu_home {
        Some(v) => std::env::set_var("EMUCAP_EMU_HOME", v),
        None => std::env::remove_var("EMUCAP_EMU_HOME"),
    }
    match old_build_home {
        Some(v) => std::env::set_var("EMUCAP_FLYCAST_BUILD_HOME", v),
        None => std::env::remove_var("EMUCAP_FLYCAST_BUILD_HOME"),
    }
    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match old_path {
        Some(v) => std::env::set_var("PATH", v),
        None => std::env::remove_var("PATH"),
    }

    assert_eq!(out["launched"], serde_json::json!(false));
    assert_eq!(
        out["reason"],
        serde_json::json!("adapter binary is unavailable")
    );
    assert_eq!(out["adapter"], serde_json::json!("flycast"));
    assert_eq!(
        out["preconditions"]["adapter_binary"]["available"],
        serde_json::json!(false)
    );
    assert!(out["preconditions"]["build_required"]
        .as_str()
        .is_some_and(|s| s.contains("FLYCAST_APP/default install/PATH")));
    assert_eq!(link.calls, 1);
}

#[test]
fn launch_refuses_missing_pc98_bridge_with_precondition() {
    let _guard = env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let disk = tmp.path().join("game.hdi");
    let fake_mame = tmp
        .path()
        .join(if cfg!(windows) { "mame.exe" } else { "mame" });
    std::fs::write(&disk, b"fake hdi").unwrap();
    std::fs::write(&fake_mame, b"fake mame").unwrap();
    make_executable(&fake_mame);

    let old_mame_bin = std::env::var_os("MAME_BIN");
    let old_bridge_bin = std::env::var_os("EMUCAP_PC98_BRIDGE_BIN");
    let old_bridge = std::env::var_os("EMUCAP_PC98_BRIDGE");
    std::env::set_var("MAME_BIN", &fake_mame);
    std::env::set_var("EMUCAP_PC98_BRIDGE", "rust");
    std::env::set_var("EMUCAP_PC98_BRIDGE_BIN", tmp.path().join("missing-bridge"));

    let mut link = NotConnectedPortLink::new();
    let out = make_launch(
        &mut link,
        &LaunchArgs {
            content_path: disk.display().to_string(),
            content_path2: None,
            system: Some("pc98".into()),
            name: None,
            display: None,
        },
    );

    match old_mame_bin {
        Some(v) => std::env::set_var("MAME_BIN", v),
        None => std::env::remove_var("MAME_BIN"),
    }
    match old_bridge_bin {
        Some(v) => std::env::set_var("EMUCAP_PC98_BRIDGE_BIN", v),
        None => std::env::remove_var("EMUCAP_PC98_BRIDGE_BIN"),
    }
    match old_bridge {
        Some(v) => std::env::set_var("EMUCAP_PC98_BRIDGE", v),
        None => std::env::remove_var("EMUCAP_PC98_BRIDGE"),
    }

    assert_eq!(out["launched"], serde_json::json!(false));
    assert_eq!(
        out["reason"],
        serde_json::json!("mame_pc98 bridge is unavailable")
    );
    assert_eq!(
        out["preconditions"]["adapter_binary"]["available"],
        serde_json::json!(true)
    );
    assert_eq!(
        out["preconditions"]["bridge"]["available"],
        serde_json::json!(false)
    );
    assert!(out["preconditions"]["bridge"]["error"]
        .as_str()
        .is_some_and(|s| s.contains("EMUCAP_PC98_BRIDGE_BIN")));
    assert_eq!(link.calls, 1);
}

#[test]
fn launch_refuses_occupied_port_before_spawn() {
    let mut link = MismatchLink::new();
    let out = make_launch(
        &mut link,
        &LaunchArgs {
            content_path: "/tmp/game.md".into(),
            content_path2: None,
            system: Some("md".into()),
            name: None,
            display: None,
        },
    );

    assert_eq!(out["launched"], serde_json::json!(false));
    assert_eq!(
        out["status"]["occupied_by_foreign"],
        serde_json::json!(true)
    );
    assert_eq!(
        out["status"]["occupant"]["name"],
        serde_json::json!("foreign")
    );
    assert_eq!(link.calls, 1);
}

/// This session already has a live, connected emulator on its listening port.
struct ConnectedLink {
    caps: Capabilities,
    calls: usize,
}

impl ConnectedLink {
    fn new() -> Self {
        let mut caps = Capabilities::empty();
        // emulator_identity is injected from the link's advertised identity, not the status body.
        caps.identity = EmulatorIdentity {
            system: Some("pc98".into()),
            adapter: Some("mame-pc98-rust-gdb".into()),
            name: Some("existing-A".into()),
            content: Some("/tmp/existing.hdm".into()),
            ..Default::default()
        };
        Self { caps, calls: 0 }
    }
}

impl EmulatorLink for ConnectedLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        self.calls += 1;
        Ok(serde_json::json!({ "connected": true, "system": "pc98" }))
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(47811)
    }

    fn session_token(&self) -> Option<&str> {
        Some("test-token")
    }
}

#[test]
fn launch_refuses_when_this_session_already_connected() {
    let mut link = ConnectedLink::new();
    let out = make_launch(
        &mut link,
        &LaunchArgs {
            content_path: "/tmp/second.hdm".into(),
            content_path2: None,
            system: Some("pc98".into()),
            name: Some("dup-B".into()),
            display: None,
        },
    );

    // Refused before any spawn: only the pre-launch status probe ran.
    assert_eq!(out["launched"], serde_json::json!(false));
    assert!(out["reason"]
        .as_str()
        .is_some_and(|s| s.contains("already connected")));
    // The agent is shown what is already attached so it can decide to tear it down.
    assert_eq!(
        out["connected_emulator"]["name"],
        serde_json::json!("existing-A")
    );
    assert_eq!(link.calls, 1);
}
