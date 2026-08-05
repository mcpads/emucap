use super::link::LinkError;
use super::link::*;
use serde_json::json;

#[test]
fn fake_link_reports_capabilities() {
    let link = FakeLink::ok(json!({ "hex": "00" }));
    assert_eq!(link.capabilities().protocol_version, 1);
}

#[test]
fn fake_link_returns_canned_result() {
    let mut link = FakeLink::ok(json!({ "hex": "00ff" }));
    let out = link.call("read_memory", json!({ "address": 0 })).unwrap();
    assert_eq!(out["hex"], "00ff");
}

#[test]
fn fake_link_can_return_error() {
    let mut link = FakeLink::err(LinkError::NotConnected);
    assert!(matches!(
        link.call("read_memory", json!({})),
        Err(LinkError::NotConnected)
    ));
}

#[test]
fn link_error_messages() {
    assert!(format!("{}", LinkError::Busy).contains("another session"));
    assert!(format!(
        "{}",
        LinkError::NoSuchEmulator {
            names: vec!["a".into()]
        }
    )
    .contains("a"));
    assert!(format!(
        "{}",
        LinkError::Ambiguous {
            names: vec!["a".into(), "b".into()]
        }
    )
    .contains("b"));
}

#[test]
fn mesen_host_features_distinguish_native_halt_from_safe_savestates() {
    let halt_only = EmulatorIdentity::from_hello(&json!({
        "mesen_host_api": 1,
        "host_features": ["code_break_idle", "native_halt_service"]
    }));
    assert!(halt_only.has_mesen_native_halt());
    assert!(!halt_only.has_mesen_native_halt_savestate());

    let safe_state = EmulatorIdentity::from_hello(&json!({
        "mesen_host_api": 2,
        "host_features": [
            "code_break_idle",
            "native_halt_service",
            "native_halt_savestate"
        ]
    }));
    assert!(safe_state.has_mesen_native_halt());
    assert!(safe_state.has_mesen_native_halt_savestate());
}

#[test]
fn memory_regions_accept_exact_finite_regions_from_advertised_memory_types() {
    let memory_types = vec!["workram".to_string(), "vram".to_string()];
    let regions = memory_regions_from_hello(
        Some(&json!([
            {"memory_type": "workram", "size": 131072},
            {"memory_type": "vram", "size": 65536}
        ])),
        &memory_types,
    )
    .unwrap();
    assert_eq!(regions[0].memory_type, "workram");
    assert_eq!(regions[0].size, 131072);
    assert_eq!(regions[1].memory_type, "vram");
}

#[test]
fn memory_regions_are_optional_for_legacy_adapters() {
    assert!(memory_regions_from_hello(None, &["workram".into()])
        .unwrap()
        .is_empty());
}

#[test]
fn memory_regions_reject_unusable_or_ambiguous_claims() {
    let memory_types = vec!["workram".to_string()];
    for invalid in [
        json!([{"memory_type": "unknown", "size": 1}]),
        json!([{"memory_type": "workram", "size": 0}]),
        json!([
            {"memory_type": "workram", "size": 1},
            {"memory_type": "workram", "size": 2}
        ]),
        json!([{"memory_type": "workram", "size": 1, "guessed": true}]),
    ] {
        assert!(memory_regions_from_hello(Some(&invalid), &memory_types).is_err());
    }
}
