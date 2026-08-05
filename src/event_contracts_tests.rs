use crate::bundle::recording_manifest::EventClassIdentity;
use crate::event_contracts::{EventContractError, EventContractRegistry};

#[test]
fn builtin_registry_verifies_its_contract_digest() {
    let registry = EventContractRegistry::builtin().unwrap();
    let identities = registry
        .identities(["frame_boundary", "frame_completed"])
        .unwrap();
    assert_eq!(identities.len(), 2);
    assert_eq!(
        identities[0].contract_sha256,
        "498fcd52f2fa2327e0af9e9730b4314f0854a6047f57dcde16961b8a4ecb80cd"
    );
    assert_eq!(
        identities[1].contract_sha256,
        "a335a785a0c109cc7edc6ecab27ff429e386c2ad2eb34769cac4f9cc47378b91"
    );
    assert_eq!(registry.revision().len(), 64);
}

#[test]
fn semantic_contracts_are_platform_local_and_declaratively_typed() {
    let registry = EventContractRegistry::builtin().unwrap();
    let contract = registry.get("snes_ppu_obj_handoff").unwrap();
    assert_eq!(contract.clock_domain, "snes_master");
    assert!(contract
        .payload_fields
        .iter()
        .any(|field| field.path == "cpu.pc"));
    assert!(contract
        .payload_fields
        .iter()
        .any(|field| field.path == "ppu.scanline"));
}

#[test]
fn registry_rejects_a_tampered_contract() {
    let json = include_str!("../contracts/event-classes.json").replace(
        "frame_boundary/v1;clock=frame:strict;frame=tick;payload=empty-object",
        "tampered",
    );
    assert!(matches!(
        EventContractRegistry::parse(&json),
        Err(EventContractError::DigestMismatch { .. })
    ));
}

#[test]
fn identity_requires_the_exact_registered_digest() {
    let registry = EventContractRegistry::builtin().unwrap();
    let error = registry
        .validate_identity(&EventClassIdentity {
            id: "frame_boundary".into(),
            contract_sha256: "00".repeat(32),
        })
        .unwrap_err();
    assert!(matches!(error, EventContractError::DigestMismatch { .. }));
}

#[test]
fn registry_rejects_unknown_class_discovery() {
    let registry = EventContractRegistry::builtin().unwrap();
    assert!(matches!(
        registry.identities(["consumer_specific_event"]),
        Err(EventContractError::UnknownClass(_))
    ));
}

#[test]
fn registry_rejects_open_or_invalid_payload_contracts() {
    let json = include_str!("../contracts/event-classes.json").replace(
        "\"payload_fields\": []",
        "\"payload_fields\": [{\"path\":\"bad..path\",\"type\":\"u64\"}]",
    );
    assert!(matches!(
        EventContractRegistry::parse(&json),
        Err(EventContractError::InvalidPayloadContract { .. })
    ));
}
