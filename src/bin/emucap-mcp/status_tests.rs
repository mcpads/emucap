use super::*;

fn enriched(methods: &[&str]) -> serde_json::Value {
    let m: Vec<String> = methods.iter().map(|s| s.to_string()).collect();
    let mut v = serde_json::json!({"connected": true});
    enrich_status_value(&mut v, &m, &[], None);
    v
}
fn has_method(v: &serde_json::Value, name: &str) -> bool {
    v["methods"]
        .as_array()
        .map(|a| a.iter().any(|x| x == name))
        .unwrap_or(false)
}
fn notes_contain(v: &serde_json::Value, sub: &str) -> bool {
    v["capability_notes"]
        .as_array()
        .map(|a| {
            a.iter()
                .any(|x| x.as_str().map(|s| s.contains(sub)).unwrap_or(false))
        })
        .unwrap_or(false)
}

fn path_ends_with(value: &str, parts: &[&str]) -> bool {
    let mut suffix = std::path::PathBuf::new();
    for part in parts {
        suffix.push(part);
    }
    std::path::Path::new(value).ends_with(suffix)
}

#[test]
fn composites_appear_when_deps_met() {
    let v = enriched(&[
        "set_input",
        "step",
        "pause",
        "read_memory",
        "probe",
        "set_breakpoint",
        "watch_register",
        "step_instructions",
        "set_trace",
    ]);
    for c in [
        "tap",
        "tap_sequence",
        "hold_until",
        "bisect",
        "regression_run",
        "verify_determinism",
    ] {
        assert!(has_method(&v, c), "composite {c} missing");
    }
    // 의존 충족된 풀셋엔 substitute note가 없다.
    assert!(v.get("capability_notes").is_none());
}

#[test]
fn composites_absent_without_deps_and_trace_note_present() {
    // MD류: set_input/step/pause 있으나 probe 없음 → tap O, bisect X. trace 없음 → 콜체인 역추적 대체 note만.
    let v = enriched(&[
        "set_input",
        "step",
        "pause",
        "read_memory",
        "set_breakpoint",
        "screenshot",
    ]);
    assert!(has_method(&v, "tap") && has_method(&v, "hold_until"));
    assert!(!has_method(&v, "bisect"));
    assert!(notes_contain(&v, "콜체인 역추적"), "trace 대체 note 누락");
    // 토큰으로 구분 못 하는 명령단위 step·layer 노트는 도출하지 않는다(거짓 신호 방지).
    assert!(!notes_contain(&v, "명령단위 step"), "거짓 step note 도출됨");
    assert!(
        !notes_contain(&v, "레이어 토글"),
        "과발화 layer note 도출됨"
    );
    // step 없으면 tap도 없다.
    let v2 = enriched(&["read_memory", "set_breakpoint"]);
    assert!(!has_method(&v2, "tap"));
}

#[test]
fn bisect_needs_probe_not_just_load_state() {
    // Flycast류: load_state 광고·probe 미광고 → bisect 과대광고 금지, regression/verify는 OK.
    let v = enriched(&[
        "set_input",
        "step",
        "pause",
        "load_state",
        "run_frames",
        "read_memory",
    ]);
    assert!(
        !has_method(&v, "bisect"),
        "bisect는 probe 전용인데 load_state로 과대광고됨"
    );
    assert!(has_method(&v, "regression_run") && has_method(&v, "verify_determinism"));
}

#[test]
fn snes_button_hint_exposes_common_aliases() {
    let hint = button_hint_for_system(Some("snes")).unwrap();

    assert_eq!(hint["aliases"]["enter"], "start");
    assert_eq!(hint["aliases"]["return"], "start");
    assert_eq!(hint["aliases"]["l1"], "l");
    assert!(hint["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b.as_str() == Some("start")));
}

#[test]
fn mednafen_button_hints_expose_common_aliases() {
    let saturn = button_hint_for_system(Some("saturn")).unwrap();
    assert_eq!(saturn["aliases"]["enter"], "start");
    assert_eq!(saturn["aliases"]["l1"], "l");

    let psx = button_hint_for_system(Some("psx")).unwrap();
    assert_eq!(psx["aliases"]["x"], "cross");
    assert_eq!(psx["aliases"]["o"], "circle");

    let pce = button_hint_for_system(Some("pce")).unwrap();
    assert_eq!(pce["aliases"]["start"], "run");
    assert_eq!(pce["aliases"]["a"], "i");

    let md = button_hint_for_system(Some("md")).unwrap();
    assert_eq!(md["aliases"]["enter"], "start");
}

#[test]
fn dreamcast_button_hint_exposes_start_aliases() {
    let hint = button_hint_for_system(Some("dreamcast")).unwrap();

    assert_eq!(hint["aliases"]["enter"], "start");
    assert_eq!(hint["aliases"]["return"], "start");
    assert!(hint["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b.as_str() == Some("start")));
}

#[test]
fn adapter_provided_dict_capability_notes_preserved() {
    // PC-98류: 어댑터가 capability_notes를 dict로 제공 → enrich가 보존(배열로 덮어쓰지 않음).
    let m: Vec<String> = ["read_memory", "set_breakpoint"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut v = serde_json::json!({"connected": true, "capability_notes": {"backend": "gdbstub", "frame_step": true}});
    enrich_status_value(&mut v, &m, &[], None);
    assert!(
        v["capability_notes"].is_object(),
        "어댑터 dict capability_notes가 파괴됨"
    );
    assert_eq!(v["capability_notes"]["backend"], "gdbstub");
}

#[test]
fn runtime_paths_exposes_preferred_launch_tool_and_repo_fallbacks() {
    let paths = runtime_paths(Some(47803));
    let root = paths
        .get("repo_root")
        .and_then(|v| v.as_str())
        .expect("repo_root");
    assert!(
        repo_path(
            std::path::Path::new(root),
            &["adapters", "mame-pc98", "launch.sh"]
        )
        .is_file(),
        "repo_root must point at this repository"
    );
    assert_eq!(
        paths
            .pointer("/adapters/mame_pc98/preferred_launcher")
            .and_then(|v| v.as_str()),
        Some("MCP tool: launch")
    );
    let mesen_platform_launch = paths
        .pointer("/adapters/mesen2/platform_launch")
        .and_then(|v| v.as_str())
        .expect("mesen2 platform_launch");
    let mesen_template = paths
        .pointer("/command_templates/legacy_mesen2")
        .and_then(|v| v.as_str())
        .expect("mesen2 legacy template");
    if cfg!(windows) {
        assert!(path_ends_with(
            mesen_platform_launch,
            &["adapters", "mesen2", "launch.ps1"]
        ));
        assert!(mesen_template.contains("powershell -ExecutionPolicy Bypass -File"));
        assert!(mesen_template.contains("launch.ps1"));
        assert_eq!(
            paths
                .pointer("/legacy_fallbacks/mesen2/available_on_this_host")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    } else {
        assert!(path_ends_with(
            mesen_platform_launch,
            &["adapters", "mesen2", "launch.sh"]
        ));
        assert!(mesen_template.contains("mesen2/launch.sh"));
        assert_eq!(
            paths
                .pointer("/legacy_fallbacks/mesen2/available_on_this_host")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }
    assert_eq!(
        paths
            .pointer("/adapters/mame_pc98/launch")
            .and_then(|v| v.as_str()),
        Some(
            repo_path(
                std::path::Path::new(root),
                &["adapters", "mame-pc98", "launch.sh"]
            )
            .to_str()
            .unwrap()
        )
    );
    assert_eq!(
        paths
            .pointer("/adapters/mame_pc98/work_source_dir")
            .and_then(|v| v.as_str()),
        Some(
            repo_path(
                std::path::Path::new(root),
                &["adapters", "mame-pc98", "work", "mame-src"]
            )
            .to_str()
            .unwrap()
        )
    );
    assert_eq!(
        paths
            .pointer("/adapters/mame_pc98/work_wrapper")
            .and_then(|v| v.as_str()),
        Some(
            repo_path(
                std::path::Path::new(root),
                &["adapters", "mame-pc98", "work", "mame"]
            )
            .to_str()
            .unwrap()
        )
    );
    let mame_template = paths.pointer("/command_templates/legacy_mame_pc98");
    if cfg!(windows) {
        assert_eq!(mame_template, Some(&serde_json::Value::Null));
        assert_eq!(
            paths
                .pointer("/legacy_fallbacks/mame_pc98/available_on_this_host")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    } else {
        assert!(
            mame_template
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("47803")
                    && s.contains("mame-pc98")
                    && s.contains("launch.sh")),
            "legacy command template should include the current listening port and launcher"
        );
        assert_eq!(
            paths
                .pointer("/legacy_fallbacks/mame_pc98/available_on_this_host")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }
    assert_eq!(
        paths
            .pointer("/command_templates/preferred")
            .and_then(|v| v.as_str()),
        Some("launch(content_path, system?, name?)")
    );
    assert!(
        supported_systems_value()
            .as_array()
            .and_then(|systems| systems.iter().find(|system| system["system"] == "snes"))
            .and_then(|system| system["legacy_launcher"].as_str())
            == Some("runtime_paths.adapters.mesen2.platform_launch")
    );
}

struct NotConnectedLink {
    caps: emucap::live::link::Capabilities,
}

impl EmulatorLink for NotConnectedLink {
    fn capabilities(&self) -> &emucap::live::link::Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        Err(LinkError::NotConnected)
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(47855)
    }

    fn session_token(&self) -> Option<&str> {
        Some("test-token")
    }
}

#[test]
fn bootstrap_not_connected_tells_agent_to_ask_when_content_unknown() {
    let mut link = NotConnectedLink {
        caps: emucap::live::link::Capabilities {
            protocol_version: 1,
            methods: vec![],
            memory_types: vec![],
            identity: EmulatorIdentity::default(),
        },
    };
    let value = make_bootstrap_value(&mut link).unwrap();
    assert_eq!(value["first_tool"], "bootstrap");
    assert_eq!(value["listening_port"], 47855);
    assert!(value["question_to_user_if_content_unknown"]
        .as_str()
        .unwrap()
        .contains("어떤 ROM/disc/disk 경로"));
    assert_eq!(
        value
            .pointer("/workflow/unknown_content/then_call")
            .and_then(|v| v.as_str()),
        Some("launch_plan")
    );
    assert!(value["start_here"].as_bool().unwrap());
    assert!(value["do_not"]
        .as_str()
        .unwrap()
        .contains("추측 실행하지 말라"));
}

struct TimeoutLink {
    caps: emucap::live::link::Capabilities,
}

impl EmulatorLink for TimeoutLink {
    fn capabilities(&self) -> &emucap::live::link::Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        Err(LinkError::Timeout)
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(47856)
    }
}

#[test]
fn bootstrap_is_total_when_status_times_out() {
    let mut link = TimeoutLink {
        caps: emucap::live::link::Capabilities::empty(),
    };

    let value = make_bootstrap_value(&mut link).unwrap();

    assert_eq!(value["status"]["request_succeeded"], false);
    assert_eq!(value["status"]["error_kind"], "request_timeout");
    assert_eq!(
        value["status"]["continuity"]["transport"]["state"],
        "disconnected"
    );
    assert_eq!(value["listening_port"], 47856);
}

#[test]
fn enrich_status_value_adds_methods() {
    let mut v = serde_json::json!({"connected": true, "system": "snes"});
    enrich_status_value(
        &mut v,
        &["read_memory".to_string(), "set_breakpoint".to_string()],
        &[],
        None,
    );
    assert_eq!(
        v["methods"],
        serde_json::json!(["read_memory", "set_breakpoint"])
    );
    // 기존 보강(input_buttons)도 유지
    assert!(v.get("input_buttons").is_some());
}

#[test]
fn enrich_status_value_methods_reflect_downgrade() {
    // 강등 어댑터(pce_fast)는 hello에 memory/BP를 안 실음 → methods가 강등을 그대로 반영
    let mut v = serde_json::json!({"connected": true, "system": "pce"});
    enrich_status_value(
        &mut v,
        &["status".to_string(), "screenshot".to_string()],
        &[],
        None,
    );
    let methods = v["methods"].as_array().unwrap();
    assert!(!methods.iter().any(|m| m == "read_memory"));
}

#[test]
fn normalize_rom_sha1_prefers_content_md5() {
    // Mednafen: content_md5가 canonical, sha1은 보조 — content_md5를 rom_sha1로.
    let mut v = serde_json::json!({"content_md5": "abc", "sha1": "def"});
    normalize_rom_sha1(&mut v);
    assert_eq!(v["rom_sha1"], "abc");
    // 기존 필드 보존.
    assert_eq!(v["content_md5"], "abc");
    assert_eq!(v["sha1"], "def");
}

#[test]
fn normalize_rom_sha1_falls_back_to_sha1() {
    // Mesen/PC-98: content_md5 없음 → sha1로 폴백.
    let mut v = serde_json::json!({"sha1": "def"});
    normalize_rom_sha1(&mut v);
    assert_eq!(v["rom_sha1"], "def");
}

#[test]
fn normalize_rom_sha1_skips_too_large_marker() {
    // 대용량 디스크: content_md5는 유효하면 그것을 쓴다(sha1=skipped 무관).
    let mut v = serde_json::json!({"content_md5": "abc", "sha1": "skipped:too_large"});
    normalize_rom_sha1(&mut v);
    assert_eq!(v["rom_sha1"], "abc");
    // content_md5도 무효(skipped/빈값)이고 sha1만 skipped면 폴백 대상 없음 → rom_sha1 미생성.
    let mut v2 =
        serde_json::json!({"content_md5": "skipped:too_large", "sha1": "skipped:too_large"});
    normalize_rom_sha1(&mut v2);
    assert!(v2.get("rom_sha1").is_none());
}

#[test]
fn normalize_rom_sha1_absent_when_no_hash() {
    // Flycast(gameId만, 해시 미반환): rom_sha1 미생성 → 호출자 shasum 폴백.
    let mut v = serde_json::json!({"game_id": "T1234", "name": "GAME"});
    normalize_rom_sha1(&mut v);
    assert!(v.get("rom_sha1").is_none());
}

#[test]
fn normalize_rom_sha1_no_overwrite() {
    // 이미 rom_sha1이 있으면 덮어쓰지 않는다.
    let mut v = serde_json::json!({"rom_sha1": "preset", "content_md5": "abc"});
    normalize_rom_sha1(&mut v);
    assert_eq!(v["rom_sha1"], "preset");
}

#[test]
fn enrich_status_value_adds_memory_types() {
    let mut v = serde_json::json!({"connected": true, "system": "ss"});
    enrich_status_value(
        &mut v,
        &["read_memory".to_string()],
        &["workraml".to_string(), "vdp2vram".to_string()],
        None,
    );
    assert_eq!(
        v["memory_types"],
        serde_json::json!(["workraml", "vdp2vram"])
    );
}

#[test]
fn enrich_status_value_no_memory_types_when_empty() {
    // 어댑터가 빈 목록을 advertise(예: Debugger 부재)하면 표면화하지 않는다.
    let mut v = serde_json::json!({"connected": true});
    enrich_status_value(&mut v, &["status".to_string()], &[], None);
    assert!(v.get("memory_types").is_none());
}

#[test]
fn missing_system_does_not_default_to_snes() {
    // system을 특정할 수 없으면 input_buttons를 snes로 위장하지 말고 생략한다 —
    // 조용한 default는 다른 시스템(예: DC)을 SNES로 오표시하는 거짓 신호를 만든다.
    let mut v = serde_json::json!({"connected": true});
    enrich_status_value(&mut v, &["status".to_string()], &[], None);
    assert!(
        v.get("input_buttons").is_none(),
        "system 불명이면 input_buttons를 snes로 위장하지 말고 생략해야 한다"
    );
}

#[test]
fn enrich_status_value_disconnected_is_noop() {
    let mut v = serde_json::json!({"connected": false});
    enrich_status_value(
        &mut v,
        &["read_memory".to_string()],
        &["workraml".to_string()],
        None,
    );
    assert!(v.get("methods").is_none());
    assert!(v.get("memory_types").is_none());
}

#[test]
fn input_buttons_uses_fallback_system_when_top_level_missing() {
    // Flycast류: status 최상위에 system이 없고 어댑터가 advertise한 emulator_identity.system만
    // 있을 때 — fallback_system으로 정확한 힌트를 낸다(snes로 위장하지 않는다).
    let mut v = serde_json::json!({"connected": true});
    enrich_status_value(&mut v, &["status".to_string()], &[], Some("dreamcast"));
    assert_eq!(v["input_buttons"]["system"], "dreamcast");
}

#[test]
fn button_hint_none_for_unknown_or_absent_system() {
    assert!(button_hint_for_system(None).is_none());
    assert!(button_hint_for_system(Some("gamecube")).is_none());
    assert!(button_hint_for_system(Some("snes")).is_some());
    assert!(button_hint_for_system(Some("dreamcast")).is_some());
}
