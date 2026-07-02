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
            "notes": "Prefer canonical PCE names i/ii/run/select. a/b/start are accepted aliases."
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
            "notes": "PC-98 uses keyboard inputs through MAME ioport overrides. step(frames) is frame-based, so tap/tap_sequence can drive deterministic frozen input."
        }),
        "dc" | "dreamcast" => serde_json::json!({
            "system": "dreamcast",
            "buttons": ["a", "b", "c", "x", "y", "z", "d", "start", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start"},
            "notes": "Dreamcast pad buttons are lowercase: a/b/x/y/start + up/down/left/right are standard; c/z/d exist on some pads. Analog triggers/stick are not injectable by name. Input is injected at the maple GetInput consumer (single controller; port is ignored)."
        }),
        "snes" | "sfc" => serde_json::json!({
            "system": "snes",
            "buttons": ["a", "b", "x", "y", "l", "r", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r", "lb": "l", "rb": "r"},
            "notes": "Mesen SNES uses lowercase SNES button names."
        }),
        // 알 수 없는 system은 어느 패드로도 위장하지 않는다 — 거짓 버튼 힌트 대신 input_buttons를 생략한다.
        _ => return None,
    })
}

/// get_rom_info 응답에 균일 `rom_sha1` 필드를 삽입한다 — canonical 콘텐츠 해시(content_md5 우선,
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
    // 능력 발견 표면(계약 #2): Agent가 실제 부르는 status에 어댑터 광고 메서드 + 그 위에 의존이 충족된
    // MCP 컴포지트(tap 등 — 어댑터 메서드가 아니라 set_input+step 조합)를 합쳐, 호출 가능한 전 도구를
    // 한 곳에서 보이게 한다. 어댑터 강등으로 의존이 빠지면 컴포지트도 자동 사라진다.
    if !obj.contains_key("methods") && !methods.is_empty() {
        let has = |m: &str| methods.iter().any(|x| x == m);
        let mut full: Vec<String> = methods.to_vec();
        let push = |f: &mut Vec<String>, c: &str| {
            if !f.iter().any(|x| x == c) {
                f.push(c.into());
            }
        };
        if has("set_input") && has("step") && has("pause") {
            push(&mut full, "tap");
            push(&mut full, "tap_sequence");
            if has("read_memory") {
                push(&mut full, "hold_until");
            }
        }
        if has("probe") {
            push(&mut full, "bisect"); // bisect는 probe 전용(load_state 폴백 없음)
        }
        if has("probe") || has("load_state") {
            push(&mut full, "regression_run"); // 케이스 재생 — InputReplay는 reset|load_state로도 동작
            push(&mut full, "verify_determinism");
        }
        obj.insert("methods".into(), serde_json::json!(full));
    }
    if !obj.contains_key("memory_types") && !memory_types.is_empty() {
        obj.insert("memory_types".into(), serde_json::json!(memory_types));
    }
    // capability_notes: 어댑터가 직접 제공하면(PC-98은 dict) 그게 정본이라 *보존*한다. 제공이 없거나
    // 배열이면, 메서드 부재에서 *신뢰 가능한* substitute만 도출해 덧붙인다(정적 capability 맵 아님 —
    // capability는 methods가 정본). 어댑터가 직접 advertise하는 능력(step_instructions 등 — Mednafen·PC-98은
    // 메서드로, Mesen은 step+unit)은 status.methods에 그대로 뜨므로 도출하지 않는다 — 여기선 *메서드로
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
                // step 입자는 플랫폼별: Mednafen은 frozen step_instructions(명령단위), Flycast는 step(frames)뿐.
                let step_kind = if has("step_instructions") {
                    "frozen step_instructions(명령단위)"
                } else {
                    "frozen step(frames)"
                };
                notes.push(format!("set_trace/get_trace·call_stack·watch_register 없음 — exec BP를 호출자로 한 홉씩 옮겨 콜체인 역추적 + {step_kind} + disassemble로 부분 대체(간접점프·자기수정·점프테이블 동적복구는 정적 disasm 병행)"));
            }
            if !notes.is_empty() {
                obj.insert("capability_notes".into(), serde_json::json!(notes));
            }
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
            "powershell -ExecutionPolicy Bypass -File {} <ROM.sfc> {port} [name]",
            powershell_quote(&launcher)
        )
    } else {
        format!("{} <ROM.sfc> {port} [name]", launcher.display())
    }
}

fn native_legacy_script(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    if cfg!(windows) {
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
        "launcher": launcher.display().to_string(),
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
    let Some(root) = find_repo_root() else {
        return serde_json::json!({
            "repo_root": null,
            "repo_root_env": "EMUCAP_REPO_ROOT",
            "error": "emucap repo root not found from EMUCAP_REPO_ROOT, current_exe, cwd, or CARGO_MANIFEST_DIR",
        });
    };
    let token_file = port.map(tcp::session_token_path);
    let mesen_launcher = mesen_platform_launcher(&root);
    let mednafen_launcher = repo_path(&root, &["adapters", "mednafen", "launch.sh"]);
    let mame_launcher = repo_path(&root, &["adapters", "mame-pc98", "launch.sh"]);
    let flycast_launcher = repo_path(&root, &["adapters", "flycast", "launch.sh"]);
    serde_json::json!({
        "repo_root": root.display().to_string(),
        "repo_root_env": "EMUCAP_REPO_ROOT",
        "token_file": token_file.map(|p| p.display().to_string()),
        "adapters": {
            "mesen2": {
                "preferred_launcher": "MCP tool: launch",
                "launch": abs_path_json(&root, &["adapters", "mesen2", "launch.sh"]),
                "windows_script": abs_path_json(&root, &["adapters", "mesen2", "launch.ps1"]),
                "platform_launch": mesen_launcher.display().to_string(),
                "lua": abs_path_json(&root, &["adapters", "mesen2", "emucap-live.lua"]),
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
            }
        },
        "command_templates": port.map(|p| serde_json::json!({
            "preferred": "launch(content_path, system?, name?)",
            "legacy_mesen2": legacy_command_template(&mesen_launcher, legacy_mesen_command(&root, p)),
            "legacy_mednafen": legacy_command_template(&mednafen_launcher, format!("{} <disc_or_rom> {p} [name] [force_module]", mednafen_launcher.display())),
            "legacy_mame_pc98": legacy_command_template(&mame_launcher, format!("{} <disk.hdi|disk.hdm|disk.d88> {p} [name] [machine]", mame_launcher.display())),
            "legacy_flycast": legacy_command_template(&flycast_launcher, format!("{} <disc.gdi|disc.cdi|disc.chd|disc.cue> {p}", flycast_launcher.display())),
        })),
        "legacy_fallbacks": port.map(|p| serde_json::json!({
            "mesen2": legacy_fallback_entry(&mesen_launcher, legacy_mesen_command(&root, p)),
            "mednafen": legacy_fallback_entry(&mednafen_launcher, format!("{} <disc_or_rom> {p} [name] [force_module]", mednafen_launcher.display())),
            "mame_pc98": legacy_fallback_entry(&mame_launcher, format!("{} <disk.hdi|disk.hdm|disk.d88> {p} [name] [machine]", mame_launcher.display())),
            "flycast": legacy_fallback_entry(&flycast_launcher, format!("{} <disc.gdi|disc.cdi|disc.chd|disc.cue> {p}", flycast_launcher.display())),
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
        }
    ])
}

pub(crate) fn make_bootstrap_value(
    link: &mut dyn EmulatorLink,
) -> Result<serde_json::Value, LinkError> {
    let status = tools::status(link);
    let port = link.endpoint_port();
    let token = link.session_token().map(str::to_string);
    let identity = link.capabilities().identity.clone();

    let mut status_value = match status {
        Ok(ToolOutput::Json(mut v)) => {
            let methods = link.capabilities().methods.clone();
            let memory_types = link.capabilities().memory_types.clone();
            enrich_status_value(&mut v, &methods, &memory_types, identity.system.as_deref());
            enrich_link_status(&mut v, port, token.as_deref(), Some(&identity));
            v
        }
        Ok(_) => serde_json::json!({"connected": true}),
        Err(LinkError::NotConnected) => {
            let mut v = serde_json::json!({
                "connected": false,
                "listening_port": port,
            });
            enrich_link_status(&mut v, port, token.as_deref(), None);
            v
        }
        Err(LinkError::IdentityMismatch { identity, .. }) => {
            occupied_graceful(&identity, port, token.as_deref())
        }
        Err(e) => return Err(e),
    };

    if let Some(obj) = status_value.as_object_mut() {
        obj.entry("listening_port")
            .or_insert_with(|| port.map_or(serde_json::Value::Null, |p| serde_json::json!(p)));
    }

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
        "required_user_input_if_content_unknown": "실행할 content_path와 시스템(snes/saturn/psx/pce/md/pc98/dc)을 물어본 뒤 launch_plan을 호출하라",
        "question_to_user_if_content_unknown": "어떤 ROM/disc/disk 경로를 어떤 시스템(snes/saturn/psx/pce/md/pc98/dc)으로 실행할까요?",
        "workflow": {
            "unknown_content": {
                "ask_user": "어떤 ROM/disc/disk 경로를 어떤 시스템(snes/saturn/psx/pce/md/pc98/dc)으로 실행할까요?",
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
    if let Some(identity) = identity {
        // 실행 중 에뮬레이터(어댑터)가 빌드/로드된 emucap git hash — server_build와 대칭. 운영자가
        // `git rev-parse --short HEAD`와 대조해 재빌드 필요 여부를 확인한다(server_build·emulator_build 둘 다).
        if let Some(build) = identity.build.as_deref() {
            obj.insert("emulator_build".into(), serde_json::json!(build));
        }
        obj.insert("emulator_identity".into(), redact_identity(identity));
    }
}
