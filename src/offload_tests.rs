use super::*;

#[test]
fn offload_writes_file_and_returns_summary() {
    let dir = tempfile::TempDir::new().unwrap();
    let p = dir.path().join("out.json");
    let v = serde_json::json!({"rows": [{"a":1},{"a":2},{"a":3},{"a":4}]});
    let s = offload_result(&v, &p).unwrap();
    assert_eq!(s["output_path"], serde_json::json!(p.display().to_string()));
    assert!(s["bytes"].as_u64().unwrap() > 0);
    assert_eq!(s["count"], serde_json::json!(4)); // 첫 배열 필드 길이
    assert_eq!(s["head"].as_array().unwrap().len(), 3); // head=첫 3
                                                        // 파일에 전체가 보존
    let back: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(back, v);
}

#[test]
fn offload_top_level_array() {
    let dir = tempfile::TempDir::new().unwrap();
    let p = dir.path().join("arr.json");
    let v = serde_json::json!([1, 2, 3, 4, 5]);
    let s = offload_result(&v, &p).unwrap();
    assert_eq!(s["count"], serde_json::json!(5));
    assert_eq!(s["head"].as_array().unwrap().len(), 3);
    assert!(s["offloaded"].as_bool().unwrap());
}

#[test]
fn offload_creates_parent_dirs() {
    let dir = tempfile::TempDir::new().unwrap();
    let p = dir.path().join("a/b/c/out.json");
    let v = serde_json::json!({"x": 1});
    let s = offload_result(&v, &p).unwrap();
    assert!(p.exists());
    assert_eq!(s["output_path"], serde_json::json!(p.display().to_string()));
}

#[test]
fn offload_no_array_no_count() {
    let dir = tempfile::TempDir::new().unwrap();
    let p = dir.path().join("scalar.json");
    let v = serde_json::json!({"x": 42, "y": "hello"});
    let s = offload_result(&v, &p).unwrap();
    assert!(s.get("count").is_none());
    assert!(s.get("head").is_none());
    assert!(s["bytes"].as_u64().unwrap() > 0);
}

#[cfg(unix)]
#[test]
fn offload_rejects_a_destination_symlink() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::TempDir::new().unwrap();
    let outside = dir.path().join("outside.json");
    std::fs::write(&outside, "original").unwrap();
    let output = dir.path().join("output.json");
    symlink(&outside, &output).unwrap();
    assert!(offload_result(&serde_json::json!({"x": 1}), &output).is_err());
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "original");
}
