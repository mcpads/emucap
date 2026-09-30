use serde_json::{json, Value};

use super::*;

fn types() -> Vec<String> {
    vec!["ram".into(), "vram".into()]
}

fn capability() -> MemoryBatchCapability {
    MemoryBatchCapability::from_hello(
        &json!({
            "max_ranges": 4, "max_range_bytes": 16, "max_total_bytes": 32,
            "consistency": "frozen_boundary", "halt_kinds": ["debugger_break"],
            "windows": [{"memory_type":"ram","address":0x100,"length":0x100},
                        {"memory_type":"vram","address":0,"length":0x40}]
        }),
        &types(),
    )
    .unwrap()
}

fn range(memory_type: &str, address: u64, length: u64) -> BatchRange {
    BatchRange {
        memory_type: memory_type.into(),
        address,
        length,
    }
}

fn reply(ranges: &[BatchRange]) -> Value {
    let reads: Vec<_> = ranges
        .iter()
        .enumerate()
        .map(|(index, r)| {
            json!({"index":index, "memory_type":r.memory_type, "address":r.address,
            "length":r.length, "hex":"a5".repeat(r.length as usize)})
        })
        .collect();
    json!({"state":"frozen", "consistency":"frozen_boundary",
        "boundary":{"runtime_generation":"launch-1","stop_epoch":"7","memory_mapping_epoch":"3",
            "clocks":[{"domain":"adapter_frame","value":100}]},
        "total_bytes": ranges.iter().map(|r| r.length).sum::<u64>(), "reads": reads})
}

#[test]
fn capability_rejects_limits_beyond_the_common_envelope() {
    let base = json!({"max_ranges": 64, "max_range_bytes": 16, "max_total_bytes": 65536,
        "consistency":"frozen_boundary", "halt_kinds":["x"],
        "windows":[{"memory_type":"ram","address":0,"length":1}]});
    assert!(MemoryBatchCapability::from_hello(&base, &types()).is_ok());
    for (field, value) in [
        ("max_ranges", json!(65)),
        ("max_ranges", json!(1)),
        ("max_total_bytes", json!(65537)),
        ("max_range_bytes", json!(65537)),
        ("consistency", json!("best_effort")),
        ("halt_kinds", json!([])),
        ("windows", json!([])),
        (
            "windows",
            json!([{"memory_type":"cram","address":0,"length":1}]),
        ),
        (
            "windows",
            json!([{"memory_type":"ram","address":u64::MAX,"length":2}]),
        ),
    ] {
        let mut candidate = base.clone();
        candidate[field] = value;
        assert!(
            MemoryBatchCapability::from_hello(&candidate, &types()).is_err(),
            "{field}={} accepted",
            candidate[field]
        );
    }
}

#[test]
fn admission_names_the_first_rejected_range_before_any_read() {
    let cap = capability();
    // Duplicates, overlap and the exact final byte of a window are valid.
    let valid = [
        range("ram", 0x100, 8),
        range("ram", 0x100, 8),
        range("ram", 0x104, 8),
        range("vram", 0x3f, 1),
    ];
    assert!(cap.admit(&valid).is_ok());
    let cases: [(&[BatchRange], &str); 7] = [
        (&[], "1..=4"),
        (
            &[
                range("ram", 0x100, 1),
                range("ram", 0x100, 1),
                range("ram", 0x100, 1),
                range("ram", 0x100, 1),
                range("ram", 0x100, 1),
            ],
            "1..=4",
        ),
        (&[range("ram", 0x100, 1), range("vram", 0x3f, 2)], "range 1"),
        (&[range("ram", 0x100, 0)], "range 0 length"),
        (&[range("ram", 0x100, 17)], "range 0 length"),
        (&[range("ram", u64::MAX, 1)], "range 0"),
        (
            &[
                range("ram", 0x100, 16),
                range("ram", 0x110, 16),
                range("vram", 0, 1),
            ],
            "max_total_bytes",
        ),
    ];
    for (ranges, expected) in cases {
        let error = cap.admit(ranges).unwrap_err();
        assert!(error.contains(expected), "{error} lacks {expected}");
    }
    assert!(cap.admit(&[range("cram", 0, 1)]).is_err());
}

#[test]
fn reply_must_answer_exactly_the_admitted_request() {
    let ranges = [range("ram", 0x100, 2), range("vram", 4, 1)];
    assert!(verify_reply(&ranges, &reply(&ranges), Some("launch-1")).is_ok());
    assert!(verify_reply(&ranges, &reply(&ranges), None).is_ok());

    type Mutation = (&'static str, fn(&mut Value));
    let mutations: [Mutation; 11] = [
        ("generation", |v| {
            v["boundary"]["runtime_generation"] = json!("launch-2")
        }),
        ("stop epoch", |v| v["boundary"]["stop_epoch"] = json!("")),
        ("mapping epoch", |v| {
            v["boundary"]
                .as_object_mut()
                .unwrap()
                .remove("memory_mapping_epoch");
        }),
        ("clock", |v| {
            v["boundary"]["clocks"] = json!([{"domain":"frame"}])
        }),
        ("running", |v| v["state"] = json!("running")),
        ("order", |v| v["reads"].as_array_mut().unwrap().swap(0, 1)),
        ("missing", |v| {
            v["reads"].as_array_mut().unwrap().pop();
        }),
        ("short hex", |v| v["reads"][0]["hex"] = json!("a5")),
        ("odd hex", |v| v["reads"][0]["hex"] = json!("zzzz")),
        ("descriptor", |v| v["reads"][1]["address"] = json!(5)),
        ("total", |v| v["total_bytes"] = json!(4)),
    ];
    for (name, mutate) in mutations {
        let mut value = reply(&ranges);
        mutate(&mut value);
        assert!(
            verify_reply(&ranges, &value, Some("launch-1")).is_err(),
            "{name} accepted"
        );
    }
}
