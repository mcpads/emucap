use std::path::{Path, PathBuf};

use emucap::live::link::{EmulatorIdentity, EmulatorLink, LinkError};
use emucap::live::tcp;
use emucap::live::tools::{self, ToolOutput};

use crate::launch::occupied_graceful;

/// 이 MCP 바이너리가 빌드된 emucap git hash(build.rs가 OUT_DIR에 기록; include_str!로 cargo가 파일 의존성을
/// 추적해 hash 변경 시 이 파일이 재컴파일된다). status.server_build로 노출. `\n` 없이 정확히 hash 문자열.
pub(crate) const BUILD_HASH: &str = include_str!(concat!(env!("OUT_DIR"), "/emucap_build_hash"));

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;

#[path = "status/button_hints.rs"]
mod button_hints;
pub(crate) use button_hints::button_hint_for_system;

/// get_rom_info 응답에 균일 `rom_sha1` 필드를 삽입한다 — 정규화된 콘텐츠 해시(content_md5 우선,
/// 없으면 sha1; 빈값·"skipped:too_large"는 무효로 보고 폴백). 어댑터가 어떤 해시를 쓰든 에이전트가
/// 플랫폼별 필드를 고를 필요 없이 이 필드를 추적 MCP run_start에 넘긴다. 해시를 전혀 안 주는 백엔드는
/// 무효라 필드가 안 생긴다(→ 호출자 shasum 폴백). 기존 필드는 보존하고 이미 있으면 덮어쓰지 않는다.
pub(crate) fn normalize_rom_sha1(v: &mut serde_json::Value) {
    fn valid(s: Option<&str>) -> Option<&str> {
        s.filter(|s| !s.is_empty() && *s != "skipped:too_large")
    }
    let Some(obj) = v.as_object_mut() else { return };
    if obj.contains_key("rom_sha1") {
        return;
    }
    let canon = valid(obj.get("content_md5").and_then(|x| x.as_str()))
        .or_else(|| valid(obj.get("sha1").and_then(|x| x.as_str())))
        .map(String::from);
    if let Some(c) = canon {
        obj.insert("rom_sha1".into(), serde_json::json!(c));
    }
}

pub(crate) fn enrich_status_value(
    v: &mut serde_json::Value,
    methods: &[String],
    memory_types: &[String],
    fallback_system: Option<&str>,
) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    let connected = obj
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !connected {
        return;
    }
    if !obj.contains_key("input_buttons") {
        // 어댑터가 status 최상위 system을 안 실을 수 있으므로(예: Flycast) 어댑터가 advertise한
        // emulator_identity.system을 fallback으로 쓴다. 어느 쪽도 알려진 system이 아니면 생략한다.
        let system = obj
            .get("system")
            .and_then(|v| v.as_str())
            .or(fallback_system);
        if let Some(hint) = button_hint_for_system(system) {
            obj.insert("input_buttons".into(), hint);
        }
    }
    if let Some(status_methods) = obj.get("methods").and_then(serde_json::Value::as_array) {
        let status_methods = status_methods
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(String::from)
            .collect::<Vec<_>>();
        obj.insert(
            "methods".into(),
            serde_json::json!(public_method_names(&status_methods)),
        );
    } else if !methods.is_empty() {
        obj.insert(
            "methods".into(),
            serde_json::json!(public_method_names(methods)),
        );
    }
    if !obj.contains_key("memory_types") && !memory_types.is_empty() {
        obj.insert("memory_types".into(), serde_json::json!(memory_types));
    }
    // capability_notes: 어댑터가 직접 제공하면(PC-98은 dict) 그 값을 *보존*한다. 제공이 없거나
    // 배열이면, 메서드 부재에서 *신뢰 가능한* substitute만 도출해 덧붙인다(정적 capability 맵 아님 —
    // capability는 methods에서 판단한다). 어댑터가 직접 advertise하는 명령 단위 step 능력은 외부의 step(unit)에
    // 합쳐지므로 여기서 별도 capability note로 반복하지 않는다 — 여기선 *메서드로
    // 표현되지 않는 substitute*(트레이스 부재 시 콜체인 역추적 등)만 도출한다.
    {
        let adapter_provided = obj
            .get("capability_notes")
            .map(|v| !v.is_array())
            .unwrap_or(false);
        if !adapter_provided {
            let has = |m: &str| methods.iter().any(|x| x == m);
            let mut notes: Vec<String> = obj
                .get("capability_notes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            // 명령단위 추적·콜스택·레지스터워치 부재 → exec BP 콜체인 역추적 대체. watch_register/set_trace는
            // Mesen·PC-98만 보유하는 일관된 토큰이라 부재 도출이 신뢰 가능(Mednafen·Flycast에서만 발화).
            if !has("watch_register") && !has("set_trace") && has("set_breakpoint") {
                let mut missing = vec!["set_trace/get_trace"];
                if !has("call_stack") {
                    missing.push("call_stack");
                }
                missing.push("watch_register");
                // step 입자는 플랫폼별: Mednafen은 명령 단위, Flycast는 프레임 단위만 지원한다.
                let step_kind = if has("step_instructions") {
                    "frozen step(unit=instructions)"
                } else {
                    "frozen step(unit=frames)"
                };
                notes.push(format!("{} unavailable; partially substitute by moving an exec breakpoint backward one caller at a time, then use {step_kind} and disassemble. Pair this with static disassembly for indirect jumps, self-modifying code, and jump-table recovery.", missing.join(", ")));
            }
            if !notes.is_empty() {
                obj.insert("capability_notes".into(), serde_json::json!(notes));
            }
        }
    }
}

pub(crate) fn enrich_breakpoint_kinds(
    v: &mut serde_json::Value,
    breakpoint_kinds: &[serde_json::Value],
) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    if !obj
        .get("connected")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
        || obj.contains_key("breakpoint_kinds")
        || breakpoint_kinds.is_empty()
    {
        return;
    }
    obj.insert(
        "breakpoint_kinds".into(),
        serde_json::json!(breakpoint_kinds),
    );
}

fn public_method_names(methods: &[String]) -> Vec<String> {
    let mut normalized = Vec::with_capacity(methods.len());
    for method in methods {
        let method = if method == "step_instructions" {
            "step"
        } else {
            method.as_str()
        };
        if !normalized.iter().any(|known| known == method) {
            normalized.push(method.to_string());
        }
    }
    normalized
}

pub(crate) fn enrich_contract_status(
    v: &mut serde_json::Value,
    identity: &EmulatorIdentity,
    advertisement: &emucap::contracts::ContractAdvertisement,
) {
    let connected = v
        .get("connected")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if !connected {
        return;
    }
    let methods: Vec<String> = v
        .get("methods")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let mut contracts = emucap::contracts::validate_advertisement(
        advertisement,
        identity.adapter.as_deref(),
        identity.system.as_deref(),
        &methods,
    );
    let adapter_sync_limit = |pointer: &str| {
        v.pointer(pointer)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(crate::args::MAX_SYNC_ADVANCE_COUNT)
            .min(crate::args::MAX_SYNC_ADVANCE_COUNT)
    };
    let step_limit = adapter_sync_limit("/execution_limits/max_sync_advance_count");
    let run_frames_limit = v
        .pointer("/execution_limits/frame/max_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(step_limit)
        .min(step_limit);
    if methods.iter().any(|method| method == "write_memory") {
        contracts.constraints.insert(
            "memory.write.input_sources".into(),
            serde_json::json!(["hex", "file"]),
        );
        contracts.constraints.insert(
            "memory.write.max_bytes".into(),
            serde_json::json!(tools::MAX_WRITE_BYTES),
        );
        contracts.constraints.insert(
            "memory.write.file_load_timeout_ms".into(),
            serde_json::json!(crate::memory_write::FILE_LOAD_TIMEOUT_MS),
        );
    }
    if methods
        .iter()
        .any(|method| method == "step" || method == "step_instructions")
    {
        contracts.constraints.insert(
            "execution.step.max_count".into(),
            serde_json::json!(step_limit),
        );
    }
    if methods.iter().any(|method| method == "run_frames") {
        contracts.constraints.insert(
            "execution.run_frames.max_frames".into(),
            serde_json::json!(run_frames_limit),
        );
    }
    if contracts.state == "validated" {
        add_composite_methods(v, &contracts);
    }
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "contracts".into(),
            serde_json::to_value(contracts).unwrap_or_else(|_| {
                serde_json::json!({
                    "catalog": emucap::contracts::CATALOG_ID,
                    "state": "unvalidated",
                    "errors": ["failed to serialize contract validation result"],
                })
            }),
        );
    }
}

fn add_composite_methods(v: &mut serde_json::Value, contracts: &emucap::contracts::ContractStatus) {
    let Some(methods) = v
        .get_mut("methods")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    let has = |method: &str| methods.iter().any(|value| value == method);
    let raw_has = |method: &str| has(method);
    let frame_step_available = contracts
        .constraints
        .get("execution.step.units")
        .map(|units| {
            units
                .as_array()
                .is_some_and(|units| units.iter().any(|unit| unit == "frames"))
        })
        .unwrap_or(true);
    let tap_ready =
        frame_step_available && raw_has("set_input") && raw_has("step") && raw_has("pause");
    let hold_until_ready = tap_ready && raw_has("read_memory");
    let probe_ready = raw_has("probe");
    let replay_ready = probe_ready || raw_has("load_state");

    for (ready, method) in [
        (tap_ready, "tap"),
        (hold_until_ready, "hold_until"),
        (replay_ready, "regression_run"),
        (replay_ready, "verify_determinism"),
    ] {
        if ready && !methods.iter().any(|value| value == method) {
            methods.push(serde_json::json!(method));
        }
    }
}

fn redact_identity(identity: &EmulatorIdentity) -> serde_json::Value {
    let mut v = serde_json::to_value(identity).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        if obj.remove("session_token").is_some() {
            obj.insert("session_token_present".into(), serde_json::json!(true));
        }
    }
    v
}

fn token_file_status(port: Option<u16>) -> serde_json::Value {
    match port {
        Some(p) => {
            let path = tcp::session_token_path(p);
            serde_json::json!({
                "path": path.display().to_string(),
                "present": path.is_file(),
            })
        }
        None => serde_json::Value::Null,
    }
}

fn has_repo_markers(dir: &Path) -> bool {
    repo_path(dir, &["adapters", "mesen2", "launch.sh"]).is_file()
        && repo_path(dir, &["adapters", "mednafen", "launch.sh"]).is_file()
        && repo_path(dir, &["adapters", "mame-pc98", "launch.sh"]).is_file()
}

pub(crate) fn find_repo_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("EMUCAP_REPO_ROOT") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors() {
            candidates.push(ancestor.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors() {
            candidates.push(ancestor.to_path_buf());
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    candidates
        .into_iter()
        .find(|candidate| has_repo_markers(candidate))
}

fn repo_path(root: &Path, parts: &[&str]) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in parts {
        path.push(part);
    }
    path
}

fn abs_path_json(root: &Path, parts: &[&str]) -> serde_json::Value {
    repo_path(root, parts).display().to_string().into()
}

fn mesen_platform_launcher(root: &Path) -> PathBuf {
    let ps1 = repo_path(root, &["adapters", "mesen2", "launch.ps1"]);
    if cfg!(windows) && ps1.is_file() {
        ps1
    } else {
        repo_path(root, &["adapters", "mesen2", "launch.sh"])
    }
}

fn powershell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn legacy_mesen_command(root: &Path, port: u16) -> String {
    let launcher = mesen_platform_launcher(root);
    if launcher.extension().and_then(|e| e.to_str()) == Some("ps1") {
        format!(
            "powershell -ExecutionPolicy Bypass -File {} <ROM> {port} [name] [system]",
            powershell_quote(&launcher)
        )
    } else {
        format!("{} <ROM> {port} [name] [system]", launcher.display())
    }
}

fn native_legacy_script(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    path.is_file()
        && if cfg!(windows) {
            ext.is_some_and(|e| e.eq_ignore_ascii_case("ps1"))
        } else {
            ext == Some("sh")
        }
}

fn legacy_command_template(launcher: &Path, command: String) -> serde_json::Value {
    if native_legacy_script(launcher) {
        serde_json::json!(command)
    } else {
        serde_json::Value::Null
    }
}

fn legacy_fallback_entry(launcher: &Path, command: String) -> serde_json::Value {
    let available = native_legacy_script(launcher);
    serde_json::json!({
        "available_on_this_host": available,
        "launcher": if available {
            serde_json::json!(launcher.display().to_string())
        } else {
            serde_json::Value::Null
        },
        "command_template": if available {
            serde_json::json!(command)
        } else {
            serde_json::Value::Null
        },
        "reason": if available {
            "native script for this host"
        } else {
            "no native legacy script for this host; use command_templates.preferred"
        },
    })
}

pub(crate) fn runtime_paths(port: Option<u16>) -> serde_json::Value {
    let runtime_store = emucap::live::runtime::RuntimeStore::discover();
    let capsule_paths = serde_json::json!({
        "root": runtime_store.root().display().to_string(),
        "current": port.map(|p| runtime_store.current_path(p).display().to_string()),
    });
    let Some(root) = find_repo_root() else {
        return serde_json::json!({
            "repo_root": null,
            "repo_root_env": "EMUCAP_REPO_ROOT",
            "runtime_capsule": capsule_paths,
            "error": "emucap repo root not found from EMUCAP_REPO_ROOT, current_exe, cwd, or CARGO_MANIFEST_DIR",
        });
    };
    let token_file = port.map(tcp::session_token_path);
    let mesen_launcher = mesen_platform_launcher(&root);
    let mednafen_launcher = repo_path(&root, &["adapters", "mednafen", "launch.sh"]);
    let mame_launcher = repo_path(&root, &["adapters", "mame-pc98", "launch.sh"]);
    let flycast_launcher = repo_path(&root, &["adapters", "flycast", "launch.sh"]);
    let desmume_launcher = repo_path(&root, &["adapters", "desmume-nds", "launch.sh"]);
    let ppsspp_launcher = repo_path(&root, &["adapters", "ppsspp", "launch.sh"]);
    let pcsx2_launcher = repo_path(&root, &["adapters", "pcsx2", "launch.sh"]);
    let dolphin_launcher = repo_path(&root, &["adapters", "dolphin", "launch-native.ps1"]);
    serde_json::json!({
        "repo_root": root.display().to_string(),
        "repo_root_env": "EMUCAP_REPO_ROOT",
        "token_file": token_file.map(|p| p.display().to_string()),
        "runtime_capsule": capsule_paths,
        "adapters": {
            "mesen2": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "mesen2", if cfg!(windows) { "build.ps1" } else { "build.sh" }]),
                "launch": abs_path_json(&root, &["adapters", "mesen2", "launch.sh"]),
                "windows_script": abs_path_json(&root, &["adapters", "mesen2", "launch.ps1"]),
                "platform_launch": mesen_launcher.display().to_string(),
                "lua": abs_path_json(&root, &["adapters", "mesen2", "emucap-core.lua"]),
            },
            "mednafen": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "mednafen", "build.sh"]),
                "launch": abs_path_json(&root, &["adapters", "mednafen", "launch.sh"]),
                "work_dir": abs_path_json(&root, &["adapters", "mednafen", "work"]),
            },
            "mame_pc98": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "mame-pc98", "build.sh"]),
                "launch": abs_path_json(&root, &["adapters", "mame-pc98", "launch.sh"]),
                "headless_wrapper": abs_path_json(&root, &["adapters", "mame-pc98", "mame-headless.sh"]),
                "work_source_dir": abs_path_json(&root, &["adapters", "mame-pc98", "work", "mame-src"]),
                "work_wrapper": abs_path_json(&root, &["adapters", "mame-pc98", "work", "mame"]),
                "work_raw_binary": abs_path_json(&root, &["adapters", "mame-pc98", "work", "mame.raw"]),
            },
            "mame_neogeo": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "mame-neogeo", "build.sh"]),
                "bridge_binary": abs_path_json(&root, &["target", "release", if cfg!(windows) { "emucap-mame-neogeo-bridge.exe" } else { "emucap-mame-neogeo-bridge" }]),
            },
            "mupen64plus": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "mupen64plus", "build.sh"]),
                "adapter_binary": abs_path_json(&root, &["target", "release", if cfg!(windows) { "emucap-mupen64plus.exe" } else { "emucap-mupen64plus" }]),
                "plugin_root": emucap::launch::mupen64plus::default_root(&root).display().to_string(),
            },
            "openmsx": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "openmsx", "build.sh"]),
                "bridge_binary": abs_path_json(&root, &["target", "release", if cfg!(windows) { "emucap-openmsx-bridge.exe" } else { "emucap-openmsx-bridge" }]),
                "work_dir": abs_path_json(&root, &["adapters", "openmsx", "work"]),
            },
            "flycast": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "flycast", "build.sh"]),
                "launch": abs_path_json(&root, &["adapters", "flycast", "launch.sh"]),
            },
            "desmume_nds": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "desmume-nds", "build.sh"]),
                "launch": abs_path_json(&root, &["adapters", "desmume-nds", "launch.sh"]),
            },
            "ppsspp": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "ppsspp", "build.sh"]),
                "launch": abs_path_json(&root, &["adapters", "ppsspp", "launch.sh"]),
            },
            "pcsx2": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "pcsx2", "build.sh"]),
                "bios_env": "EMUCAP_PCSX2_BIOS",
            },
            "dolphin": {
                "preferred_launcher": "MCP tool: launch",
                "build": abs_path_json(&root, &["adapters", "dolphin", if cfg!(windows) { "build.ps1" } else { "build.sh" }]),
                "windows_script": abs_path_json(&root, &["adapters", "dolphin", "launch-native.ps1"]),
            }
        },
        "command_templates": port.map(|p| serde_json::json!({
            "preferred": "launch(content_path, system?, name?)",
            "legacy_mesen2": legacy_command_template(&mesen_launcher, legacy_mesen_command(&root, p)),
            "legacy_mednafen": legacy_command_template(&mednafen_launcher, format!("{} <disc_or_rom> {p} [name] [force_module]", mednafen_launcher.display())),
            "legacy_mame_pc98": legacy_command_template(&mame_launcher, format!("{} <disk.hdi|disk.hdm|disk.d88> {p} [name] [machine]", mame_launcher.display())),
            "legacy_flycast": legacy_command_template(&flycast_launcher, format!("{} <disc.gdi|disc.cdi|disc.chd|disc.cue> {p}", flycast_launcher.display())),
            "legacy_desmume_nds": legacy_command_template(&desmume_launcher, format!("{} <rom.nds> {p} [name]", desmume_launcher.display())),
            "legacy_ppsspp": legacy_command_template(&ppsspp_launcher, format!("{} <game.iso|game.cso|game.pbp> {p} [name]", ppsspp_launcher.display())),
            "legacy_pcsx2": legacy_command_template(&pcsx2_launcher, format!("{} <game.iso> {p} [name]", pcsx2_launcher.display())),
            "legacy_dolphin": legacy_command_template(&dolphin_launcher, format!("powershell -ExecutionPolicy Bypass -File {} <game.gcm|game.iso|game.wbfs> {p} [name]", dolphin_launcher.display())),
        })),
        "legacy_fallbacks": port.map(|p| serde_json::json!({
            "mesen2": legacy_fallback_entry(&mesen_launcher, legacy_mesen_command(&root, p)),
            "mednafen": legacy_fallback_entry(&mednafen_launcher, format!("{} <disc_or_rom> {p} [name] [force_module]", mednafen_launcher.display())),
            "mame_pc98": legacy_fallback_entry(&mame_launcher, format!("{} <disk.hdi|disk.hdm|disk.d88> {p} [name] [machine]", mame_launcher.display())),
            "flycast": legacy_fallback_entry(&flycast_launcher, format!("{} <disc.gdi|disc.cdi|disc.chd|disc.cue> {p}", flycast_launcher.display())),
            "desmume_nds": legacy_fallback_entry(&desmume_launcher, format!("{} <rom.nds> {p} [name]", desmume_launcher.display())),
            "ppsspp": legacy_fallback_entry(&ppsspp_launcher, format!("{} <game.iso|game.cso|game.pbp> {p} [name]", ppsspp_launcher.display())),
            "pcsx2": legacy_fallback_entry(&pcsx2_launcher, format!("{} <game.iso> {p} [name]", pcsx2_launcher.display())),
            "dolphin": legacy_fallback_entry(&dolphin_launcher, format!("powershell -ExecutionPolicy Bypass -File {} <game.gcm|game.iso|game.wbfs> {p} [name]", dolphin_launcher.display())),
        })),
    })
}

pub(crate) fn supported_systems_value() -> serde_json::Value {
    serde_json::json!([
        {
            "system": "snes",
            "adapter": "mesen2",
            "content": ["sfc", "smc"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mesen2.platform_launch"
        },
        {
            "system": "gamegear",
            "aliases": ["gg", "game-gear", "sms", "master-system"],
            "adapter": "mesen2",
            "content": ["gg", "sms"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mesen2.platform_launch"
        },
        {
            "system": "gb",
            "aliases": ["gameboy", "game-boy", "dmg"],
            "adapter": "mesen2",
            "content": ["gb"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mesen2.platform_launch"
        },
        {
            "system": "gbc",
            "aliases": ["gbcolor", "gameboycolor", "game-boy-color", "cgb"],
            "adapter": "mesen2",
            "content": ["gbc"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mesen2.platform_launch",
            "notes": "GB and GBC share the emucap-gb.lua entry (Mesen gameboy console / SM83 CPU)."
        },
        {
            "system": "gba",
            "aliases": ["gameboyadvance", "game-boy-advance", "agb"],
            "adapter": "mesen2",
            "content": ["gba"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mesen2.platform_launch",
            "notes": "ARM7: disassemble/call_stack are unsupported; memory/state/BP/save/input/screenshot are supported."
        },
        {
            "system": "nes",
            "aliases": ["famicom", "fc", "nintendo"],
            "adapter": "mesen2",
            "content": ["nes"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mesen2.platform_launch",
            "notes": "6502/2A03: disassemble/call_stack/break_on_reset supported; memory/state/BP/save/input/screenshot supported."
        },
        {
            "system": "n64",
            "aliases": ["nintendo64", "nintendo-64"],
            "adapter": "mupen64plus",
            "content": ["z64", "n64", "v64"],
            "launcher": "MCP tool: launch",
            "notes": "The Unix adapter uses the pinned Mupen64Plus pure interpreter. Both modes expose pause/resume, reset, R4300 instruction step and state, bounded frozen RDRAM access, port-0 persistent input with explicit release, R4300 exec/read/write breakpoints, event polling, and disassembly. Visible launch also exposes exact rendered-frame step, bounded run_frames and input pulse, current PNG capture, and completion-checked native save/load. Headless launch omits rendered-frame operations. RSP state is not exposed."
        },
        {
            "system": "msx",
            "aliases": ["msx2+", "msx2plus", "openmsx"],
            "adapter": "openmsx",
            "content": ["rom", "mx1", "mx2", "ri", "sg"],
            "launcher": "MCP tool: launch",
            "notes": "Pinned openMSX 21.0 C-BIOS MSX2+ cartridge profile. It exposes Z80 exec/read/write breakpoints with atomic evidence and disassembly in addition to frame/instruction control, memory, state, input, and visible frozen screenshots. Generic .rom files require system=msx."
        },
        {
            "system": "msx1",
            "aliases": [],
            "adapter": "openmsx",
            "content": ["rom", "mx1", "mx2", "ri", "sg", "cas", "tsx", "wav"],
            "launcher": "MCP tool: launch",
            "notes": "Pinned Philips VG 8020 real-firmware profile. Set EMUCAP_OPENMSX_FIRMWARE to an absolute firmware directory. Cartridge and cassette media are admitted; disk is not."
        },
        {
            "system": "msx2",
            "aliases": [],
            "adapter": "openmsx",
            "content": ["rom", "mx1", "mx2", "ri", "sg", "dsk", "cas", "tsx", "wav"],
            "launcher": "MCP tool: launch",
            "notes": "Pinned Philips NMS 8250 real-firmware profile. Set EMUCAP_OPENMSX_FIRMWARE to an absolute firmware directory. Mutable media is mounted from an isolated working copy."
        },
        {
            "system": "msx2p",
            "aliases": [],
            "adapter": "openmsx",
            "content": ["rom", "mx1", "mx2", "ri", "sg", "dsk", "cas", "tsx", "wav"],
            "launcher": "MCP tool: launch",
            "notes": "Pinned Panasonic FS-A1WSX real-firmware profile. It is not the legacy msx2+ alias. Set EMUCAP_OPENMSX_FIRMWARE to an absolute firmware directory."
        },
        {
            "system": "saturn",
            "aliases": ["ss"],
            "adapter": "mednafen",
            "content": ["cue", "chd"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mednafen.launch"
        },
        {
            "system": "psx",
            "aliases": ["ps1", "playstation"],
            "adapter": "mednafen",
            "content": ["cue", "bin", "chd", "iso"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mednafen.launch"
        },
        {
            "system": "pce",
            "aliases": ["pcengine", "pc-engine", "pce-cd"],
            "adapter": "mednafen",
            "content": ["cue", "pce", "chd"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mednafen.launch",
            "force_module": "pce"
        },
        {
            "system": "pcfx",
            "aliases": ["pc-fx"],
            "adapter": "mednafen",
            "content": ["cue", "ccd", "toc", "m3u"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mednafen.launch",
            "force_module": "pcfx",
            "required_firmware": ["pcfx.rom"],
            "notes": "Disc formats are ambiguous; pass system=pcfx explicitly. V810 call_stack is a best-effort trace-derived shadow stack."
        },
        {
            "system": "md",
            "aliases": ["genesis", "megadrive", "mega-drive"],
            "adapter": "mednafen",
            "content": ["md", "gen", "smd", "bin"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mednafen.launch",
            "force_module": "md",
            "notes": ".bin is only inferred as MD when a Mega Drive/Genesis header is present; otherwise pass system=md explicitly"
        },
        {
            "system": "wswan",
            "aliases": ["ws", "wsc", "wonderswan", "wonderswan-color", "wonderswancolor", "wonderswan_color"],
            "adapter": "mednafen",
            "content": ["ws", "wsc", "wsr"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mednafen.launch",
            "force_module": "wswan",
            "notes": "WonderSwan and WonderSwan Color share the Mednafen wswan module."
        },
        {
            "system": "ngp",
            "aliases": ["ngpc", "neo-geo-pocket", "neo-geo-pocket-color", "neogeo-pocket", "neogeo-pocket-color"],
            "adapter": "mednafen",
            "content": ["ngp", "ngpc", "ngc", "npc"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mednafen.launch",
            "force_module": "ngp",
            "notes": "Neo Geo Pocket and Pocket Color share the patched Mednafen ngp module. Its TLCS-900/H profile exposes side-effect-free RAM/ROM/BIOS views, RAM writes, exact instruction step, safe disassembly, and exec-only breakpoints. Sound-Z80 state, read/write breakpoints, trace, and call-stack classification are not exposed."
        },
        {
            "system": "pc98",
            "aliases": ["pc-98", "mame-pc98"],
            "adapter": "mame_pc98",
            "content": ["hdi", "hdm", "d88"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mame_pc98.launch"
        },
        {
            "system": "neogeo_mvs",
            "aliases": ["neo-geo-mvs", "neogeo-mvs", "mvs"],
            "adapter": "mame_neogeo",
            "content": ["zip"],
            "launcher": "MCP tool: launch",
            "required_firmware": ["neogeo.zip"],
            "notes": ".zip is not auto-inferred; pass system=neogeo_mvs explicitly. AES, CD, Pocket/Color, and Hyper Neo Geo 64 are separate targets and are not accepted as aliases."
        },
        {
            "system": "neogeo_aes",
            "aliases": ["neo-geo-aes", "neogeo-aes", "aes"],
            "adapter": "mame_neogeo",
            "content": ["zip"],
            "launcher": "MCP tool: launch",
            "required_firmware": ["aes.zip"],
            "notes": ".zip is not auto-inferred; pass system=neogeo_aes explicitly. The ZIP stem must name an AES-compatible entry in the pinned MAME Neo Geo software list."
        },
        {
            "system": "neogeo_cd",
            "aliases": ["neo-geo-cd", "neogeo-cd", "ngcd"],
            "adapter": "mame_neogeo",
            "content": ["cue"],
            "launcher": "MCP tool: launch",
            "required_firmware": ["neocdz.zip"],
            "notes": "CUE is ambiguous; pass system=neogeo_cd explicitly. The content identity covers the CUE and every referenced file. Native save/load is not advertised."
        },
        {
            "system": "dc",
            "aliases": ["dreamcast", "flycast"],
            "adapter": "flycast",
            "content": ["gdi", "cdi", "chd", "cue"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.flycast.launch"
        },
        {
            "system": "nds",
            "aliases": ["ds", "nintendo-ds", "desmume"],
            "adapter": "desmume_nds",
            "content": ["nds"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.desmume_nds.launch"
        },
        {
            "system": "psp",
            "aliases": ["ppsspp", "playstation-portable"],
            "adapter": "ppsspp",
            "content": ["iso", "cso", "pbp"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.ppsspp.launch",
            "notes": ".iso is shared with Saturn/PSX/PCE/MD/Dreamcast — a PSP GAME ISO9660 header disambiguates automatically; otherwise pass system=psp explicitly."
        },
        {
            "system": "ps2",
            "aliases": ["pcsx2", "playstation2", "playstation-2"],
            "adapter": "pcsx2",
            "content": ["iso"],
            "launcher": "MCP tool: launch",
            "required_environment": ["EMUCAP_PCSX2_BIOS"],
            "notes": "An ISO9660 SYSTEM.CNF BOOT2 entry is inferred automatically. The pinned PCSX2 fork and Rust bridge are required."
        },
        {
            "system": "gamecube",
            "aliases": ["gc", "ngc", "game-cube"],
            "adapter": "dolphin",
            "content": ["gcm", "iso", "rvz", "gcz"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.dolphin.windows_script",
            "notes": ".gcm and the GameCube disc magic are inferred automatically; shared container extensions require system=gamecube."
        },
        {
            "system": "wii",
            "aliases": ["nintendo-wii"],
            "adapter": "dolphin",
            "content": ["wbfs", "iso", "rvz", "wia", "gcz"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.dolphin.windows_script",
            "notes": ".wbfs and the Wii disc magic are inferred automatically; shared container extensions require system=wii."
        }
    ])
}

pub(crate) fn supported_system_names() -> String {
    supported_systems_value()
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|system| system["system"].as_str())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn unknown_content_question() -> String {
    format!(
        "Which ROM, disc, or disk path should be launched, and for which system ({})?",
        supported_system_names()
    )
}

pub(crate) fn required_unknown_content_input() -> String {
    format!(
        "Ask for content_path and a system ({}) before calling launch_plan(content_path, system).",
        supported_system_names()
    )
}

pub(crate) fn make_bootstrap_value(
    link: &mut dyn EmulatorLink,
) -> Result<serde_json::Value, LinkError> {
    let status = tools::status(link);
    let port = link.endpoint_port();
    let token = link.session_token().map(str::to_string);
    let identity = link.capabilities().identity.clone();
    let contracts = link.capabilities().contracts.clone();

    let mut status_value = match status {
        Ok(ToolOutput::Json(mut v)) => {
            let methods = link.capabilities().methods.clone();
            let memory_types = link.capabilities().memory_types.clone();
            let breakpoint_kinds = link.capabilities().breakpoint_kinds.clone();
            enrich_status_value(&mut v, &methods, &memory_types, identity.system.as_deref());
            enrich_breakpoint_kinds(&mut v, &breakpoint_kinds);
            enrich_contract_status(&mut v, &identity, &contracts);
            enrich_link_status(&mut v, port, token.as_deref(), Some(&identity));
            enrich_continuity(&mut v, link);
            v["request_succeeded"] = serde_json::json!(true);
            v
        }
        Ok(_) => serde_json::json!({"connected": true}),
        Err(LinkError::NotConnected) => {
            let mut v = serde_json::json!({
                "connected": false,
                "listening_port": port,
            });
            enrich_link_status(&mut v, port, token.as_deref(), None);
            enrich_continuity(&mut v, link);
            v["request_succeeded"] = serde_json::json!(false);
            v
        }
        Err(LinkError::IdentityMismatch { identity, .. }) => {
            occupied_graceful(&identity, port, token.as_deref())
        }
        Err(e) if is_observation_failure(&e) => {
            let mut v = serde_json::json!({
                "connected": false,
                "request_succeeded": false,
                "error_kind": e.kind(),
                "error": e.to_string(),
                "listening_port": port,
            });
            enrich_link_status(&mut v, port, token.as_deref(), None);
            enrich_continuity(&mut v, link);
            v
        }
        Err(e) => return Err(e),
    };

    // Also covers the identity-mismatch branch, whose graceful response is assembled separately.
    enrich_continuity(&mut status_value, link);

    if let Some(obj) = status_value.as_object_mut() {
        obj.entry("listening_port")
            .or_insert_with(|| port.map_or(serde_json::Value::Null, |p| serde_json::json!(p)));
    }

    let unknown_content_question = unknown_content_question();

    Ok(serde_json::json!({
        "ok": true,
        "start_here": true,
        "first_tool": "bootstrap",
        "connected": status_value
            .get("connected")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        "listening_port": port,
        "status": status_value,
        "runtime_paths": runtime_paths(port),
        "supported_systems": supported_systems_value(),
        "required_user_input_if_content_unknown": required_unknown_content_input(),
        "question_to_user_if_content_unknown": unknown_content_question.clone(),
        "workflow": {
            "unknown_content": {
                "ask_user": unknown_content_question,
                "then_call": "launch_plan",
                "required_args": ["content_path", "system"]
            },
            "known_content": {
                "then_call": "launch_plan",
                "required_args": ["content_path"],
                "optional_args": ["system"]
            },
            "already_running": {
                "then_call": "status"
            }
        },
        "next_action": "When content_path is known, call launch_plan(content_path, system?). Otherwise ask question_to_user_if_content_unknown verbatim.",
        "do_not": "Do not infer and execute a runtime_paths command_template when content_path or system is unknown."
    }))
}

pub(crate) fn is_observation_failure(error: &LinkError) -> bool {
    matches!(
        error,
        LinkError::NotConnected
            | LinkError::PortBusy { .. }
            | LinkError::Timeout
            | LinkError::Protocol(_)
    )
}

pub(crate) fn enrich_continuity(v: &mut serde_json::Value, link: &dyn EmulatorLink) {
    let continuity = link.continuity();
    let Some(object) = v.as_object_mut() else {
        return;
    };
    object.insert(
        "continuity".into(),
        serde_json::to_value(&continuity).unwrap_or_else(|_| serde_json::json!({})),
    );
    if !continuity.runtime_diagnostics.is_empty() {
        object.insert(
            "next_safe_action".into(),
            serde_json::json!(
                "inspect the reported runtime artifact; do not replace a live emulator until ownership is proven"
            ),
        );
    }
    let candidates = link.runtime_candidates();
    if !candidates.is_empty() {
        object.insert(
            "runtime_candidates".into(),
            serde_json::Value::Array(candidates),
        );
        object.insert(
            "next_safe_action".into(),
            serde_json::json!("select an explicit runtime candidate; automatic attach refused"),
        );
    }
    let refreshed_current = link.endpoint_port().and_then(|port| {
        emucap::live::runtime::RuntimeStore::discover()
            .read_current(port)
            .ok()
            .flatten()
    });
    enrich_runtime_instance(
        object,
        &continuity,
        refreshed_current.map(|current| current.public_value_with_lease(&continuity.lease)),
    );
}

fn enrich_runtime_instance(
    object: &mut serde_json::Map<String, serde_json::Value>,
    continuity: &emucap::live::continuity::ContinuitySnapshot,
    current: Option<serde_json::Value>,
) {
    if let Some(current) = current {
        if matches!(
            continuity.runtime_binding.state,
            emucap::live::continuity::RuntimeBindingState::Mismatched
                | emucap::live::continuity::RuntimeBindingState::Unmanaged
        ) {
            object.remove("runtime_instance");
            object.insert("stale_runtime_instance".into(), current);
            object.insert(
                "next_safe_action".into(),
                serde_json::json!(
                    "use the live emulator identity for observation; do not treat the stale capsule as ownership evidence or edit runtime files"
                ),
            );
        } else {
            object.remove("stale_runtime_instance");
            object.insert("runtime_instance".into(), current);
        }
    } else if let Some(runtime) = object
        .get_mut("runtime_instance")
        .and_then(serde_json::Value::as_object_mut)
    {
        runtime.insert(
            "lease".into(),
            serde_json::to_value(&continuity.lease)
                .unwrap_or_else(|_| serde_json::json!({"state": "unknown"})),
        );
    }
}

pub(crate) fn enrich_link_status(
    v: &mut serde_json::Value,
    port: Option<u16>,
    session_token: Option<&str>,
    identity: Option<&EmulatorIdentity>,
) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    // 이 MCP 바이너리가 빌드된 emucap git hash(build.rs 임베드). 운영자가 `git rev-parse --short HEAD`와
    // 대조해 실행 중 서버가 최신인지 확인한다 — 재빌드 안 하면 옛 hash 그대로라 stale이 드러난다.
    obj.insert("server_build".into(), serde_json::json!(BUILD_HASH));
    obj.insert(
        "identity_guard".into(),
        serde_json::json!({
            "mode": "session_token",
            "protected": session_token.is_some(),
            "session_token_present": session_token.is_some(),
            "session_token_file": token_file_status(port),
            "mismatch_policy": "hard_fail_on_handshake",
            "launcher_contract": "Use the MCP launch tool with status.listening_port; legacy adapters/* launchers remain fallback paths and read the token file automatically.",
        }),
    );
    obj.insert("runtime_paths".into(), runtime_paths(port));
    if let Some(port) = port {
        if let Ok(Some(current)) =
            emucap::live::runtime::RuntimeStore::discover().read_current(port)
        {
            obj.insert("runtime_instance".into(), current.public_value());
        }
    }
    if let Some(identity) = identity {
        // 실행 중 에뮬레이터(어댑터)가 빌드/로드된 emucap git hash — server_build와 대칭. 운영자가
        // `git rev-parse --short HEAD`와 대조해 재빌드 필요 여부를 확인한다(server_build·emulator_build 둘 다).
        if let Some(build) = identity.build.as_deref() {
            obj.insert("emulator_build".into(), serde_json::json!(build));
        }
        obj.insert("emulator_identity".into(), redact_identity(identity));
        // 소유 인스턴스 정리 정보: 이 포트의 pidfile에서 이 세션이 띄운 프로세스 PID를 재발견해준다. agent가
        // launch 응답을 지나쳐도(다음 턴 등) 여기 pids만 kill하면 되므로, 자기 것을 못 찾아 broad pkill로
        // 도망쳐 타 세션 에뮬레이터를 죽이는 사고를 막는다.
        if let (Some(p), Some(emu_dir)) = (
            port,
            identity.system.as_deref().and_then(emu_dir_for_system),
        ) {
            obj.insert("owned_instance".into(), owned_instance_json(emu_dir, p));
        }
    }
}

/// `status.emulator_identity.system` → 런처가 pidfile을 쓰는 emu 홈 디렉터리 이름(pidfile이 사는 곳).
/// 아직 런처가 per-port pidfile을 남기는 어댑터만 매핑한다(그 외는 None → owned_instance 생략).
fn emu_dir_for_system(system: &str) -> Option<&'static str> {
    match system {
        "nds" => Some("desmume-nds"),
        "psp" => Some("ppsspp"),
        "ps2" => Some("pcsx2"),
        _ => None,
    }
}

/// 이 포트 RUN_DIR의 `*.pid`를 읽어 소유 인스턴스 PID + 정리 규칙을 반환한다. best-effort(디렉터리/파일이
/// 없으면 빈 pids). PID는 launch 시 기록된 값이라 프로세스가 이미 죽었을 수 있으니 kill 전 확인은 agent 몫.
fn owned_instance_json(emu_dir: &str, port: u16) -> serde_json::Value {
    let run_dir = emucap::launch::emu_home_dir(emu_dir, port);
    let mut pids = Vec::new();
    let mut pidfiles = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&run_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("pid") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(pid) = text.trim().parse::<u32>() {
                pids.push(pid);
                pidfiles.push(path.display().to_string());
            }
        }
    }
    pids.sort_unstable();
    serde_json::json!({
        "run_dir": run_dir.display().to_string(),
        "pids": pids,
        "pidfiles": pidfiles,
        "cleanup": "To stop this instance, terminate only the PIDs listed here and recorded in the per-port pidfiles. On Unix use `kill <pid>`; on Windows use `taskkill /PID <pid> /F`. Never use name- or path-based broad termination such as pkill, killall, or taskkill /IM because another session may use the same binary.",
    })
}
