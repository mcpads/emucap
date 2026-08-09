use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use emucap::live::link::{EmulatorIdentity, EmulatorLink, LinkError};
use emucap::live::task_entry::{
    classify_entry, observe_runtime, EntryDisposition, EntryReason, EntryState, ListenerState,
    RuntimeObservation,
};
use emucap::live::tcp;
use emucap::live::tools::{self, ToolOutput};
use sha2::{Digest, Sha256};

use crate::launch::occupied_graceful;

/// 이 MCP 바이너리가 빌드된 emucap git hash(build.rs가 OUT_DIR에 기록; include_str!로 cargo가 파일 의존성을
/// 추적해 hash 변경 시 이 파일이 재컴파일된다). status.server_build로 노출. `\n` 없이 정확히 hash 문자열.
pub(crate) const BUILD_HASH: &str = emucap::build_identity::BUILD_HASH;

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;

#[path = "status/button_hints.rs"]
mod button_hints;
pub(crate) use button_hints::button_hint_for_system;

#[path = "status/continuity.rs"]
mod continuity;
pub(crate) use continuity::enrich_continuity;
#[cfg(test)]
use continuity::{enrich_runtime_instance, recording_capture_projection};

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

pub(crate) fn enrich_memory_regions(
    value: &mut serde_json::Value,
    memory_regions: &[emucap::live::link::MemoryRegion],
) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if !object
        .get("connected")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
        || object.contains_key("memory_regions")
        || memory_regions.is_empty()
    {
        return;
    }
    object.insert("memory_regions".into(), serde_json::json!(memory_regions));
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

pub(crate) fn enrich_recording_capability(
    value: &mut serde_json::Value,
    capability: Option<&emucap::live::recording_capability::RecordingCapability>,
) {
    if let Some(capability) = capability {
        value["recording_capability"] = serde_json::to_value(capability)
            .unwrap_or_else(|_| serde_json::json!({"available": false}));
    }
}

fn public_method_names(methods: &[String]) -> Vec<String> {
    let mut normalized = Vec::with_capacity(methods.len());
    let push_unique = |normalized: &mut Vec<String>, method: &str| {
        if !normalized.iter().any(|known| known == method) {
            normalized.push(method.to_string());
        }
    };
    for method in methods {
        match method.as_str() {
            "run_frames" => {}
            "press_buttons" => push_unique(&mut normalized, "pulse_while_running"),
            "touch" => {
                for public_name in ["hold_touch", "release_touch", "pulse_touch_while_running"] {
                    push_unique(&mut normalized, public_name);
                }
            }
            "step" | "step_instructions" => push_unique(&mut normalized, "step"),
            method => push_unique(&mut normalized, method),
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
    if methods.iter().any(|method| method == "step") {
        contracts.constraints.insert(
            "execution.step.max_count".into(),
            serde_json::json!(step_limit),
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
    serde_json::json!({
        "repo_root": root.display().to_string(),
        "repo_root_env": "EMUCAP_REPO_ROOT",
        "token_file": token_file.map(|p| p.display().to_string()),
        "runtime_capsule": capsule_paths,
        "adapters": {
            "mesen2": {
                "build": abs_path_json(&root, &["adapters", "mesen2", if cfg!(windows) { "build.ps1" } else { "build.sh" }]),
            },
            "mednafen": {
                "build": abs_path_json(&root, &["adapters", "mednafen", "build.sh"]),
            },
            "mame_pc98": {
                "build": abs_path_json(&root, &["adapters", "mame-pc98", "build.sh"]),
            },
            "mame_neogeo": {
                "build": abs_path_json(&root, &["adapters", "mame-neogeo", "build.sh"]),
                "bridge_binary": abs_path_json(&root, &["target", "release", if cfg!(windows) { "emucap-mame-neogeo-bridge.exe" } else { "emucap-mame-neogeo-bridge" }]),
            },
            "mupen64plus": {
                "build": abs_path_json(&root, &["adapters", "mupen64plus", "build.sh"]),
                "adapter_binary": abs_path_json(&root, &["target", "release", if cfg!(windows) { "emucap-mupen64plus.exe" } else { "emucap-mupen64plus" }]),
            },
            "openmsx": {
                "build": abs_path_json(&root, &["adapters", "openmsx", "build.sh"]),
                "bridge_binary": abs_path_json(&root, &["target", "release", if cfg!(windows) { "emucap-openmsx-bridge.exe" } else { "emucap-openmsx-bridge" }]),
            },
            "flycast": {
                "build": abs_path_json(&root, &["adapters", "flycast", "build.sh"]),
            },
            "desmume_nds": {
                "build": abs_path_json(&root, &["adapters", "desmume-nds", "build.sh"]),
            },
            "ppsspp": {
                "build": abs_path_json(&root, &["adapters", "ppsspp", "build.sh"]),
            },
            "pcsx2": {
                "build": abs_path_json(&root, &["adapters", "pcsx2", "build.sh"]),
                "bios_env": "EMUCAP_PCSX2_BIOS",
            },
            "dolphin": {
                "build": abs_path_json(&root, &["adapters", "dolphin", if cfg!(windows) { "build.ps1" } else { "build.sh" }]),
            }
        }
    })
}

fn build_supported_systems_value() -> serde_json::Value {
    serde_json::json!([
        {
            "system": "snes",
            "adapter": "mesen2",
            "content": ["sfc", "smc"],
        },
        {
            "system": "gamegear",
            "aliases": ["gg", "game-gear", "sms", "master-system"],
            "adapter": "mesen2",
            "content": ["gg", "sms"],
        },
        {
            "system": "gb",
            "aliases": ["gameboy", "game-boy", "dmg"],
            "adapter": "mesen2",
            "content": ["gb"],
        },
        {
            "system": "gbc",
            "aliases": ["gbcolor", "gameboycolor", "game-boy-color", "cgb"],
            "adapter": "mesen2",
            "content": ["gbc"],
            "notes": "GB and GBC share the emucap-gb.lua entry (Mesen gameboy console / SM83 CPU)."
        },
        {
            "system": "gba",
            "aliases": ["gameboyadvance", "game-boy-advance", "agb"],
            "adapter": "mesen2",
            "content": ["gba"],
            "notes": "ARM7: disassemble/call_stack are unsupported; memory/state/BP/save/input/screenshot are supported."
        },
        {
            "system": "nes",
            "aliases": ["famicom", "fc", "nintendo"],
            "adapter": "mesen2",
            "content": ["nes"],
            "notes": "6502/2A03: disassemble/call_stack/break_on_reset supported; memory/state/BP/save/input/screenshot supported."
        },
        {
            "system": "n64",
            "aliases": ["nintendo64", "nintendo-64"],
            "adapter": "mupen64plus",
            "content": ["z64", "n64", "v64"],
            "notes": "The Unix adapter uses the pinned Mupen64Plus pure interpreter. Both modes expose pause/resume, reset, R4300 instruction step and state, bounded frozen RDRAM access, port-0 persistent input with explicit release, R4300 exec/read/write breakpoints, event polling, and disassembly. Visible launch also exposes exact rendered-frame step, bounded input pulse, current PNG capture, and completion-checked native save/load. Headless launch omits rendered-frame operations. RSP state is not exposed."
        },
        {
            "system": "msx",
            "aliases": ["msx2+", "msx2plus", "openmsx"],
            "adapter": "openmsx",
            "content": ["rom", "mx1", "mx2", "ri", "sg"],
            "notes": "Pinned openMSX 21.0 C-BIOS MSX2+ cartridge profile. It exposes Z80 exec/read/write breakpoints with atomic evidence and disassembly in addition to frame/instruction control, memory, state, input, and visible frozen screenshots. Generic .rom files require system=msx."
        },
        {
            "system": "msx1",
            "aliases": [],
            "adapter": "openmsx",
            "content": ["rom", "mx1", "mx2", "ri", "sg", "cas", "tsx", "wav"],
            "notes": "Pinned Philips VG 8020 real-firmware profile. Set EMUCAP_OPENMSX_FIRMWARE to an absolute firmware directory. Cartridge and cassette media are admitted; disk is not."
        },
        {
            "system": "msx2",
            "aliases": [],
            "adapter": "openmsx",
            "content": ["rom", "mx1", "mx2", "ri", "sg", "dsk", "cas", "tsx", "wav"],
            "notes": "Pinned Philips NMS 8250 real-firmware profile. Set EMUCAP_OPENMSX_FIRMWARE to an absolute firmware directory. Mutable media is mounted from an isolated working copy."
        },
        {
            "system": "msx2p",
            "aliases": [],
            "adapter": "openmsx",
            "content": ["rom", "mx1", "mx2", "ri", "sg", "dsk", "cas", "tsx", "wav"],
            "notes": "Pinned Panasonic FS-A1WSX real-firmware profile. It is not the legacy msx2+ alias. Set EMUCAP_OPENMSX_FIRMWARE to an absolute firmware directory."
        },
        {
            "system": "saturn",
            "aliases": ["ss"],
            "adapter": "mednafen",
            "content": ["cue", "chd"],
        },
        {
            "system": "psx",
            "aliases": ["ps1", "playstation"],
            "adapter": "mednafen",
            "content": ["cue", "bin", "chd", "iso"],
        },
        {
            "system": "pce",
            "aliases": ["pcengine", "pc-engine", "pce-cd"],
            "adapter": "mednafen",
            "content": ["cue", "pce", "chd"],
            "force_module": "pce"
        },
        {
            "system": "pcfx",
            "aliases": ["pc-fx"],
            "adapter": "mednafen",
            "content": ["cue", "ccd", "toc", "m3u"],
            "force_module": "pcfx",
            "required_firmware": ["pcfx.rom"],
            "notes": "Disc formats are ambiguous; pass system=pcfx explicitly. V810 call_stack is a best-effort trace-derived shadow stack."
        },
        {
            "system": "md",
            "aliases": ["genesis", "megadrive", "mega-drive"],
            "adapter": "mednafen",
            "content": ["md", "gen", "smd", "bin"],
            "force_module": "md",
            "notes": ".bin is only inferred as MD when a Mega Drive/Genesis header is present; otherwise pass system=md explicitly"
        },
        {
            "system": "wswan",
            "aliases": ["ws", "wsc", "wonderswan", "wonderswan-color", "wonderswancolor", "wonderswan_color"],
            "adapter": "mednafen",
            "content": ["ws", "wsc", "wsr"],
            "force_module": "wswan",
            "notes": "WonderSwan and WonderSwan Color share the Mednafen wswan module."
        },
        {
            "system": "ngp",
            "aliases": ["ngpc", "neo-geo-pocket", "neo-geo-pocket-color", "neogeo-pocket", "neogeo-pocket-color"],
            "adapter": "mednafen",
            "content": ["ngp", "ngpc", "ngc", "npc"],
            "force_module": "ngp",
            "notes": "Neo Geo Pocket and Pocket Color share the patched Mednafen ngp module. Its TLCS-900/H profile exposes side-effect-free RAM/ROM/BIOS views, RAM writes, exact instruction step, safe disassembly, and exec-only breakpoints. Sound-Z80 state, read/write breakpoints, trace, and call-stack classification are not exposed."
        },
        {
            "system": "pc98",
            "aliases": ["pc-98", "mame-pc98"],
            "adapter": "mame_pc98",
            "content": ["hdi", "hdm", "d88"],
        },
        {
            "system": "neogeo_mvs",
            "aliases": ["neo-geo-mvs", "neogeo-mvs", "mvs"],
            "adapter": "mame_neogeo",
            "content": ["zip"],
            "required_firmware": ["neogeo.zip"],
            "notes": ".zip is not auto-inferred; pass system=neogeo_mvs explicitly. AES, CD, Pocket/Color, and Hyper Neo Geo 64 are separate targets and are not accepted as aliases."
        },
        {
            "system": "neogeo_aes",
            "aliases": ["neo-geo-aes", "neogeo-aes", "aes"],
            "adapter": "mame_neogeo",
            "content": ["zip"],
            "required_firmware": ["aes.zip"],
            "notes": ".zip is not auto-inferred; pass system=neogeo_aes explicitly. The ZIP stem must name an AES-compatible entry in the pinned MAME Neo Geo software list."
        },
        {
            "system": "neogeo_cd",
            "aliases": ["neo-geo-cd", "neogeo-cd", "ngcd"],
            "adapter": "mame_neogeo",
            "content": ["cue"],
            "required_firmware": ["neocdz.zip"],
            "notes": "CUE is ambiguous; pass system=neogeo_cd explicitly. The content identity covers the CUE and every referenced file. Native save/load is not advertised."
        },
        {
            "system": "dc",
            "aliases": ["dreamcast", "flycast"],
            "adapter": "flycast",
            "content": ["gdi", "cdi", "chd", "cue"],
        },
        {
            "system": "nds",
            "aliases": ["ds", "nintendo-ds", "desmume"],
            "adapter": "desmume_nds",
            "content": ["nds"],
        },
        {
            "system": "psp",
            "aliases": ["ppsspp", "playstation-portable"],
            "adapter": "ppsspp",
            "content": ["iso", "cso", "pbp"],
            "notes": ".iso is shared with Saturn/PSX/PCE/MD/Dreamcast — a PSP GAME ISO9660 header disambiguates automatically; otherwise pass system=psp explicitly."
        },
        {
            "system": "ps2",
            "aliases": ["pcsx2", "playstation2", "playstation-2"],
            "adapter": "pcsx2",
            "content": ["iso"],
            "required_environment": ["EMUCAP_PCSX2_BIOS"],
            "notes": "An ISO9660 SYSTEM.CNF BOOT2 entry is inferred automatically. The pinned PCSX2 fork and Rust bridge are required."
        },
        {
            "system": "gamecube",
            "aliases": ["gc", "ngc", "game-cube"],
            "adapter": "dolphin",
            "content": ["gcm", "iso", "rvz", "gcz"],
            "notes": ".gcm and the GameCube disc magic are inferred automatically; shared container extensions require system=gamecube."
        },
        {
            "system": "wii",
            "aliases": ["nintendo-wii"],
            "adapter": "dolphin",
            "content": ["wbfs", "iso", "rvz", "wia", "gcz"],
            "notes": ".wbfs and the Wii disc magic are inferred automatically; shared container extensions require system=wii."
        }
    ])
}

fn supported_systems_catalog() -> &'static serde_json::Value {
    static CATALOG: OnceLock<serde_json::Value> = OnceLock::new();
    CATALOG.get_or_init(build_supported_systems_value)
}

pub(crate) fn supported_systems_value() -> serde_json::Value {
    supported_systems_catalog().clone()
}

pub(crate) fn supported_system_ids_value() -> serde_json::Value {
    static IDS: OnceLock<serde_json::Value> = OnceLock::new();
    IDS.get_or_init(|| {
        serde_json::Value::Array(
            supported_systems_catalog()
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|system| system["system"].as_str())
                .map(|system| serde_json::json!(system))
                .collect(),
        )
    })
    .clone()
}

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json(&values[key]));
            }
            serde_json::Value::Object(canonical)
        }
        scalar => scalar.clone(),
    }
}

fn json_revision(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(&canonical_json(value))
        .expect("serializing a serde_json::Value cannot fail");
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

pub(crate) fn system_catalog_revision() -> String {
    static REVISION: OnceLock<String> = OnceLock::new();
    REVISION
        .get_or_init(|| json_revision(supported_systems_catalog()))
        .clone()
}

pub(crate) fn unknown_content_question() -> &'static str {
    "Which ROM, disc, or disk path should be used?"
}

const CAPABILITY_FIELDS: &[&str] = &[
    "methods",
    "memory_types",
    "memory_regions",
    "media_devices",
    "breakpoint_kinds",
    "input_buttons",
    "contracts",
    "capability_notes",
    "execution_limits",
    "freeze_policy",
    "bank_tagging",
    "recording_capability",
];

fn capability_revision(value: &serde_json::Value) -> String {
    let mut snapshot = serde_json::Map::new();
    snapshot.insert("schema".into(), serde_json::json!(1));
    for field in CAPABILITY_FIELDS {
        if let Some(field_value) = value.get(field) {
            snapshot.insert((*field).into(), field_value.clone());
        }
    }
    for field in [
        "server_build",
        "emulator_build",
        "emulator_identity",
        "protocol_version",
    ] {
        if let Some(field_value) = value.get(field) {
            snapshot.insert(field.into(), field_value.clone());
        }
    }
    json_revision(&serde_json::Value::Object(snapshot))
}

fn remove_capability_fields(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for field in CAPABILITY_FIELDS {
        object.remove(*field);
    }
}

pub(crate) fn apply_capability_revision(
    value: &mut serde_json::Value,
    known_revision: Option<&str>,
) -> String {
    let revision = capability_revision(value);
    let unchanged = known_revision == Some(revision.as_str());
    if unchanged {
        remove_capability_fields(value);
    }
    value["capability_revision"] = serde_json::json!(revision);
    value["capability_snapshot"] = serde_json::json!(if unchanged { "unchanged" } else { "full" });
    revision
}

pub(crate) fn enrich_connected_status(value: &mut serde_json::Value, link: &dyn EmulatorLink) {
    let base_port = link.base_port();
    let port = link.endpoint_port();
    let token = link.session_token().map(str::to_string);
    let identity = link.capabilities().identity.clone();
    let methods = link.capabilities().methods.clone();
    let memory_types = link.capabilities().memory_types.clone();
    let memory_regions = link.capabilities().memory_regions.clone();
    let breakpoint_kinds = link.capabilities().breakpoint_kinds.clone();
    let contracts = link.capabilities().contracts.clone();
    let recording = (port.is_some()
        && link.continuity().runtime_binding.state
            == emucap::live::continuity::RuntimeBindingState::Bound)
        .then(|| link.capabilities().recording.as_ref())
        .flatten();
    enrich_status_value(value, &methods, &memory_types, identity.system.as_deref());
    enrich_memory_regions(value, &memory_regions);
    enrich_breakpoint_kinds(value, &breakpoint_kinds);
    enrich_contract_status(value, &identity, &contracts);
    enrich_recording_capability(value, recording);
    enrich_link_status(value, port, token.as_deref(), Some(&identity));
    enrich_listener_ports(value, base_port, port);
    enrich_continuity(value, link);
    if let Some(object) = value.as_object_mut() {
        object
            .entry("protocol_version")
            .or_insert_with(|| serde_json::json!(link.capabilities().protocol_version));
    }
    value["request_succeeded"] = serde_json::json!(true);
}

pub(crate) struct ControlObservation {
    pub(crate) status: serde_json::Value,
    pub(crate) runtime: RuntimeObservation,
    pub(crate) disposition: EntryDisposition,
}

/// Reconcile an unfinished host-owned recording only after the live adapter identity proves that
/// it is the exact current direct-mode generation. This is intentionally part of status recovery:
/// after a Control MCP restart, agents should not need to launch or mutate the emulator merely to
/// close a capture whose adapter-side terminal state is already observable.
pub(crate) fn reconcile_connected_recording(
    link: &mut dyn EmulatorLink,
) -> Result<bool, emucap::live::recording::RecordingError> {
    reconcile_connected_recording_with_store(link, emucap::live::runtime::RuntimeStore::discover())
}

fn reconcile_connected_recording_with_store(
    link: &mut dyn EmulatorLink,
    store: emucap::live::runtime::RuntimeStore,
) -> Result<bool, emucap::live::recording::RecordingError> {
    let Some(port) = link.endpoint_port() else {
        return Ok(false);
    };
    let current = store
        .read_current(port)
        .map_err(|error| {
            emucap::live::recording::RecordingError::Recovery(format!(
                "runtime capsule could not be read during status recovery: {error}"
            ))
        })?
        .filter(|current| {
            link.capabilities().identity.launch_id.as_deref() == Some(current.launch_id.as_str())
        });
    let Some(current) = current else {
        return Ok(false);
    };
    emucap::live::recording::reconcile_abandoned_capture(link, store, port, &current.launch_id)
        .map(|outcome| outcome.is_some())
}

pub(crate) fn observe_control_state(
    link: &mut dyn EmulatorLink,
) -> Result<ControlObservation, LinkError> {
    let base_port = link.base_port();
    let token = link.session_token().map(str::to_string);
    let status_result = tools::status(link);
    // status may perform the first lazy bind and move from base_port to a free listener port. Read
    // the endpoint only after that operation; a pre-call value is not an assigned port.
    let port = link.endpoint_port();
    let (mut status, listener, adapter_connected, observation_uncertain) = match status_result {
        Ok(ToolOutput::Json(mut v)) => {
            enrich_connected_status(&mut v, link);
            (v, ListenerState::Bound, true, false)
        }
        Ok(_) => {
            let mut v = serde_json::json!({"connected": true});
            enrich_continuity(&mut v, link);
            (v, ListenerState::Bound, true, false)
        }
        Err(LinkError::NotConnected) => {
            let mut v = serde_json::json!({
                "connected": false,
                "listening_port": port,
                "request_succeeded": false,
            });
            enrich_link_status(&mut v, port, token.as_deref(), None);
            enrich_continuity(&mut v, link);
            let listener = if port.is_some() {
                ListenerState::Bound
            } else {
                ListenerState::Unavailable
            };
            (v, listener, false, false)
        }
        Err(LinkError::IdentityMismatch { identity, .. }) => {
            let mut v = occupied_graceful(&identity, port, token.as_deref());
            enrich_continuity(&mut v, link);
            (v, ListenerState::Blocked, false, false)
        }
        Err(e) if is_observation_failure(&e) => {
            let observation_uncertain = matches!(e, LinkError::Timeout | LinkError::Protocol(_));
            let mut v = serde_json::json!({
                "connected": false,
                "request_succeeded": false,
                "error_kind": e.kind(),
                "error": e.to_string(),
                "listening_port": port,
            });
            enrich_link_status(&mut v, port, token.as_deref(), None);
            enrich_continuity(&mut v, link);
            let listener = if matches!(e, LinkError::PortBusy { .. }) {
                ListenerState::Blocked
            } else if port.is_some() {
                ListenerState::Bound
            } else {
                ListenerState::Unavailable
            };
            (v, listener, false, observation_uncertain)
        }
        Err(e) => return Err(e),
    };
    enrich_listener_ports(&mut status, base_port, port);
    let mut runtime = observe_runtime(link, listener, adapter_connected);
    if observation_uncertain {
        runtime.control_observation_uncertain = true;
        runtime.transport = emucap::live::continuity::TransportState::Stalled;
    }
    let disposition = classify_entry(&runtime);
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "task_entry".into(),
            serde_json::json!({
                "state": disposition.state,
                "reason": disposition.reason,
                "accepts_new_content": disposition.accepts_new_content(),
            }),
        );
    }
    Ok(ControlObservation {
        status,
        runtime,
        disposition,
    })
}

fn primary_action(disposition: EntryDisposition) -> serde_json::Value {
    match disposition.state {
        EntryState::ReadyForContent => serde_json::json!({
            "kind": "resolve_input",
            "required_input": ["content_path"],
            "question_if_missing": unknown_content_question(),
            "then_call": {
                "tool": "launch_plan",
                "arguments_from": ["content_path", "system?"]
            }
        }),
        EntryState::InspectFailure => serde_json::json!({
            "kind": "call_tool",
            "tool": "get_failure_context",
            "arguments": {}
        }),
        EntryState::RepairRuntimeMetadata
        | EntryState::InspectExisting
        | EntryState::ReattachExisting
        | EntryState::TransitionBlocked => serde_json::json!({
            "kind": "call_tool",
            "tool": "status",
            "arguments": {}
        }),
    }
}

pub(crate) fn make_bootstrap_value(
    link: &mut dyn EmulatorLink,
    include_systems: bool,
    include_installation: bool,
    include_runtimes: bool,
) -> Result<serde_json::Value, LinkError> {
    let observation = observe_control_state(link)?;
    let runtime_reservations = link.runtime_reservations();
    let base_port = link.base_port();
    let port = link.endpoint_port();
    let mut result = serde_json::json!({
        "listener": {
            "state": observation.runtime.listener,
            "base_port": base_port,
            "port": port,
        },
        "server_build": BUILD_HASH,
        "adapter_connection": {
            "state": observation.runtime.transport,
        },
        "entry": {
            "state": observation.disposition.state,
            "reason": observation.disposition.reason,
            "primary_action": primary_action(observation.disposition),
        },
        "supported_system_ids": supported_system_ids_value(),
        "system_catalog_revision": system_catalog_revision(),
        "runtime_reservation_count": runtime_reservations.len(),
        "optional_details": {
            "systems": "bootstrap(include=[\"systems\"])",
            "installation": "bootstrap(include=[\"installation\"])",
            "runtimes": "bootstrap(include=[\"runtimes\"])"
        }
    });
    if base_port.is_none() {
        result["listener"]
            .as_object_mut()
            .expect("listener is an object")
            .remove("base_port");
    }
    if observation.disposition.reason == EntryReason::TerminalHistory {
        result["terminal_history_available"] = serde_json::json!(true);
    }
    if observation.disposition.reason == EntryReason::ListenerBlocked
        && !runtime_reservations.is_empty()
    {
        result["entry"]["primary_action"] = serde_json::json!({
            "kind": "call_tool",
            "tool": "bootstrap",
            "arguments": {"include": ["runtimes"]},
        });
    }
    if include_systems {
        result["supported_systems"] = supported_systems_value();
    }
    if include_installation {
        result["runtime_paths"] = runtime_paths(port);
    }
    if include_runtimes {
        result["runtime_reservations"] = serde_json::Value::Array(runtime_reservations);
    }
    Ok(result)
}

fn enrich_listener_ports(
    value: &mut serde_json::Value,
    base_port: Option<u16>,
    listening_port: Option<u16>,
) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(base_port) = base_port {
        object.insert("base_port".into(), serde_json::json!(base_port));
    }
    if let Some(listening_port) = listening_port {
        object.insert("listening_port".into(), serde_json::json!(listening_port));
    } else {
        object.remove("listening_port");
    }
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
            "launcher_contract": "Use the MCP launch tool with status.listening_port. It passes the per-port session token automatically.",
        }),
    );
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
        "cleanup": "For a managed runtime, use stop(status.runtime_instance.launch_id). These pidfiles are diagnostic fallback data and do not replace launch-generation, lease, and process-start identity verification. Never use name- or path-based broad termination such as pkill, killall, or taskkill /IM.",
    })
}
