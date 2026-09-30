use serde_json::{json, Value};

use super::*;

fn stepped() -> ExecutionSpeedCapability {
    ExecutionSpeedCapability::from_hello(&json!({
        "modes": ["limited", "unlimited"],
        "percent": {"min": 1, "max": 1000, "quantum": 1},
        "states": ["running", "frozen"], "scope": "host_pacing", "source": "native",
        "control_service_ms": 50, "host_constraints": ["audio_queue"]
    }))
    .unwrap()
}

fn policy(mode: &str, percent: Value) -> Value {
    json!({"mode": mode, "percent": percent, "source": "native", "policy_revision": "r1",
        "host_constraints": []})
}

#[test]
fn wire_percent_normalizes_exactly_or_is_rejected() {
    assert_eq!(centi_percent(50.0), Some(5000));
    assert_eq!(centi_percent(0.01), Some(1));
    assert_eq!(centi_percent(0.1 + 0.2), Some(30));
    assert_eq!(centi_percent(12.34), Some(1234));
    for rejected in [12.345, 0.0, -1.0, 0.001, f64::NAN, f64::INFINITY, 1e12] {
        assert_eq!(centi_percent(rejected), None, "{rejected}");
    }
    assert_eq!(percent_value(5000), json!(50));
    assert_eq!(percent_value(1234), json!(12.34));
}

#[test]
fn request_shapes_are_explicit() {
    assert_eq!(parse_request(None, None), Ok(SpeedRequest::Query));
    assert_eq!(
        parse_request(Some("unlimited"), None),
        Ok(SpeedRequest::Unlimited)
    );
    assert_eq!(
        parse_request(Some("limited"), Some(50.0)),
        Ok(SpeedRequest::Limited {
            centi_percent: 5000
        })
    );
    for (mode, percent) in [
        (Some("unlimited"), Some(100.0)),
        (Some("limited"), None),
        (None, Some(100.0)),
        (Some("turbo"), None),
        (Some("limited"), Some(0.0)),
        (Some("limited"), Some(33.333)),
    ] {
        assert!(
            parse_request(mode, percent).is_err(),
            "{mode:?} {percent:?}"
        );
    }
}

#[test]
fn capability_must_admit_the_common_minimum_targets() {
    let valid = json!({"modes":["limited","unlimited"], "percent":{"values":[50,100,200,400]},
        "states":["running","frozen"], "scope":"host_pacing", "source":"frontend",
        "control_service_ms": 16});
    let cap = ExecutionSpeedCapability::from_hello(&valid).unwrap();
    assert!(cap
        .admit(SpeedRequest::Limited {
            centi_percent: 20000
        })
        .is_ok());
    assert!(cap
        .admit(SpeedRequest::Limited {
            centi_percent: 30000
        })
        .is_err());

    for (field, value) in [
        ("percent", json!({"values":[50,100,200]})),
        ("percent", json!({"values":[400,200,100,50]})),
        ("percent", json!({"min":50,"max":400,"quantum":100})),
        ("percent", json!({"min":1,"max":400})),
        (
            "percent",
            json!({"min":1,"max":400,"quantum":1,"values":[50]}),
        ),
        ("modes", json!(["limited"])),
        ("states", json!(["running"])),
        ("scope", json!("cpu_clock")),
        ("source", json!("broker")),
        ("control_service_ms", json!(0)),
    ] {
        let mut candidate = valid.clone();
        candidate[field] = value;
        assert!(
            ExecutionSpeedCapability::from_hello(&candidate).is_err(),
            "{candidate}"
        );
    }
}

#[test]
fn stepped_domain_rejects_values_off_quantum_or_range() {
    let cap = ExecutionSpeedCapability::from_hello(&json!({"modes":["limited","unlimited"],
        "percent":{"min":25,"max":1600,"quantum":25}, "states":["running","frozen"],
        "scope":"host_pacing", "source":"native", "control_service_ms": 50}))
    .unwrap();
    for (centi, admitted) in [
        (2500, true),
        (160000, true),
        (5000, true),
        (2600, false),
        (162500, false),
        (100, false),
    ] {
        assert_eq!(
            cap.admit(SpeedRequest::Limited {
                centi_percent: centi
            })
            .is_ok(),
            admitted,
            "{centi}"
        );
    }
}

#[test]
fn policy_reports_percent_only_for_limited_targets() {
    let cap = stepped();
    assert!(cap.verify_policy(&policy("limited", json!(50))).is_ok());
    assert!(cap.verify_policy(&policy("unlimited", Value::Null)).is_ok());
    assert!(cap.verify_policy(&policy("custom", Value::Null)).is_ok());
    for bad in [
        policy("limited", Value::Null),
        policy("limited", json!(1000.5)),
        policy("unlimited", json!(100)),
        policy("custom", json!(100)),
        policy("fast", Value::Null),
    ] {
        assert!(cap.verify_policy(&bad).is_err(), "{bad}");
    }
    let mut missing_revision = policy("limited", json!(100));
    missing_revision["policy_revision"] = json!("");
    assert!(cap.verify_policy(&missing_revision).is_err());
    let mut other_source = policy("limited", json!(100));
    other_source["source"] = json!("frontend");
    assert!(cap.verify_policy(&other_source).is_err());
}

#[test]
fn change_reply_must_confirm_the_requested_policy() {
    let cap = stepped();
    let reply = |confirmed: Value| {
        json!({"status":"completed", "state":"running", "previous": policy("limited", json!(100)),
            "execution_speed": confirmed})
    };
    let half = SpeedRequest::Limited {
        centi_percent: 5000,
    };
    assert!(cap
        .verify_change(half, &reply(policy("limited", json!(50))))
        .is_ok());
    assert!(cap
        .verify_change(
            SpeedRequest::Unlimited,
            &reply(policy("unlimited", Value::Null))
        )
        .is_ok());
    assert!(cap
        .verify_change(half, &reply(policy("limited", json!(51))))
        .is_err());
    assert!(cap
        .verify_change(half, &reply(policy("custom", Value::Null)))
        .is_err());
    assert!(cap
        .verify_change(
            SpeedRequest::Unlimited,
            &reply(policy("custom", Value::Null))
        )
        .is_err());
    let mut interrupted = reply(policy("limited", json!(50)));
    interrupted["status"] = json!("failed_restored");
    assert!(cap.verify_change(half, &interrupted).is_err());
}
