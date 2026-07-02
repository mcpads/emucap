use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

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
    let mut s = serde_json::to_string(req).expect("요청 직렬화");
    s.push('\n');
    s
}

pub fn parse_response(line: &str) -> Result<Response, serde_json::Error> {
    serde_json::from_str(line)
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
