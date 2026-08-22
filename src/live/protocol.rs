use std::io::{self, BufRead};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
/// Maximum NDJSON payload size, excluding the terminating newline. Native adapters already cap
/// their transmit buffers at 8 MiB; every Rust transport hop enforces the same bound before
/// allocating further.
pub const MAX_NDJSON_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Raw PNG bytes that still leave room for base64 expansion and response metadata
/// inside one protocol frame.
pub const MAX_INLINE_SCREENSHOT_BYTES: u64 = ((MAX_NDJSON_FRAME_BYTES - 64 * 1024) * 3 / 4) as u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            method: method.to_string(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub kind: String,
    #[serde(default)]
    pub message: String,
}

pub fn to_line(req: &Request) -> String {
    let mut s = serde_json::to_string(req).expect("serialize request");
    s.push('\n');
    s
}

pub fn parse_response(line: &str) -> Result<Response, serde_json::Error> {
    serde_json::from_str(line)
}

/// Read one newline-terminated UTF-8 NDJSON frame while preserving bytes already received across
/// read timeouts. The caller owns `pending` for the lifetime of the connection. An oversized,
/// non-UTF-8, or EOF-truncated frame is `InvalidData`/`UnexpectedEof` and must poison that
/// connection; continuing would make frame boundaries ambiguous.
pub fn read_ndjson_frame<R: BufRead>(
    reader: &mut R,
    pending: &mut Vec<u8>,
) -> io::Result<Option<String>> {
    read_ndjson_frame_with_limit(reader, pending, MAX_NDJSON_FRAME_BYTES)
}

pub(crate) fn read_ndjson_frame_with_limit<R: BufRead>(
    reader: &mut R,
    pending: &mut Vec<u8>,
    max_payload_bytes: usize,
) -> io::Result<Option<String>> {
    loop {
        let (consumed, complete) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if pending.is_empty() {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated NDJSON frame before newline",
                ));
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let payload_part = newline.unwrap_or(available.len());
            if pending.len().saturating_add(payload_part) > max_payload_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("NDJSON frame exceeds {max_payload_bytes} byte payload limit"),
                ));
            }
            let consumed = newline.map_or(available.len(), |index| index + 1);
            pending.extend_from_slice(&available[..consumed]);
            (consumed, newline.is_some())
        };
        reader.consume(consumed);

        if complete {
            let bytes = std::mem::take(pending);
            return String::from_utf8(bytes).map(Some).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("NDJSON frame is not UTF-8: {error}"),
                )
            });
        }
    }
}

pub const STATUS_WORKING: &str = "working";
pub const STATUS_INTERRUPTED: &str = "interrupted";
pub const STATUS_COMPLETED: &str = "completed";

/// 지연 명령 응답의 result에서 `status`를 읽는다. 없으면 "completed".
pub fn result_status(result: &Value) -> &str {
    result
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or(STATUS_COMPLETED)
}
