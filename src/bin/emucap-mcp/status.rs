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

pub(crate) fn button_hint_for_system(system: Option<&str>) -> Option<serde_json::Value> {
    Some(match system? {
        "ss" | "saturn" => serde_json::json!({
            "system": "saturn",
            "buttons": ["a", "b", "c", "x", "y", "z", "l", "r", "start", "up", "down", "left", "right"],
            "aliases": {"ls": "l", "rs": "r", "l1": "l", "r1": "r", "lb": "l", "rb": "r", "enter": "start", "return": "start"},
            "notes": "Saturn pad buttons are lowercase. Directions are up/down/left/right."
        }),
        "psx" | "ps1" | "playstation" => serde_json::json!({
            "system": "psx",
            "buttons": ["cross", "circle", "triangle", "square", "l1", "l2", "r1", "r2", "start", "select", "up", "down", "left", "right"],
            "aliases": {"x": "cross", "o": "circle", "l": "l1", "r": "r1", "enter": "start", "return": "start"},
            "optional": ["l3", "r3"],
            "notes": "Use PlayStation names, not SNES/Saturn a/b."
        }),
        "pce" | "pce_fast" | "pcengine" | "pc-engine" => serde_json::json!({
            "system": "pce",
            "buttons": ["i", "ii", "run", "select", "up", "down", "left", "right"],
            "aliases": {"a": "i", "b": "ii", "start": "run", "enter": "run", "return": "run"},
            "six_button": ["iii", "iv", "v", "vi"],
            "notes": "Prefer PCE button names i/ii/run/select. a/b/start are accepted aliases."
        }),
        "md" | "genesis" | "megadrive" | "mega-drive" => serde_json::json!({
            "system": "md",
            "buttons": ["a", "b", "c", "x", "y", "z", "mode", "start", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start"},
            "notes": "Mega Drive/Genesis uses Mednafen md.input.port1=gamepad6 through the launcher so x/y/z/mode are available."
        }),
        "pc98" => serde_json::json!({
            "system": "pc98",
            "buttons": ["enter", "esc", "space", "up", "down", "left", "right", "backspace", "tab", "del", "ins", "home", "help", "stop", "copy", "shift", "ctrl", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "vf1", "vf2", "vf3", "vf4", "vf5", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9"],
            "aliases": {"start": "enter", "return": "enter", "escape": "esc", "select": "space"},
            "notes": "PC-98 uses keyboard inputs through MAME ioport overrides. step(frames) is frame-based, so tap can drive deterministic frozen input."
        }),
        "dc" | "dreamcast" => serde_json::json!({
            "system": "dreamcast",
            "buttons": ["a", "b", "c", "x", "y", "z", "d", "start", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start"},
            "notes": "Dreamcast pad buttons are lowercase: a/b/x/y/start + up/down/left/right are standard; c/z/d exist on some pads. Analog triggers/stick are not injectable by name. Input is injected at the maple GetInput consumer; only controller port 0 is supported."
        }),
        "gamecube" | "gc" | "ngc" => serde_json::json!({
            "system": "gamecube",
            "buttons": ["a", "b", "x", "y", "z", "l", "r", "start", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r"},
            "notes": "GameCube controller button names are lowercase. Only controller port 0 is supported by the native adapter."
        }),
        "snes" | "sfc" => serde_json::json!({
            "system": "snes",
            "buttons": ["a", "b", "x", "y", "l", "r", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r", "lb": "l", "rb": "r"},
            "notes": "Mesen SNES uses lowercase SNES button names."
        }),
        "gamegear" | "gg" | "sms" => serde_json::json!({
            "system": "gamegear",
            "buttons": ["up", "down", "left", "right", "one", "two", "pause"],
            "aliases": {"start": "pause", "enter": "pause", "return": "pause", "a": "two", "b": "one", "1": "one", "2": "two", "button1": "one", "button2": "two"},
            "notes": "Mesen Game Gear (SMS controller): one=Button1(B), two=Button2(A), pause=Start. Aliases let you use start/a/b/1/2."
        }),
        "gb" | "gbc" | "gameboy" | "game-boy" | "dmg" | "gbcolor" | "gameboycolor" | "cgb" => {
            serde_json::json!({
                "system": "gb",
                "buttons": ["a", "b", "start", "select", "up", "down", "left", "right"],
                "aliases": {"enter": "start", "return": "start"},
                "notes": "Mesen Game Boy / Game Boy Color (gameboy console): a/b/start/select + directions, lowercase. No X/Y/L/R."
            })
        }
        "gba" | "gameboyadvance" | "game-boy-advance" | "agb" => serde_json::json!({
            "system": "gba",
            "buttons": ["a", "b", "l", "r", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r", "lb": "l", "rb": "r"},
            "notes": "Mesen Game Boy Advance (ARM7): a/b/l/r/start/select + directions, lowercase. No X/Y."
        }),
        "nes" | "famicom" | "fc" | "nintendo" => serde_json::json!({
            "system": "nes",
            "buttons": ["a", "b", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start"},
            "notes": "Mesen NES / Famicom (nes console / 6502-2A03 CPU): a/b/start/select + directions, lowercase. No X/Y/L/R."
        }),
        "nds" | "ds" | "nintendo-ds" => serde_json::json!({
            "system": "nds",
            "buttons": ["a", "b", "x", "y", "l", "r", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r"},
            "notes": "Nintendo DS buttons are injected through the DeSmuME bridge; only controller port 0 is supported. Use the dedicated touch tool for the lower screen. Microphone input is not injectable."
        }),
        // 알 수 없는 system은 어느 패드로도 위장하지 않는다 — 거짓 버튼 힌트 대신 input_buttons를 생략한다.
        _ => return None,
    })
}

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
                notes.push(format!("{} 없음 — exec BP를 호출자로 한 홉씩 옮겨 콜체인 역추적 + {step_kind} + disassemble로 부분 대체(간접점프·자기수정·점프테이블 동적복구는 정적 disasm 병행)", missing.join("·")));
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
            "system": "pc98",
            "aliases": ["pc-98", "mame-pc98"],
            "adapter": "mame_pc98",
            "content": ["hdi", "hdm", "d88"],
            "launcher": "MCP tool: launch",
            "legacy_launcher": "runtime_paths.adapters.mame_pc98.launch"
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
        "어떤 ROM/disc/disk 경로를 어떤 시스템({})으로 실행할까요?",
        supported_system_names()
    )
}

pub(crate) fn required_unknown_content_input() -> String {
    format!(
        "실행할 content_path와 시스템({})을 물어본 뒤 launch_plan(content_path, system)을 호출하라",
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
        "next_action": "content_path가 있으면 launch_plan(content_path, system?)을 호출한다. 없으면 사용자에게 question_to_user_if_content_unknown을 그대로 물어본다.",
        "do_not": "content_path/system이 없으면 runtime_paths command_template만 보고 추측 실행하지 말라"
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
    if let Some(current) = refreshed_current {
        object.insert(
            "runtime_instance".into(),
            current.public_value_with_lease(&continuity.lease),
        );
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
        "cleanup": "이 인스턴스를 멈추려면 여기 pids만 종료하라(포트별 pidfile 기록) — Unix `kill <pid>`, \
                    Windows `taskkill /PID <pid> /F`. 바이너리 이름/경로로 광역 종료(Unix `pkill -f`·`killall`· \
                    `pkill -i`, Windows `taskkill /IM`)는 절대 금지 — 같은 바이너리를 쓰는 타 세션 에뮬레이터까지 죽인다.",
    })
}
