use super::protocol::*;

#[test]
fn request_roundtrips_as_ndjson() {
    let req = Request::new(
        7,
        "read_memory",
        serde_json::json!({ "address": 0, "length": 16 }),
    );
    let line = to_line(&req);
    assert!(line.ends_with('\n'), "NDJSON 한 줄은 개행으로 끝나야 한다");
    let back: Request = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(back, req);
    assert_eq!(back.v, PROTOCOL_VERSION);
}

#[test]
fn parses_ok_response() {
    let r = parse_response(r#"{ "id": 7, "ok": true, "result": { "hex": "00ff" } }"#).unwrap();
    assert!(r.ok);
    assert_eq!(r.result.unwrap()["hex"], "00ff");
    assert!(r.error.is_none());
}

#[test]
fn parses_error_response() {
    let r = parse_response(
        r#"{ "id": 7, "ok": false, "error": { "kind": "bad_params", "message": "x" } }"#,
    )
    .unwrap();
    assert!(!r.ok);
    assert_eq!(r.error.unwrap().kind, "bad_params");
}

#[test]
fn result_status_defaults_to_completed() {
    assert_eq!(result_status(&serde_json::json!({})), "completed");
    assert_eq!(
        result_status(&serde_json::json!({"status":"working"})),
        "working"
    );
    assert_eq!(
        result_status(&serde_json::json!({"status":"interrupted"})),
        "interrupted"
    );
}
