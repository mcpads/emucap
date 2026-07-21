use serde_json::json;

use super::*;

#[test]
fn rdram_access_is_offset_based_and_fail_loud_at_the_boundary() {
    assert_eq!(
        rdram_address(&json!({"memory_type":"rdram", "address":0}), 1).unwrap(),
        RDRAM_BASE
    );
    assert!(matches!(
        rdram_address(&json!({"memory_type":"rdram", "address":RDRAM_SIZE - 1}), 2),
        Err(N64Error::BadParams(_))
    ));
}

#[test]
fn rdram_access_rejects_unknown_memory_types() {
    assert!(matches!(
        rdram_address(&json!({"memory_type":"rom", "address":0}), 1),
        Err(N64Error::BadParams(_))
    ));
}

#[test]
fn execution_cpu_is_explicitly_limited_to_r4300() {
    require_r4300(&json!({"cpu":"r4300"})).unwrap();
    require_r4300(&json!({})).unwrap();
    assert!(matches!(
        require_r4300(&json!({"cpu":"rsp"})),
        Err(N64Error::BadParams(_))
    ));
}

#[test]
fn numeric_parameters_accept_decimal_and_prefixed_hex() {
    assert_eq!(parse_num(&json!("0x20")), Some(32));
    assert_eq!(parse_num(&json!("$20")), Some(32));
    assert_eq!(parse_num(&json!(32)), Some(32));
}

#[test]
fn initial_contract_advertisement_validates() {
    let hello = json!({
        "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS)
    });
    let advertisement = crate::contracts::advertisement_from_hello(&hello);
    let methods = METHODS
        .iter()
        .map(|method| (*method).to_string())
        .collect::<Vec<_>>();
    let status = crate::contracts::validate_advertisement(
        &advertisement,
        Some("mupen64plus-native"),
        Some("n64"),
        &methods,
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
}
