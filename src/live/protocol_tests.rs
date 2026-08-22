use super::protocol::*;
use std::collections::VecDeque;
use std::io::{self, BufReader, Cursor, Read};

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

#[test]
fn bounded_reader_rejects_payload_past_limit_before_growing_pending() {
    let mut reader = BufReader::new(Cursor::new(b"12345\n"));
    let mut pending = Vec::new();
    let error = read_ndjson_frame_with_limit(&mut reader, &mut pending, 4).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(pending.len() <= 4);
}

struct ScriptedRead {
    steps: VecDeque<io::Result<Vec<u8>>>,
}

impl Read for ScriptedRead {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self.steps.pop_front() {
            Some(Ok(bytes)) => {
                assert!(bytes.len() <= output.len());
                output[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            Some(Err(error)) => Err(error),
            None => Ok(0),
        }
    }
}

#[test]
fn bounded_reader_preserves_partial_frame_across_timeout() {
    let source = ScriptedRead {
        steps: VecDeque::from([
            Ok(br#"{"id":1,"#.to_vec()),
            Err(io::Error::new(io::ErrorKind::TimedOut, "test timeout")),
            Ok(br#""ok":true}"#.iter().copied().chain(*b"\n").collect()),
        ]),
    };
    let mut reader = BufReader::new(source);
    let mut pending = Vec::new();

    let timeout = read_ndjson_frame_with_limit(&mut reader, &mut pending, 64).unwrap_err();
    assert_eq!(timeout.kind(), io::ErrorKind::TimedOut);
    assert_eq!(pending, br#"{"id":1,"#);

    let frame = read_ndjson_frame_with_limit(&mut reader, &mut pending, 64)
        .unwrap()
        .unwrap();
    assert_eq!(frame, "{\"id\":1,\"ok\":true}\n");
    assert!(pending.is_empty());
}

#[test]
fn bounded_reader_rejects_eof_truncated_frame() {
    let mut reader = BufReader::new(Cursor::new(br#"{"id":1"#));
    let mut pending = Vec::new();
    let error = read_ndjson_frame_with_limit(&mut reader, &mut pending, 64).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}
