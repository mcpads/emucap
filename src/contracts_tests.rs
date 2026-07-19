use super::*;

fn identity_methods() -> Vec<String> {
    vec!["status".to_string(), "step".to_string()]
}

#[test]
fn embedded_contract_sources_validate() {
    assert!(validate_sources(catalog(), registry()).is_empty());
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
    let value = advertisement_value(&["nds.execution.frame-step-absent"]);
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
        json!(["instructions"])
    );
}

#[test]
fn dolphin_native_advertisement_exposes_its_composition_limits() {
    let value = advertisement_value(&[
        "dolphin.execution.frame-step-absent",
        "dolphin.breakpoint.exact-exec-only",
        "dolphin.input-hold.port-zero-only",
        "dolphin.state-save.frozen-only",
        "dolphin.state-load.frozen-only",
        "dolphin.screenshot.running-only",
    ]);
    let ad = ContractAdvertisement::Reported(serde_json::from_value(value).unwrap());
    let methods = [
        "status",
        "step_instructions",
        "set_breakpoint",
        "set_input",
        "save_state",
        "load_state",
        "screenshot",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let status = validate_advertisement(&ad, Some("dolphin-native"), Some("gamecube"), &methods);

    assert_eq!(status.state, "validated", "{:?}", status.errors);
    assert_eq!(
        status.constraints["execution.step.units"],
        json!(["instructions"])
    );
    assert_eq!(
        status.constraints["state.save.execution_states.allowed"],
        json!(["frozen"])
    );
    assert_eq!(
        status.constraints["state.load.execution_states.allowed"],
        json!(["frozen"])
    );
    assert_eq!(
        status.constraints["video.capture.execution_states.allowed"],
        json!(["running"])
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
    let value = advertisement_value(&["nds.execution.frame-step-absent"]);
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
