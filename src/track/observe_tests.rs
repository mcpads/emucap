use super::observe::*;
use crate::live::link::{Capabilities, EmulatorIdentity, EmulatorLink, LinkError};
use serde_json::{json, Value};

/// 메서드별 고정 응답을 돌려주는 관측용 목.
struct ObsLink {
    caps: Capabilities,
    responses: std::collections::HashMap<String, Value>,
}
impl ObsLink {
    fn new(methods: &[&str]) -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: methods.iter().map(|m| (*m).to_string()).collect(),
                memory_types: vec![],
                breakpoint_kinds: vec![],
                contracts: crate::contracts::ContractAdvertisement::Unreported,
                identity: EmulatorIdentity::default(),
            },
            responses: std::collections::HashMap::new(),
        }
    }
    fn resp(mut self, method: &str, v: Value) -> Self {
        self.responses.insert(method.into(), v);
        self
    }
}
impl EmulatorLink for ObsLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn call(&mut self, method: &str, _p: Value) -> Result<Value, LinkError> {
        self.responses
            .get(method)
            .cloned()
            .ok_or_else(|| LinkError::Protocol(format!("no resp: {method}")))
    }
}

#[test]
fn memory_observe_is_deterministic_and_typed() {
    let mut link = ObsLink::new(&["read_memory"]).resp("read_memory", json!({"hex": "0a0b0c"}));
    let a = observe_hash(
        &mut link,
        &ObserveSpec::Memory {
            memory_type: "wram".into(),
            address: 0,
            length: 3,
        },
    )
    .unwrap();
    let mut link2 = ObsLink::new(&["read_memory"]).resp("read_memory", json!({"hex": "0a0b0c"}));
    let b = observe_hash(
        &mut link2,
        &ObserveSpec::Memory {
            memory_type: "wram".into(),
            address: 0,
            length: 3,
        },
    )
    .unwrap();
    assert_eq!(a.kind_used, "memory");
    assert_eq!(a.byte_len, 3);
    assert_eq!(a.sha256, b.sha256); // 결정적
}

#[test]
fn truncated_memory_read_is_rejected() {
    // 부분 읽기(truncated=true)를 성공으로 받으면 prefix만 해시해 거짓 pass/fail이 난다 — 거부해야.
    let mut link = ObsLink::new(&["read_memory"])
        .resp("read_memory", json!({"hex": "0a0b", "truncated": true}));
    let r = observe_hash(
        &mut link,
        &ObserveSpec::Memory {
            memory_type: "wram".into(),
            address: 0,
            length: 200_000,
        },
    );
    assert!(r.is_err(), "truncated 읽기는 검증 관측에서 거부해야: {r:?}");
}

#[test]
fn different_bytes_differ() {
    let mut x = ObsLink::new(&["read_memory"]).resp("read_memory", json!({"hex": "0a0b"}));
    let mut y = ObsLink::new(&["read_memory"]).resp("read_memory", json!({"hex": "0a0c"}));
    let hx = observe_hash(
        &mut x,
        &ObserveSpec::Memory {
            memory_type: "w".into(),
            address: 0,
            length: 2,
        },
    )
    .unwrap();
    let hy = observe_hash(
        &mut y,
        &ObserveSpec::Memory {
            memory_type: "w".into(),
            address: 0,
            length: 2,
        },
    )
    .unwrap();
    assert_ne!(hx.sha256, hy.sha256);
}

#[test]
fn canonical_json_sorts_keys_recursively() {
    // 중첩 객체·배열까지 키 정렬한 정규형 출력을 정확히 고정한다(format+재귀 검증).
    let v = serde_json::json!({"b": {"d": 4, "c": 3}, "a": [ {"y": 2, "x": 1} ]});
    assert_eq!(
        canonical_json(&v),
        r#"{"a":[{"x":1,"y":2}],"b":{"c":3,"d":4}}"#
    );
}

#[test]
fn state_observe_is_deterministic() {
    let mut a =
        ObsLink::new(&["get_state"]).resp("get_state", json!({"state": {"a": 1, "b": {"c": 2}}}));
    let mut b =
        ObsLink::new(&["get_state"]).resp("get_state", json!({"state": {"a": 1, "b": {"c": 2}}}));
    let ha = observe_hash(&mut a, &ObserveSpec::State).unwrap();
    let hb = observe_hash(&mut b, &ObserveSpec::State).unwrap();
    assert_eq!(ha.kind_used, "state");
    assert_eq!(ha.sha256, hb.sha256);
}

#[test]
fn bad_hex_is_decode_error() {
    let mut link = ObsLink::new(&["read_memory"]).resp("read_memory", json!({"hex": "zzzz"}));
    assert!(matches!(
        observe_hash(
            &mut link,
            &ObserveSpec::Memory {
                memory_type: "w".into(),
                address: 0,
                length: 2
            }
        ),
        Err(ObserveError::Decode(_))
    ));
}

#[test]
fn bad_base64_is_decode_error() {
    let mut link = ObsLink::new(&["screenshot"]).resp("screenshot", json!({"png_base64": "!!!!"}));
    assert!(matches!(
        observe_hash(&mut link, &ObserveSpec::Screenshot),
        Err(ObserveError::Decode(_))
    ));
}

#[test]
fn auto_prefers_screenshot_then_state_then_unsupported() {
    // base64 "AAA=" = bytes [0,0]
    let mut shot = ObsLink::new(&["screenshot", "get_state"])
        .resp("screenshot", json!({"png_base64": "AAA="}));
    assert_eq!(
        observe_hash(&mut shot, &ObserveSpec::Auto)
            .unwrap()
            .kind_used,
        "screenshot"
    );

    let mut st = ObsLink::new(&["get_state"]).resp("get_state", json!({"state": {"x": 1}}));
    assert_eq!(
        observe_hash(&mut st, &ObserveSpec::Auto).unwrap().kind_used,
        "state"
    );

    let mut none = ObsLink::new(&["read_memory"]);
    assert!(matches!(
        observe_hash(&mut none, &ObserveSpec::Auto),
        Err(ObserveError::Unsupported(_))
    ));
}

#[test]
fn missing_capability_is_unsupported() {
    let mut link = ObsLink::new(&["status"]);
    assert!(matches!(
        observe_hash(&mut link, &ObserveSpec::Screenshot),
        Err(ObserveError::Unsupported(_))
    ));
}
