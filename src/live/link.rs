use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("emulator not connected")]
    NotConnected,
    /// 포트를 다른 emucap-mcp 인스턴스(다른 세션)가 이미 점유 중. 이 세션은 그 에뮬레이터를
    /// 쓸 수 없다. 세션마다 `EMUCAP_PORT`를 다르게 두고 에뮬레이터도 그 포트로 띄워야 격리된다.
    #[error("port {addr} busy — 다른 emucap-mcp 인스턴스가 점유 중(세션별 EMUCAP_PORT 분리 필요)")]
    PortBusy { addr: String },
    /// 에뮬레이터가 이미 살아있는 다른 세션에 attach됨(broker 모드).
    #[error("emulator busy — 다른 세션이 제어 중")]
    Busy,
    /// 지정한 name의 에뮬레이터가 broker에 없음.
    #[error("no such emulator (가용: {names:?})")]
    NoSuchEmulator { names: Vec<String> },
    /// 이름 없이 attach인데 에뮬레이터가 다중.
    #[error("ambiguous emulator — 이름 지정 필요 (가용: {names:?})")]
    Ambiguous { names: Vec<String> },
    #[error("request timed out")]
    Timeout,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("emulator error [{kind}]: {message}")]
    Emulator { kind: String, message: String },
    #[error("emulator identity mismatch — expected session_token {expected}, got {actual:?}; identity={identity:?}")]
    IdentityMismatch {
        expected: String,
        actual: Option<String>,
        identity: Box<EmulatorIdentity>,
    },
}

impl LinkError {
    /// Stable machine-readable category for continuity records. Display text remains free to be
    /// operator-friendly and localized without changing the persisted contract.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotConnected => "not_connected",
            Self::PortBusy { .. } => "port_busy",
            Self::Busy => "busy",
            Self::NoSuchEmulator { .. } => "no_such_emulator",
            Self::Ambiguous { .. } => "ambiguous",
            Self::Timeout => "request_timeout",
            Self::Protocol(_) => "protocol_error",
            Self::Emulator { .. } => "emulator_error",
            Self::IdentityMismatch { .. } => "identity_mismatch",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulatorIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// 어댑터가 빌드된 emucap git hash(hello의 "build"). 운영자가 git HEAD와 대조해 최신 여부 확인.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
}

impl EmulatorIdentity {
    pub fn from_hello(v: &Value) -> Self {
        Self {
            system: v.get("system").and_then(Value::as_str).map(String::from),
            adapter: v.get("adapter").and_then(Value::as_str).map(String::from),
            build: v.get("build").and_then(Value::as_str).map(String::from),
            name: v.get("name").and_then(Value::as_str).map(String::from),
            session_token: v
                .get("session_token")
                .and_then(Value::as_str)
                .map(String::from),
            content: v.get("content").and_then(Value::as_str).map(String::from),
            launch_id: v.get("launch_id").and_then(Value::as_str).map(String::from),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    pub protocol_version: u32,
    pub methods: Vec<String>,
    /// read/write_memory에 유효한 memory_type 이름들 — 어댑터가 hello로 advertise(에뮬레이터의
    /// debugger address space에서; 없으면 빈 vec). MCP는 status에 표면화만 한다(정적 맵 금지).
    pub memory_types: Vec<String>,
    pub identity: EmulatorIdentity,
}

impl Capabilities {
    pub fn empty() -> Self {
        Self {
            protocol_version: 0,
            methods: vec![],
            memory_types: vec![],
            identity: EmulatorIdentity::default(),
        }
    }
}

pub trait EmulatorLink {
    fn capabilities(&self) -> &Capabilities;
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError>;
    /// 직접 모드에서 에뮬레이터가 접속해야 하는 포트(자동 선택 결과). status가 에이전트에게
    /// 알려주려 쓴다. broker 모드 등 포트 개념이 없으면 None.
    fn endpoint_port(&self) -> Option<u16> {
        None
    }
    /// 이 MCP 세션의 direct-mode guard token. status가 launcher env로 안내한다.
    fn session_token(&self) -> Option<&str> {
        None
    }
    /// 새 launch generation의 비공개 reclaim capability를 direct listener에 설치한다.
    /// broker 등 capability를 직접 소유하지 않는 링크는 false로 강등한다.
    fn replace_reclaim_token(&mut self, _token: &str) -> Result<bool, LinkError> {
        Ok(false)
    }
    /// Last host-side observation, available even when the adapter socket is gone.
    fn continuity(&self) -> super::continuity::ContinuitySnapshot {
        super::continuity::ContinuitySnapshot::default()
    }
    /// Durable host/adapter failure context. Implementations must not contact the emulator here.
    fn failure_context(&mut self) -> Value {
        serde_json::json!({
            "continuity": self.continuity(),
            "link_failure": null,
            "adapter_failure": null,
        })
    }
    /// Public live-generation candidates when direct automatic reattachment is ambiguous or held
    /// by a still-live lease. Empty for links without a direct port range.
    fn runtime_candidates(&self) -> Vec<Value> {
        Vec::new()
    }
}

/// 테스트용 링크. 한 번의 응답(성공/에러)을 정해 돌려준다.
pub struct FakeLink {
    caps: Capabilities,
    response: Result<Value, LinkError>,
    pub last_method: Option<String>,
    pub last_params: Option<Value>,
}

impl FakeLink {
    pub fn ok(result: Value) -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec!["read_memory".into()],
                memory_types: vec![],
                identity: EmulatorIdentity::default(),
            },
            response: Ok(result),
            last_method: None,
            last_params: None,
        }
    }

    pub fn err(e: LinkError) -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec![],
                memory_types: vec![],
                identity: EmulatorIdentity::default(),
            },
            response: Err(e),
            last_method: None,
            last_params: None,
        }
    }
}

impl EmulatorLink for FakeLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        self.last_method = Some(method.to_string());
        self.last_params = Some(params);
        match &self.response {
            Ok(v) => Ok(v.clone()),
            Err(LinkError::NotConnected) => Err(LinkError::NotConnected),
            Err(LinkError::PortBusy { addr }) => Err(LinkError::PortBusy { addr: addr.clone() }),
            Err(LinkError::Busy) => Err(LinkError::Busy),
            Err(LinkError::NoSuchEmulator { names }) => Err(LinkError::NoSuchEmulator {
                names: names.clone(),
            }),
            Err(LinkError::Ambiguous { names }) => Err(LinkError::Ambiguous {
                names: names.clone(),
            }),
            Err(LinkError::Timeout) => Err(LinkError::Timeout),
            Err(LinkError::Protocol(s)) => Err(LinkError::Protocol(s.clone())),
            Err(LinkError::Emulator { kind, message }) => Err(LinkError::Emulator {
                kind: kind.clone(),
                message: message.clone(),
            }),
            Err(LinkError::IdentityMismatch {
                expected,
                actual,
                identity,
            }) => Err(LinkError::IdentityMismatch {
                expected: expected.clone(),
                actual: actual.clone(),
                identity: identity.clone(),
            }),
        }
    }
}
