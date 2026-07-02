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
    assert!(format!("{}", LinkError::Busy).contains("다른 세션"));
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
