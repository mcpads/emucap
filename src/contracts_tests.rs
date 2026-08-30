use super::*;

fn identity_methods() -> Vec<String> {
    vec!["status".to_string(), "step".to_string()]
}

#[test]
fn embedded_contract_sources_validate() {
    assert!(validate_sources(catalog(), registry()).is_empty());
}

#[test]
fn multi_operation_public_feature_can_expose_each_method_directly() {
    let mut catalog = catalog().clone();
    let feature = catalog
        .features
        .iter_mut()
        .find(|feature| feature.id == "debug.breakpoint")
        .unwrap();
    feature.route = None;

    let errors = validate_sources(&catalog, registry());
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn internal_wire_features_cannot_leak_into_an_mcp_route() {
    let mut catalog = catalog().clone();
    let feature = catalog
        .features
        .iter_mut()
        .find(|feature| feature.id == "transport.hello")
        .unwrap();
    feature.route = Some("debug".into());

    let errors = validate_sources(&catalog, registry());
    assert!(errors
        .iter()
        .any(|error| error == "non-public feature transport.hello cannot declare an MCP route"));
}

#[test]
fn public_features_can_use_only_declared_role_drawers() {
    let mut catalog = catalog().clone();
    let feature = catalog
        .features
        .iter_mut()
        .find(|feature| feature.id == "memory.write")
        .unwrap();
    feature.route = Some("platform_extras".into());

    let errors = validate_sources(&catalog, registry());
    assert!(errors
        .iter()
        .any(|error| error == "unknown public route platform_extras for memory.write"));
}

#[test]
fn every_method_has_exactly_one_feature_owner() {
    let mut owners = BTreeMap::new();
    for feature in &catalog().features {
        for method in &feature.methods {
            assert!(
                owners.insert(method, &feature.id).is_none(),
                "duplicate method owner: {method}"
            );
        }
    }
}

#[test]
fn unreported_adapter_is_not_promoted() {
    let status = validate_advertisement(
        &ContractAdvertisement::Unreported,
        Some("a"),
        Some("s"),
        &[],
    );
    assert_eq!(status.state, "unreported");
    assert!(status.active_exceptions.is_empty());
}

#[test]
fn known_scoped_advertisement_validates() {
    let value = advertisement_value(&["nds.execution.frame-step-vblank"]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let status = validate_advertisement(
        &ad,
        Some("desmume-nds-rust-gdb"),
        Some("nds"),
        &identity_methods(),
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert_eq!(
        status.constraints["execution.step.units"],
        json!(["frames", "instructions"])
    );
    assert_eq!(
        status.constraints["execution.step.frames.clock"],
        json!("nds_vblank_start_complete")
    );
    assert_eq!(
        status.constraints["execution.step.frames.cpu"],
        json!("shared_scheduler")
    );
    assert_eq!(
        status.constraints["execution.step.frames.terminal_state"],
        json!("frozen")
    );
}

#[test]
fn ppsspp_frame_step_advertises_vblank_clock_and_frozen_terminal() {
    let value = advertisement_value(&["ppsspp.execution.frame-step-vblank"]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let methods = ["status", "step", "step_instructions"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let status = validate_advertisement(&ad, Some("ppsspp-rust-ws"), Some("psp"), &methods);

    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert_eq!(
        status.constraints["execution.step.units"],
        json!(["frames", "instructions"])
    );
    assert_eq!(
        status.constraints["execution.step.frames.clock"],
        json!("psp_vblank_start")
    );
    assert_eq!(
        status.constraints["execution.step.frames.terminal_state"],
        json!("frozen")
    );
    assert_eq!(
        status.constraints["execution.step.frames.presented_output"],
        json!("not_claimed")
    );
}

#[test]
fn ppsspp_call_stack_advertises_frozen_best_effort_boundary() {
    let value = advertisement_value(&["ppsspp.call-stack.frozen-best-effort"]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let methods = ["status", "call_stack"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let status = validate_advertisement(&ad, Some("ppsspp-rust-ws"), Some("psp"), &methods);

    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert_eq!(
        status.constraints["debug.call-stack.execution_states.allowed"],
        json!(["frozen"])
    );
    assert_eq!(status.constraints["debug.call-stack.max_depth"], json!(256));
    assert_eq!(status.authority["debug.call-stack"], json!("best_effort"));
}

#[test]
fn neogeo_call_stack_advertises_frozen_best_effort_boundary() {
    let value = advertisement_value(&["neogeo.call-stack.frozen-best-effort"]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let methods = ["status", "call_stack"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    for system in ["neogeo_mvs", "neogeo_aes", "neogeo_cd"] {
        let status =
            validate_advertisement(&ad, Some("mame-neogeo-rust-gdb"), Some(system), &methods);
        assert_eq!(status.state, "validated", "{:?}", status.errors);
        assert_eq!(
            status.constraints["debug.call-stack.execution_states.allowed"],
            json!(["frozen"])
        );
        assert_eq!(status.constraints["debug.call-stack.max_depth"], json!(64));
        assert_eq!(status.authority["debug.call-stack"], json!("best_effort"));
    }
}

#[test]
fn mesen_instruction_step_advertises_main_cpu_scope_without_the_absent_exception() {
    for system in ["snes", "gamegear", "gb", "gba", "nes"] {
        let value = advertisement_value(&["mesen.execution.instruction-step-main-cpu"]);
        let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
        let methods = ["status", "step", "step_instructions"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();

        let status = validate_advertisement(&ad, Some("mesen2-live"), Some(system), &methods);

        assert_eq!(status.state, "validated", "{system}: {:?}", status.errors);
        assert_eq!(
            status.constraints["execution.step.instructions.cpu"],
            json!("main")
        );
        assert_eq!(
            status.constraints["execution.step.instructions.auxiliary_clocks"],
            json!("unspecified")
        );
        assert!(!status
            .active_exceptions
            .iter()
            .any(|id| id == "mesen.execution.instruction-step-absent"));
    }
}

#[test]
fn dolphin_reset_advertises_native_button_release_and_frozen_terminal() {
    for system in ["gamecube", "gc", "ngc", "wii"] {
        let value = advertisement_value(&["dolphin.reset.native-button-tap"]);
        let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
        let methods = ["status", "reset"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();

        let status = validate_advertisement(&ad, Some("dolphin-native"), Some(system), &methods);

        assert_eq!(status.state, "validated", "{system}: {:?}", status.errors);
        assert_eq!(
            status.constraints["execution.reset.kind"],
            json!("native_reset_button_tap")
        );
        assert_eq!(
            status.constraints["execution.reset.completion_boundary"],
            json!("button_release")
        );
        assert_eq!(
            status.constraints["execution.reset.terminal_state"],
            json!("frozen")
        );
    }
}

#[test]
fn public_step_and_compatibility_execution_methods_have_contract_owners() {
    let value = advertisement_value(&[]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let methods = ["step", "run_frames", "step_instructions"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();

    let status = validate_advertisement(&ad, Some("adapter"), Some("system"), &methods);
    assert_eq!(status.state, "validated", "{:?}", status.errors);
}

#[test]
fn bounded_advance_features_require_pausing_stops_to_preempt_progress() {
    for feature_id in [
        "execution.step",
        "execution.step-instructions-wire",
        "execution.run-frames-wire",
    ] {
        let feature = catalog()
            .features
            .iter()
            .find(|feature| feature.id == feature_id)
            .unwrap_or_else(|| panic!("missing bounded advance feature {feature_id}"));
        assert!(
            feature
                .expectations
                .iter()
                .any(|expectation| expectation == "TIME.STOP.1"),
            "{feature_id} must preserve configured debugger stops"
        );
    }
}

#[test]
fn dolphin_native_advertisement_exposes_its_composition_limits() {
    let value = advertisement_value(&[
        "dolphin.breakpoint.pausing-subset",
        "dolphin.input-hold.port-zero-only",
        "dolphin.state-save.frozen-only",
        "dolphin.state-load.frozen-only",
        "dolphin.screenshot.running-only",
        "dolphin.call-stack.best-effort",
    ]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let methods = [
        "status",
        "step",
        "set_breakpoint",
        "set_input",
        "save_state",
        "load_state",
        "screenshot",
        "call_stack",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let status = validate_advertisement(&ad, Some("dolphin-native"), Some("gamecube"), &methods);

    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert!(!status.constraints.contains_key("execution.step.units"));
    assert_eq!(
        status.constraints["state.save.execution_states.allowed"],
        json!(["frozen"])
    );
    assert_eq!(
        status.constraints["state.load.execution_states.allowed"],
        json!(["frozen"])
    );
    assert_eq!(status.authority["debug.call-stack"], json!("best_effort"));
    assert_eq!(
        status.constraints["breakpoint.kinds.allowed"],
        json!(["exec", "read", "write"])
    );
    assert_eq!(
        status.constraints["breakpoint.native_ownership"],
        json!("remove_only_matching_emucap_id")
    );
    assert_eq!(
        status.constraints["video.capture.execution_states.allowed"],
        json!(["running"])
    );
}

#[test]
fn dolphin_wii_input_uses_the_scoped_port_zero_contract() {
    let value = advertisement_value(&[
        "dolphin.breakpoint.pausing-subset",
        "dolphin.input-hold.port-zero-only",
        "dolphin.state-save.frozen-only",
        "dolphin.state-load.frozen-only",
        "dolphin.screenshot.running-only",
        "dolphin.call-stack.best-effort",
    ]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let methods = ["status", "pause", "step", "set_input"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();

    let status = validate_advertisement(&ad, Some("dolphin-native"), Some("wii"), &methods);

    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert_eq!(status.constraints["input.ports.allowed"], json!([0]));
}

#[test]
fn xemu_state_advertisement_exposes_frozen_same_generation_loading() {
    let value = advertisement_value(&[
        "xemu.state-save.frozen-only",
        "xemu.state-load.frozen-only",
        "xemu.state-load.same-generation-only",
    ]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let methods = ["status", "save_state", "load_state"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let status = validate_advertisement(&ad, Some("xemu-rust-qmp-gdb"), Some("xbox"), &methods);

    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert_eq!(
        status.constraints["state.save.execution_states.allowed"],
        json!(["frozen"])
    );
    assert_eq!(
        status.constraints["state.load.execution_states.allowed"],
        json!(["frozen"])
    );
    assert_eq!(
        status.constraints["state.load.portability"],
        json!("same_generation_only")
    );
    assert_eq!(
        status.constraints["state.load.media_boundary"],
        json!("exact_current_disc")
    );
}

#[test]
fn unknown_exception_is_unvalidated() {
    let ad = ContractAdvertisement::Reported(AdvertisedContracts {
        catalog: CATALOG_ID.to_string(),
        active_exceptions: vec!["unknown.exception".to_string()],
        constraints: None,
        authority: None,
    });
    let status = validate_advertisement(&ad, Some("a"), Some("s"), &[]);
    assert_eq!(status.state, "unvalidated");
    assert!(status.errors[0].contains("unknown active exception"));
}

#[test]
fn scope_mismatch_is_unvalidated() {
    let value = advertisement_value(&["nds.execution.frame-step-vblank"]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let status = validate_advertisement(&ad, Some("wrong"), Some("nds"), &identity_methods());
    assert_eq!(status.state, "unvalidated");
    assert!(status
        .errors
        .iter()
        .any(|error| error.contains("scope adapter")));
}

#[test]
fn unowned_constraint_is_unvalidated() {
    let ad = ContractAdvertisement::Reported(AdvertisedContracts {
        catalog: CATALOG_ID.to_string(),
        active_exceptions: Vec::new(),
        constraints: Some(BTreeMap::from([(
            "input.port.allowed".to_string(),
            json!([0]),
        )])),
        authority: None,
    });
    let status = validate_advertisement(&ad, Some("a"), Some("s"), &[]);
    assert_eq!(status.state, "unvalidated");
    assert!(status
        .errors
        .iter()
        .any(|error| error.contains("constraints do not match")));
}

#[test]
fn method_without_feature_contract_is_unvalidated() {
    let ad = ContractAdvertisement::Reported(AdvertisedContracts {
        catalog: CATALOG_ID.to_string(),
        active_exceptions: Vec::new(),
        constraints: None,
        authority: None,
    });
    let status = validate_advertisement(&ad, Some("a"), Some("s"), &["mystery".to_string()]);
    assert_eq!(status.state, "unvalidated");
    assert!(status
        .errors
        .iter()
        .any(|error| error.contains("no feature contract")));
}
