//! 결정론 게이트의 관측치 — frozen/원자 시점 상태를 sha256으로 요약한다.
//! "같은 입력 → 같은 실행"을 무엇으로 판정할지(observable)를 추상화한다.
use sha2::{Digest, Sha256};

use crate::live::link::EmulatorLink;

/// 무엇을 관측해 비교할지. Auto는 capability 따라 광역 inline 관측치를 고른다.
#[derive(Debug, Clone, PartialEq)]
pub enum ObserveSpec {
    Auto,
    Memory {
        memory_type: String,
        address: u64,
        length: u64,
    },
    Screenshot,
    State,
}

/// 한 번의 관측 결과.
#[derive(Debug, Clone, PartialEq)]
pub struct ObserveOutcome {
    pub kind_used: String,
    pub sha256: String,
    pub byte_len: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObserveError {
    /// 어댑터가 그 관측에 필요한 메서드를 광고 안 함.
    Unsupported(String),
    /// 링크/프로토콜 에러.
    Link(String),
    /// 응답 디코드 실패(hex/base64/필드 누락).
    Decode(String),
}

impl std::fmt::Display for ObserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObserveError::Unsupported(m) => write!(f, "unsupported: {m}"),
            ObserveError::Link(m) => write!(f, "link: {m}"),
            ObserveError::Decode(m) => write!(f, "decode: {m}"),
        }
    }
}

/// sha256 hex 다이제스트.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn has(link: &dyn EmulatorLink, m: &str) -> bool {
    link.capabilities().methods.iter().any(|x| x == m)
}

/// 현재(호출자가 frozen/원자 보장) 상태의 관측치 해시. Savestate+Memory의 원자 probe는
/// 호출자가 따로 처리하며 여기 안 온다.
pub fn observe_hash(
    link: &mut dyn EmulatorLink,
    spec: &ObserveSpec,
) -> Result<ObserveOutcome, ObserveError> {
    match spec {
        ObserveSpec::Auto => {
            if has(link, "screenshot") {
                observe_hash(link, &ObserveSpec::Screenshot)
            } else if has(link, "get_state") {
                observe_hash(link, &ObserveSpec::State)
            } else {
                Err(ObserveError::Unsupported("screenshot|get_state".into()))
            }
        }
        ObserveSpec::Memory {
            memory_type,
            address,
            length,
        } => {
            if !has(link, "read_memory") {
                return Err(ObserveError::Unsupported("read_memory".into()));
            }
            let r = link
                .call(
                    "read_memory",
                    serde_json::json!({
                        "memory_type": memory_type, "address": address, "length": length
                    }),
                )
                .map_err(|e| ObserveError::Link(e.to_string()))?;
            let hex = r
                .get("hex")
                .and_then(|h| h.as_str())
                .ok_or_else(|| ObserveError::Decode("read_memory 응답에 hex 없음".into()))?;
            let bytes = crate::analysis::bisect::hex_to_bytes(hex).map_err(ObserveError::Decode)?;
            Ok(ObserveOutcome {
                kind_used: "memory".into(),
                sha256: sha256_hex(&bytes),
                byte_len: bytes.len(),
            })
        }
        ObserveSpec::Screenshot => {
            if !has(link, "screenshot") {
                return Err(ObserveError::Unsupported("screenshot".into()));
            }
            let r = link
                .call("screenshot", serde_json::json!({}))
                .map_err(|e| ObserveError::Link(e.to_string()))?;
            let b64 = r
                .get("png_base64")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ObserveError::Decode("screenshot 응답에 png_base64 없음".into()))?;
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .map_err(|e| ObserveError::Decode(format!("base64: {e}")))?;
            Ok(ObserveOutcome {
                kind_used: "screenshot".into(),
                sha256: sha256_hex(&bytes),
                byte_len: bytes.len(),
            })
        }
        ObserveSpec::State => {
            if !has(link, "get_state") {
                return Err(ObserveError::Unsupported("get_state".into()));
            }
            let r = link
                .call("get_state", serde_json::json!({}))
                .map_err(|e| ObserveError::Link(e.to_string()))?;
            // dump_memory와 동일: {"state": ...} 래핑이면 벗기고, 아니면 통째로.
            let state = r.get("state").cloned().unwrap_or(r);
            let canon = canonical_json(&state).into_bytes();
            Ok(ObserveOutcome {
                kind_used: "state".into(),
                sha256: sha256_hex(&canon),
                byte_len: canon.len(),
            })
        }
    }
}

/// JSON을 키 재귀 정렬한 결정적 문자열로. 같은 논리값이면 항상 같은 바이트열.
/// serde_json 기본은 BTreeMap이라 이미 키가 정렬되지만, `preserve_order` feature가 켜지면
/// 입력 키 순서가 보존돼 비결정이 된다 — 이 명시 정렬이 그 경우에도 결정성을 보장한다(defense-in-depth).
pub(crate) fn canonical_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}
