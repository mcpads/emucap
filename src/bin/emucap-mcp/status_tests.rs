use super::*;

fn enriched_from(mut v: serde_json::Value, methods: &[&str]) -> serde_json::Value {
    let m: Vec<String> = methods.iter().map(|s| s.to_string()).collect();
    enrich_status_value(&mut v, &m, &[], None);
    let identity = EmulatorIdentity {
        adapter: Some("contract-test".into()),
        system: Some("test".into()),
        ..Default::default()
    };
    let hello = serde_json::json!({
        "contracts": emucap::contracts::advertisement_value(&[])
    });
    enrich_contract_status(
        &mut v,
        &identity,
        &emucap::contracts::advertisement_from_hello(&hello),
    );
    v
}

fn enriched(methods: &[&str]) -> serde_json::Value {
    enriched_from(serde_json::json!({"connected": true}), methods)
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
    for c in ["tap", "hold_until", "regression_run", "verify_determinism"] {
        assert!(has_method(&v, c), "composite {c} missing");
    }
    // 의존 충족된 풀셋엔 substitute note가 없다.
    assert!(v.get("capability_notes").is_none());
}

#[test]
fn write_memory_exposes_host_input_bounds() {
    let value = enriched(&["write_memory"]);
    assert_eq!(
        value.pointer("/contracts/constraints/memory.write.input_sources"),
        Some(&serde_json::json!(["hex", "file"]))
    );
    assert_eq!(
        value.pointer("/contracts/constraints/memory.write.max_bytes"),
        Some(&serde_json::json!(tools::MAX_WRITE_BYTES))
    );
    assert_eq!(
        value.pointer("/contracts/constraints/memory.write.file_load_timeout_ms"),
        Some(&serde_json::json!(
            crate::memory_write::FILE_LOAD_TIMEOUT_MS
        ))
    );
}

#[test]
fn synchronous_advance_exposes_host_admission_bounds() {
    let value = enriched(&["step", "run_frames"]);
    assert_eq!(
        value.pointer("/contracts/constraints/execution.step.max_count"),
        Some(&serde_json::json!(crate::args::MAX_SYNC_ADVANCE_COUNT))
    );
    assert_eq!(
        value.pointer("/contracts/constraints/execution.run_frames.max_frames"),
        Some(&serde_json::json!(crate::args::MAX_SYNC_ADVANCE_COUNT))
    );
}

#[test]
fn synchronous_advance_uses_smaller_adapter_frame_limit() {
    let value = enriched_from(
        serde_json::json!({
            "connected": true,
            "execution_limits": {
                "max_sync_advance_count": 5_000,
                "frame": {"max_count": 49}
            }
        }),
        &["step", "run_frames"],
    );
    assert_eq!(
        value.pointer("/contracts/constraints/execution.step.max_count"),
        Some(&serde_json::json!(5_000))
    );
    assert_eq!(
        value.pointer("/contracts/constraints/execution.run_frames.max_frames"),
        Some(&serde_json::json!(49))
    );
}

#[test]
fn composites_absent_without_deps_and_trace_note_present() {
    // MD류: set_input/step/pause 있으나 probe 없음 → tap O. trace 없음 → 콜체인 역추적 대체 note만.
    let v = enriched(&[
        "set_input",
        "step",
        "pause",
        "read_memory",
        "set_breakpoint",
        "screenshot",
    ]);
    assert!(has_method(&v, "tap") && has_method(&v, "hold_until"));
    let notes = v["capability_notes"].as_array().unwrap();
    assert_eq!(
        notes.len(),
        1,
        "only the trace substitute should be derived"
    );
    assert!(notes_contain(&v, "exec breakpoint"));
    // step 없으면 tap도 없다.
    let v2 = enriched(&["read_memory", "set_breakpoint"]);
    assert!(!has_method(&v2, "tap"));
}

#[test]
fn replay_composites_accept_load_state_without_probe() {
    // Flycast류: load_state 광고·probe 미광고여도 regression/verify는 가능하다.
    let v = enriched(&[
        "set_input",
        "step",
        "pause",
        "load_state",
        "run_frames",
        "read_memory",
    ]);
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

    let pcfx = button_hint_for_system(Some("pc-fx")).unwrap();
    assert_eq!(pcfx["system"], "pcfx");
    assert_eq!(pcfx["aliases"]["start"], "run");
    assert_eq!(pcfx["aliases"]["a"], "i");

    let md = button_hint_for_system(Some("md")).unwrap();
    assert_eq!(md["aliases"]["enter"], "start");

    let ngp = button_hint_for_system(Some("ngpc")).unwrap();
    assert_eq!(ngp["system"], "ngp");
    assert_eq!(ngp["aliases"]["start"], "option");
    assert_eq!(
        ngp["buttons"],
        serde_json::json!(["a", "b", "option", "up", "down", "left", "right"])
    );
}

#[test]
fn dreamcast_button_hint_exposes_start_aliases() {
    let hint = button_hint_for_system(Some("dreamcast")).unwrap();

    assert_eq!(hint["aliases"]["enter"], "start");
    assert_eq!(hint["aliases"]["return"], "start");
    assert!(hint["notes"]
        .as_str()
        .unwrap()
        .contains("only controller port 0"));
    assert!(!hint["notes"].as_str().unwrap().contains("ignored"));
    assert!(hint["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b.as_str() == Some("start")));
}

#[test]
fn nds_button_hint_describes_current_input_contract() {
    let hint = button_hint_for_system(Some("nds")).unwrap();
    let notes = hint["notes"].as_str().unwrap();

    assert!(notes.contains("only controller port 0"));
    assert!(notes.contains("dedicated touch tool"));
    assert!(!notes.contains("planned"));
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
fn runtime_paths_exposes_build_and_runtime_locations_without_launcher_aliases() {
    let paths = runtime_paths(Some(47803));
    let root = paths
        .get("repo_root")
        .and_then(|v| v.as_str())
        .expect("repo_root");
    assert!(std::path::Path::new(root).join("Cargo.toml").is_file());
    assert!(paths
        .pointer("/adapters/openmsx/bridge_binary")
        .and_then(|v| v.as_str())
        .is_some_and(|path| path.contains("emucap-openmsx-bridge")));
    assert!(paths.pointer("/adapters/mesen2/build").is_some());
    assert!(paths.pointer("/runtime_capsule/current").is_some());
    assert!(paths.pointer("/command_templates").is_none());
    assert!(paths.pointer("/legacy_fallbacks").is_none());
    assert!(paths.pointer("/adapters/mesen2/launch").is_none());
    assert!(paths.pointer("/adapters/pcsx2/launch").is_none());
}

#[test]
fn supported_system_catalog_keeps_platform_routing_without_launcher_paths() {
    let systems = supported_systems_value();
    assert!(systems
        .as_array()
        .is_some_and(|systems| systems.iter().all(|system| {
            system.get("launcher").is_none() && system.get("legacy_launcher").is_none()
        })));
    assert!(supported_systems_value()
        .as_array()
        .and_then(|systems| systems.iter().find(|system| system["system"] == "n64"))
        .is_some_and(|system| {
            system["adapter"] == "mupen64plus"
                && system["content"] == serde_json::json!(["z64", "n64", "v64"])
        }));
    assert!(supported_systems_value()
        .as_array()
        .and_then(|systems| systems.iter().find(|system| system["system"] == "msx"))
        .is_some_and(|system| {
            system["adapter"] == "openmsx"
                && system["content"] == serde_json::json!(["rom", "mx1", "mx2", "ri", "sg"])
        }));
    assert!(supported_systems_value()
        .as_array()
        .and_then(|systems| systems.iter().find(|system| system["system"] == "wswan"))
        .is_some_and(|system| {
            system["adapter"] == "mednafen"
                && system["content"] == serde_json::json!(["ws", "wsc", "wsr"])
                && system["force_module"] == "wswan"
        }));
    assert!(supported_systems_value()
        .as_array()
        .and_then(|systems| systems.iter().find(|system| system["system"] == "pcfx"))
        .is_some_and(|system| {
            system["adapter"] == "mednafen"
                && system["content"] == serde_json::json!(["cue", "ccd", "toc", "m3u"])
                && system["force_module"] == "pcfx"
                && system["required_firmware"] == serde_json::json!(["pcfx.rom"])
        }));
    assert!(supported_systems_value()
        .as_array()
        .and_then(|systems| systems.iter().find(|system| system["system"] == "ngp"))
        .is_some_and(|system| {
            system["adapter"] == "mednafen"
                && system["content"] == serde_json::json!(["ngp", "ngpc", "ngc", "npc"])
                && system["force_module"] == "ngp"
        }));
    assert!(supported_systems_value()
        .as_array()
        .and_then(|systems| systems
            .iter()
            .find(|system| system["system"] == "neogeo_aes"))
        .is_some_and(|system| {
            system["adapter"] == "mame_neogeo"
                && system["content"] == serde_json::json!(["zip"])
                && system["required_firmware"] == serde_json::json!(["aes.zip"])
        }));
    assert!(supported_systems_value()
        .as_array()
        .and_then(|systems| systems
            .iter()
            .find(|system| system["system"] == "neogeo_cd"))
        .is_some_and(|system| {
            system["adapter"] == "mame_neogeo"
                && system["content"] == serde_json::json!(["cue"])
                && system["required_firmware"] == serde_json::json!(["neocdz.zip"])
        }));
}

#[test]
fn neogeo_aes_button_hint_describes_console_controls() {
    let hint = button_hint_for_system(Some("neogeo_aes")).expect("AES input hint");
    assert_eq!(hint["system"], "neogeo_aes");
    assert!(hint["buttons"]
        .as_array()
        .is_some_and(|buttons| buttons.iter().any(|button| button == "select")));
    assert!(!hint["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|button| button == "coin"));
}

#[test]
fn msx_button_hint_describes_keyboard_matrix_and_release() {
    let hint = button_hint_for_system(Some("msx")).expect("MSX input hint");
    assert_eq!(hint["system"], "msx");
    assert!(hint["buttons"]
        .as_array()
        .is_some_and(|buttons| buttons.iter().any(|button| button == "space")));
    assert_eq!(hint["aliases"]["fire1"], "space");
    assert_eq!(hint["devices"][0]["device"], "keyboard");
    assert_eq!(hint["devices"][1]["device"], "joystick");
    assert_eq!(hint["devices"][1]["port"], 1);
    assert_eq!(hint["devices"][2]["port"], 2);
    assert!(hint["notes"]
        .as_str()
        .is_some_and(|notes| notes.contains("empty set") && notes.contains("native input")));
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
            breakpoint_kinds: vec![],
            contracts: emucap::contracts::ContractAdvertisement::Unreported,
            identity: EmulatorIdentity::default(),
        },
    };
    let value = make_bootstrap_value(&mut link, false, false).unwrap();
    assert_eq!(value["listener"]["state"], "bound");
    assert_eq!(value["listener"]["port"], 47855);
    assert_eq!(value["adapter_connection"]["state"], "disconnected");
    assert_eq!(value["entry"]["state"], "ready_for_content");
    assert_eq!(value["entry"]["reason"], "ready_no_history");
    assert_eq!(value["entry"]["primary_action"]["kind"], "resolve_input");
    assert_eq!(
        value["entry"]["primary_action"]["question_if_missing"],
        unknown_content_question()
    );
    assert_eq!(
        value["entry"]["primary_action"]["required_input"],
        serde_json::json!(["content_path"])
    );
    assert_eq!(
        value
            .pointer("/entry/primary_action/then_call/tool")
            .and_then(|v| v.as_str()),
        Some("launch_plan")
    );
    assert!(value["supported_system_ids"].is_array());
    assert!(value["system_catalog_revision"]
        .as_str()
        .is_some_and(|revision| revision.starts_with("sha256:")));
    assert!(value.get("supported_systems").is_none());
    assert!(value.get("runtime_paths").is_none());
    assert!(value.get("status").is_none());
    assert!(value.get("workflow").is_none());
    assert!(value.get("do_not").is_none());
    assert!(value.get("ok").is_none());
}

#[test]
fn bootstrap_details_are_explicit_opt_in_sections() {
    let mut link = NotConnectedLink {
        caps: emucap::live::link::Capabilities::empty(),
    };

    let compact = make_bootstrap_value(&mut link, false, false).unwrap();
    let value = make_bootstrap_value(&mut link, true, true).unwrap();

    assert!(value["supported_systems"].is_array());
    assert!(value["runtime_paths"].is_object());
    assert_eq!(value["entry"], compact["entry"]);
}

#[test]
fn matching_capability_revision_omits_only_the_capability_snapshot() {
    let mut full = serde_json::json!({
        "connected": true,
        "execution": {"state": "frozen"},
        "continuity": {"transport": {"state": "connected"}},
        "emulator_identity": {"system": "snes", "adapter": "mesen2", "launch_id": "launch-a"},
        "methods": ["status", "read_memory"],
        "memory_types": ["workram"],
        "breakpoint_kinds": [{"kind": "exec"}],
        "input_buttons": {"buttons": ["a"]},
        "contracts": {"state": "validated"},
        "capability_notes": ["test"],
        "execution_limits": {"frame": {"max_count": 60}}
    });
    let revision = apply_capability_revision(&mut full, None);
    assert_eq!(full["capability_snapshot"], "full");
    assert!(full.get("methods").is_some());

    let mut unchanged = serde_json::json!({
        "connected": true,
        "execution": {"state": "running"},
        "continuity": {"transport": {"state": "connected"}},
        "emulator_identity": {"system": "snes", "adapter": "mesen2", "launch_id": "launch-a"},
        "methods": ["status", "read_memory"],
        "memory_types": ["workram"],
        "breakpoint_kinds": [{"kind": "exec"}],
        "input_buttons": {"buttons": ["a"]},
        "contracts": {"state": "validated"},
        "capability_notes": ["test"],
        "execution_limits": {"frame": {"max_count": 60}}
    });
    apply_capability_revision(&mut unchanged, Some(&revision));

    assert_eq!(unchanged["capability_snapshot"], "unchanged");
    assert_eq!(unchanged["execution"]["state"], "running");
    assert!(unchanged.get("continuity").is_some());
    for field in CAPABILITY_FIELDS {
        assert!(
            unchanged.get(*field).is_none(),
            "{field} should be omitted when unchanged"
        );
    }
}

#[test]
fn capability_revision_changes_with_catalog_or_generation() {
    let base = serde_json::json!({
        "emulator_identity": {"system": "snes", "adapter": "mesen2", "launch_id": "launch-a"},
        "methods": ["status"],
        "memory_types": ["workram"]
    });
    let mut changed_catalog = base.clone();
    changed_catalog["methods"] = serde_json::json!(["status", "read_memory"]);
    let mut changed_generation = base.clone();
    changed_generation["emulator_identity"]["launch_id"] = serde_json::json!("launch-b");

    assert_ne!(
        capability_revision(&base),
        capability_revision(&changed_catalog)
    );
    assert_ne!(
        capability_revision(&base),
        capability_revision(&changed_generation)
    );
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

    let observation = observe_control_state(&mut link).unwrap();
    assert_eq!(observation.status["request_succeeded"], false);
    assert_eq!(observation.status["error_kind"], "request_timeout");
    assert_eq!(observation.runtime.listener, ListenerState::Bound);
    assert_eq!(
        observation.runtime.transport,
        emucap::live::continuity::TransportState::Stalled
    );
    assert_eq!(
        observation.disposition.state,
        emucap::live::task_entry::EntryState::TransitionBlocked
    );
    assert_eq!(
        observation.disposition.reason,
        emucap::live::task_entry::EntryReason::TransportUncertain
    );

    let value = make_bootstrap_value(&mut link, false, false).unwrap();
    assert_eq!(value["listener"]["port"], 47856);
    assert_eq!(value["adapter_connection"]["state"], "stalled");
    assert_eq!(value["entry"]["state"], "transition_blocked");
    assert_eq!(value["entry"]["reason"], "transport_uncertain");
    assert_eq!(value["entry"]["primary_action"]["tool"], "status");
    assert!(value.get("status").is_none());
    assert!(value.get("error").is_none());
}

#[test]
fn mismatched_live_identity_demotes_the_old_runtime_capsule() {
    let mut value = serde_json::json!({
        "connected": true,
        "runtime_instance": {"launch_id": "old-pc98"}
    });
    let mut continuity = emucap::live::continuity::ContinuitySnapshot::default();
    continuity.runtime_binding = emucap::live::continuity::RuntimeBinding {
        state: emucap::live::continuity::RuntimeBindingState::Mismatched,
        current_launch_id: Some("old-pc98".into()),
        live_launch_id: None,
        reason: "live identity differs".into(),
    };

    enrich_runtime_instance(
        value.as_object_mut().unwrap(),
        &continuity,
        Some(serde_json::json!({"launch_id": "old-pc98", "system": "pc98"})),
    );

    assert!(value.get("runtime_instance").is_none());
    assert_eq!(value["stale_runtime_instance"]["launch_id"], "old-pc98");
    assert!(value["next_safe_action"]
        .as_str()
        .is_some_and(|message| message.contains("do not treat the stale capsule")));
}

struct DiagnosticLink {
    caps: emucap::live::link::Capabilities,
    artifact: &'static str,
    blocks_generation_transition: bool,
}

impl EmulatorLink for DiagnosticLink {
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
        Some(47857)
    }

    fn continuity(&self) -> emucap::live::continuity::ContinuitySnapshot {
        let mut continuity = emucap::live::continuity::ContinuitySnapshot::default();
        continuity
            .runtime_diagnostics
            .push(emucap::live::continuity::RuntimeDiagnostic {
                artifact: self.artifact.into(),
                path: "/runtime/47857/current.json".into(),
                kind: "invalid".into(),
                reason: "invalid JSON".into(),
                blocks_generation_transition: self.blocks_generation_transition,
            });
        continuity
    }
}

#[test]
fn bootstrap_returns_diagnostic_json_when_runtime_capsule_is_corrupt() {
    let mut link = DiagnosticLink {
        caps: emucap::live::link::Capabilities::empty(),
        artifact: "current",
        blocks_generation_transition: true,
    };

    let value = make_bootstrap_value(&mut link, false, false).unwrap();

    assert_eq!(value["entry"]["state"], "repair_runtime_metadata");
    assert_eq!(value["entry"]["reason"], "runtime_metadata_invalid");
    assert_eq!(value["entry"]["primary_action"]["tool"], "status");
    assert!(value.get("status").is_none());
    assert!(!value.to_string().contains("/runtime/47857/current.json"));
}

#[test]
fn bootstrap_does_not_block_on_corrupt_adapter_failure_evidence() {
    let mut link = DiagnosticLink {
        caps: emucap::live::link::Capabilities::empty(),
        artifact: "adapter_failure",
        blocks_generation_transition: false,
    };

    let value = make_bootstrap_value(&mut link, false, false).unwrap();

    assert_eq!(value["entry"]["state"], "ready_for_content");
    assert_eq!(value["entry"]["reason"], "ready_no_history");
    assert_eq!(value["entry"]["primary_action"]["kind"], "resolve_input");
    assert_eq!(
        value["entry"]["primary_action"]["then_call"]["tool"],
        "launch_plan"
    );
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
fn contract_status_distinguishes_unreported_from_validated() {
    let identity = EmulatorIdentity {
        adapter: Some("desmume-nds-rust-gdb".into()),
        system: Some("nds".into()),
        ..Default::default()
    };
    let mut unreported = serde_json::json!({
        "connected": true,
        "methods": ["status", "step", "call_stack"]
    });
    enrich_contract_status(
        &mut unreported,
        &identity,
        &emucap::contracts::ContractAdvertisement::Unreported,
    );
    assert_eq!(unreported["contracts"]["state"], "unreported");
    assert!(
        !has_method(&unreported, "tap"),
        "unreported primitive set must not be promoted to a composite"
    );
    assert_eq!(
        unreported["contracts"]["catalog"],
        emucap::contracts::CATALOG_ID
    );

    let hello = serde_json::json!({
        "contracts": emucap::contracts::advertisement_value(&[
            "nds.execution.frame-step-absent",
            "nds.call-stack.best-effort",
        ])
    });
    let advertisement = emucap::contracts::advertisement_from_hello(&hello);
    let mut validated = serde_json::json!({
        "connected": true,
        "methods": ["status", "step", "call_stack"]
    });
    enrich_contract_status(&mut validated, &identity, &advertisement);
    assert_eq!(validated["contracts"]["state"], "validated");
    assert_eq!(
        validated["contracts"]["active_exceptions"],
        serde_json::json!([
            "nds.execution.frame-step-absent",
            "nds.call-stack.best-effort"
        ])
    );
    assert_eq!(
        validated["contracts"]["constraints"]["execution.step.units"],
        serde_json::json!(["instructions"])
    );
    assert_eq!(
        validated["contracts"]["authority"]["debug.call-stack"],
        "best_effort"
    );
}

#[test]
fn public_methods_consolidate_instruction_step_without_duplicates() {
    let mut v = serde_json::json!({"connected": true});
    enrich_status_value(
        &mut v,
        &[
            "status".to_string(),
            "step".to_string(),
            "step_instructions".to_string(),
        ],
        &[],
        None,
    );

    assert_eq!(v["methods"], serde_json::json!(["status", "step"]));
}

#[test]
fn adapter_provided_status_methods_are_also_normalized() {
    let mut v = serde_json::json!({
        "connected": true,
        "methods": ["status", "step_instructions"]
    });
    enrich_status_value(&mut v, &[], &[], None);

    assert_eq!(v["methods"], serde_json::json!(["status", "step"]));
}

#[test]
fn instruction_only_step_does_not_admit_frame_composites() {
    let identity = EmulatorIdentity {
        adapter: Some("desmume-nds-rust-gdb".into()),
        system: Some("nds".into()),
        ..Default::default()
    };
    let methods = [
        "status",
        "set_input",
        "step_instructions",
        "pause",
        "read_memory",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    let hello = serde_json::json!({
        "contracts": emucap::contracts::advertisement_value(&[
            "nds.execution.frame-step-absent"
        ])
    });
    let mut value = serde_json::json!({"connected": true});

    enrich_status_value(&mut value, &methods, &[], None);
    enrich_contract_status(
        &mut value,
        &identity,
        &emucap::contracts::advertisement_from_hello(&hello),
    );

    assert!(has_method(&value, "step"));
    assert!(!has_method(&value, "step_instructions"));
    assert!(!has_method(&value, "tap"));
    assert!(!has_method(&value, "hold_until"));
    assert_eq!(value["contracts"]["state"], "validated");
}

#[test]
fn composite_admission_requires_a_validated_contract_generation() {
    let identity = EmulatorIdentity {
        adapter: Some("contract-test".into()),
        system: Some("test".into()),
        ..Default::default()
    };
    let methods = ["set_input", "step", "pause", "read_memory"]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

    let mut unreported = serde_json::json!({"connected": true});
    enrich_status_value(&mut unreported, &methods, &[], None);
    enrich_contract_status(
        &mut unreported,
        &identity,
        &emucap::contracts::ContractAdvertisement::Unreported,
    );
    assert!(!has_method(&unreported, "tap"));

    let hello = serde_json::json!({
        "contracts": emucap::contracts::advertisement_value(&[])
    });
    let mut validated = serde_json::json!({"connected": true});
    enrich_status_value(&mut validated, &methods, &[], None);
    enrich_contract_status(
        &mut validated,
        &identity,
        &emucap::contracts::advertisement_from_hello(&hello),
    );
    assert!(has_method(&validated, "tap"));
    assert!(has_method(&validated, "hold_until"));
}

#[test]
fn normalize_rom_sha1_prefers_content_md5() {
    // Mednafen: content_md5가 식별 기준이고 sha1은 보조 — content_md5를 rom_sha1로.
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
fn breakpoint_kinds_are_connection_advertised_data() {
    let advertised = vec![serde_json::json!({
        "kind": "device_boundary",
        "range_unit": "scanline",
        "memory_type_used": false,
        "snapshot": true
    })];
    let mut connected = serde_json::json!({"connected": true});
    enrich_breakpoint_kinds(&mut connected, &advertised);
    assert_eq!(connected["breakpoint_kinds"], serde_json::json!(advertised));

    let mut disconnected = serde_json::json!({"connected": false});
    enrich_breakpoint_kinds(&mut disconnected, &advertised);
    assert!(disconnected.get("breakpoint_kinds").is_none());
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
    assert!(button_hint_for_system(Some("unknown")).is_none());
    assert!(button_hint_for_system(Some("gamecube")).is_some());
    let wii = button_hint_for_system(Some("wii")).unwrap();
    assert_eq!(wii["system"], "wii");
    assert_eq!(
        wii["buttons"],
        serde_json::json!([
            "a", "b", "one", "two", "minus", "plus", "home", "up", "down", "left", "right"
        ])
    );
    assert!(button_hint_for_system(Some("snes")).is_some());
    assert!(button_hint_for_system(Some("dreamcast")).is_some());
}
