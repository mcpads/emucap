use serde_json::{json, Value};

use super::*;

fn range(address: u64, length: u64) -> BatchRange {
    BatchRange {
        memory_type: "ram".into(),
        address,
        length,
    }
}

#[test]
fn features_gate_batch_and_pacing_separately() {
    let features = parse_features("peek_block,throttle_wait_hook");
    assert!(supports_batch(&features));
    assert!(!supports_pacing(&features));
    assert!(supports_pacing(&parse_features(
        "throttle_wait_hook,fastforward_readback"
    )));
    assert!(parse_features("").is_empty());
}

#[test]
fn peek_reply_must_carry_every_requested_byte() {
    let ranges = [range(0x10, 2), range(0, 1)];
    assert_eq!(peek_spec(&[(0xa8010, 2), (0, 1)]), "a8010:2,0:1");
    let reply = parse_peek_reply("OK|4@12.000000000000000000|700|abcd,ff", &ranges).unwrap();
    assert_eq!(
        (reply.epoch.as_str(), reply.frame),
        ("4@12.000000000000000000", 700)
    );
    let value = batch_reply(&ranges, reply, "launch-1".into());
    assert!(crate::live::memory_batch::verify_reply(&ranges, &value, Some("launch-1")).is_ok());
    for bad in [
        "OK|4@1|700|abcd,ff,00",
        "OK|4@1|700|ff,abcd",
        "OK|4@1|700|abcd,ffff",
        "OK|4@1|700|abcd,ff|extra",
        "OK|4@1|-1|abcd,ff",
        "OK|4@1|700|abcd",
        "OK|4@1|700|abcd,f",
        "OK|4@1|700|abcd,zz",
        "OK|4@1|x|abcd,ff",
        "OK|4|700|abcd,ff",
        "E1C",
    ] {
        assert!(parse_peek_reply(bad, &ranges).is_none(), "{bad}");
    }
}

#[test]
fn native_governor_maps_to_the_common_policy() {
    let capability = speed_capability();
    let policy = |raw: &str| MamePacing::parse(raw).unwrap().public(&capability);
    let limited = policy("3|1|500|1.0|0|0|0.016667");
    assert_eq!(
        (limited["mode"].clone(), limited["percent"].clone()),
        (json!("limited"), json!(50))
    );
    assert_eq!(policy("3|0|500|1.0|0|0|0.0167")["mode"], "unlimited");
    assert_eq!(policy("3|0|500|1.0|0|0|0.0167")["percent"], Value::Null);
    for custom in [
        "3|1|1000|1.0|1|0|0.0167",
        "3|1|1000|1.0|0|1|0.0167",
        "3|1|1000|2.0|0|0|0.0167",
    ] {
        assert_eq!(policy(custom)["mode"], "custom", "{custom}");
    }
    assert_eq!(policy("3|1|1|1.0|0|0|0.0167")["percent"], json!(0.1));
    for bad in [
        "3|1|500|1.0|0|0",
        "x|1|500|1.0|0|0|0.0",
        "3|2|500|1.0|0|0|0.0",
        "3|1|500|nan|0|0|0.0",
    ] {
        assert!(MamePacing::parse(bad).is_none(), "{bad}");
    }
    for value in [policy("1|1|500|1.0|0|0|0.0"), policy("1|0|500|1.0|1|0|0.0")] {
        assert!(capability.verify_policy(&value).is_ok(), "{value}");
    }
}

#[test]
fn requests_encode_per_mille_and_transactions_decode_boundaries() {
    assert_eq!(
        set_spec(SpeedRequest::Limited {
            centi_percent: 5000
        })
        .unwrap(),
        "limited|500"
    );
    assert_eq!(
        set_spec(SpeedRequest::Limited { centi_percent: 10 }).unwrap(),
        "limited|1"
    );
    assert_eq!(set_spec(SpeedRequest::Unlimited).unwrap(), "unlimited");
    let reply =
        parse_set_reply("1|1|1000|1.0|0|0|0.02;2|1|500|1.0|0|0|0.02;3@1.0|9;3@1.0|9").unwrap();
    assert!(reply.boundary_kept);
    assert_eq!(reply.applied.speed_per_mille, 500);
    assert_eq!(reply.previous.restore_spec(2), "2|1|1000|1.0");
    assert!(
        !parse_set_reply("1|1|1000|1.0|0|0|0.02;2|1|500|1.0|0|0|0.02;3@1.0|9;4@1.0|9")
            .unwrap()
            .boundary_kept
    );
    assert_eq!(deadline_frames("DEADLINE:12"), Some(12));
    assert_eq!(deadline_frames("OK"), None);
}

#[test]
fn slow_policies_scale_the_frame_estimate() {
    let at = |raw: &str| MamePacing::parse(raw).unwrap().frame_budget_ms(50);
    assert_eq!(at("1|1|1000|1.0|0|0|0.02"), 70);
    assert_eq!(at("1|1|10|1.0|0|0|0.02"), 2050);
    assert_eq!(at("1|0|10|1.0|0|0|0.02"), 50);
}
