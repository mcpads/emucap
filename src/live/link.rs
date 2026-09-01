use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::contracts::ContractAdvertisement;

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("emulator not connected")]
    NotConnected,
    /// 포트를 다른 emucap-mcp 인스턴스(다른 세션)가 이미 점유 중. 이 세션은 그 에뮬레이터를
    /// 쓸 수 없다. 세션마다 `EMUCAP_PORT`를 다르게 두고 에뮬레이터도 그 포트로 띄워야 격리된다.
    #[error("port {addr} is busy; another emucap-mcp instance owns it (use a distinct EMUCAP_PORT per session)")]
    PortBusy { addr: String },
    /// 에뮬레이터가 이미 살아있는 다른 세션에 attach됨(broker 모드).
    #[error("emulator is busy; another session controls it")]
    Busy,
    /// 지정한 name의 에뮬레이터가 broker에 없음.
    #[error("no such emulator (available: {names:?})")]
    NoSuchEmulator { names: Vec<String> },
    /// 이름 없이 attach인데 에뮬레이터가 다중.
    #[error("ambiguous emulator; specify a name (available: {names:?})")]
    Ambiguous { names: Vec<String> },
    #[error("request timed out")]
    Timeout,
    #[error("request was cancelled")]
    Cancelled,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("emulator error [{kind}]: {message}")]
    Emulator { kind: String, message: String },
    #[error("emulator identity mismatch; the connected adapter did not present this control session's token")]
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
            Self::Cancelled => "cancelled",
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
    /// Native emulator-host capabilities carried by hello. For Mesen this is the authoritative
    /// runtime capability check; the build sidecar is only a preflight check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_features: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesen_host_api: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_build: Option<Value>,
    /// Generation-bound debugger selection metadata from hello. These fields are carried beside
    /// the legacy identity payload for compatibility, but status exposes them as top-level
    /// capabilities rather than serializing them inside emulator_identity.
    #[serde(skip)]
    pub state_groups: Vec<String>,
    #[serde(skip)]
    pub cpu_targets: Vec<crate::debug_selection::CpuTarget>,
}

impl EmulatorIdentity {
    pub fn from_hello(v: &Value) -> Self {
        let (state_groups, cpu_targets) =
            crate::debug_selection::parse_debug_capabilities(v).unwrap_or_default();
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
            host_features: v
                .get("host_features")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default(),
            mesen_host_api: v
                .get("mesen_host_api")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            host_build: v.get("host_build").cloned(),
            state_groups,
            cpu_targets,
        }
    }

    pub fn has_mesen_native_halt(&self) -> bool {
        self.mesen_host_api.unwrap_or(0) >= 1
            && self.host_features.iter().any(|v| v == "code_break_idle")
            && self
                .host_features
                .iter()
                .any(|v| v == "native_halt_service")
    }

    pub fn has_mesen_native_halt_savestate(&self) -> bool {
        self.mesen_host_api.unwrap_or(0) >= 2
            && self
                .host_features
                .iter()
                .any(|v| v == "native_halt_savestate")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    pub protocol_version: u32,
    pub methods: Vec<String>,
    /// read/write_memory에 유효한 memory_type 이름들 — 어댑터가 hello로 advertise(에뮬레이터의
    /// debugger address space에서; 없으면 빈 vec). MCP는 status에 표면화만 한다(정적 맵 금지).
    pub memory_types: Vec<String>,
    /// Optional exact, generation-bound sizes for live memory types. Adapters that cannot prove a
    /// finite region keep this empty; callers must not infer sizes from platform tables.
    pub memory_regions: Vec<MemoryRegion>,
    /// set_breakpoint가 현재 연결에서 실제로 받는 kind별 구조화된 설명. 플랫폼별 이름과
    /// 범위 의미를 공통 MCP 서버에 정적으로 복제하지 않고 hello 값을 그대로 전달한다.
    pub breakpoint_kinds: Vec<Value>,
    pub contracts: ContractAdvertisement,
    /// Parsed and validated at hello. Public status advertises it only when the link is direct and
    /// bound to the exact managed generation and lease.
    pub recording: Option<super::recording_capability::RecordingCapability>,
    pub identity: EmulatorIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRegion {
    pub memory_type: String,
    pub size: u64,
}

pub(crate) fn memory_regions_from_hello(
    value: Option<&Value>,
    memory_types: &[String],
) -> Result<Vec<MemoryRegion>, LinkError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or_else(|| {
        LinkError::Protocol("memory_regions must be an array when advertised".into())
    })?;
    let advertised = memory_types
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut regions = Vec::with_capacity(values.len());
    for value in values {
        let region: MemoryRegion = serde_json::from_value(value.clone()).map_err(|error| {
            LinkError::Protocol(format!("invalid memory_regions entry: {error}"))
        })?;
        if region.memory_type.is_empty() || region.size == 0 {
            return Err(LinkError::Protocol(
                "memory_regions entries require a non-empty memory_type and non-zero size".into(),
            ));
        }
        if !advertised.contains(region.memory_type.as_str()) {
            return Err(LinkError::Protocol(format!(
                "memory region {} is not present in memory_types",
                region.memory_type
            )));
        }
        if !seen.insert(region.memory_type.clone()) {
            return Err(LinkError::Protocol(format!(
                "memory region {} is advertised more than once",
                region.memory_type
            )));
        }
        regions.push(region);
    }
    Ok(regions)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkingProgress {
    pub status: String,
    pub capture_id: String,
    pub sequence: u64,
    pub frame: u64,
    #[serde(default)]
    pub frames: Option<u64>,
    pub events: u64,
    pub bytes: u64,
    #[serde(default)]
    pub phase: Option<WorkingProgressPhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkingProgressPhase {
    Warming,
    Aligning,
    Recording,
}

impl WorkingProgress {
    pub fn parse(value: Value) -> Result<Self, LinkError> {
        let progress: Self = serde_json::from_value(value)
            .map_err(|error| LinkError::Protocol(format!("invalid working progress: {error}")))?;
        if progress.status != super::protocol::STATUS_WORKING
            || progress.capture_id.is_empty()
            || progress.capture_id.len() > 96
        {
            return Err(LinkError::Protocol(
                "invalid recording working progress identity".into(),
            ));
        }
        Ok(progress)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RequestCancellation(Arc<AtomicBool>);

impl RequestCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone)]
pub struct AbortRequest {
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ProgressCallControl {
    pub cancellation: RequestCancellation,
    pub abort: Option<AbortRequest>,
    /// Host deadline through the guest-affecting adapter terminal. Progress never extends it.
    pub max_host_ms: Option<u64>,
}

pub type ProgressObserver<'a> = dyn FnMut(&WorkingProgress) -> Result<(), LinkError> + Send + 'a;

impl Capabilities {
    pub fn empty() -> Self {
        Self {
            protocol_version: 0,
            methods: vec![],
            memory_types: vec![],
            memory_regions: vec![],
            breakpoint_kinds: vec![],
            contracts: ContractAdvertisement::Unreported,
            recording: None,
            identity: EmulatorIdentity::default(),
        }
    }
}

pub trait EmulatorLink {
    fn capabilities(&self) -> &Capabilities;
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError>;
    /// Observe typed `working` frames and bind cancellation to this exact request. Ordinary calls
    /// retain their existing one-response behavior through `call`.
    fn call_with_progress(
        &mut self,
        method: &str,
        params: Value,
        observer: &mut ProgressObserver<'_>,
        control: &ProgressCallControl,
    ) -> Result<Value, LinkError> {
        if control.cancellation.is_cancelled() {
            return Err(LinkError::Cancelled);
        }
        let _ = observer;
        self.call(method, params)
    }
    /// Whether the link can discard a failed front-side session and attach the same control
    /// session again. Temporal cleanup uses this only for idempotent compensation calls after an
    /// ambiguous transport failure and verifies the launch generation separately.
    fn supports_session_reconnect(&self) -> bool {
        false
    }
    /// Discard the current front-side session after an adapter has acknowledged an operation that
    /// recreates its transport. The emulator process and launch generation remain intact.
    fn prepare_reconnect(&mut self) {}
    /// Explicitly bind this control process to one exact returned managed generation. Automatic
    /// reconnect remains a separate same-control-session path; implementations that do not own a
    /// direct runtime location reject this operation.
    fn reattach_runtime(&mut self, _expected_launch_id: &str) -> Result<Value, LinkError> {
        Err(LinkError::Emulator {
            kind: "unsupported".into(),
            message: "this link does not support explicit managed runtime reattachment".into(),
        })
    }
    /// Whether an unmanaged live connection is protected by one exclusive application control
    /// session. Managed generations use their durable lease instead. Shared or unknown links must
    /// keep the default false so mutation cannot pass merely because no capsule port is visible.
    fn has_exclusive_control(&self) -> bool {
        false
    }
    /// Direct-mode search starting point. This is not a listener endpoint and must never be passed
    /// to an emulator as though it had already been reserved.
    fn base_port(&self) -> Option<u16> {
        None
    }
    /// Actual selected port that an emulator connects to in direct mode. Status uses this to tell
    /// callers the assigned endpoint. Returns `None` before binding and in broker mode.
    fn endpoint_port(&self) -> Option<u16> {
        None
    }
    /// 이 MCP 세션의 direct-mode guard token. status가 launcher env로 안내한다.
    fn session_token(&self) -> Option<&str> {
        None
    }
    /// Stage a launch-generation capability without replacing the active generation's token.
    /// Direct listeners may accept this token while launch readiness is being checked, but must
    /// leave the durable compatibility token unchanged until commit.
    fn stage_reclaim_token(&mut self, _token: &str) -> Result<bool, LinkError> {
        Ok(false)
    }
    /// Promote the exact staged capability after adapter readiness succeeds. Implementations must
    /// reject a token that is not the currently staged value.
    fn commit_staged_reclaim_token(&mut self, _token: &str) -> Result<bool, LinkError> {
        Ok(false)
    }
    /// Discard the exact staged capability after launch failure and restore acceptance of the
    /// previous active token. This operation is idempotent for an already-aborted token.
    fn abort_staged_reclaim_token(&mut self, _token: &str) -> Result<bool, LinkError> {
        Ok(false)
    }
    /// 새 launch generation의 서버 외부에 노출하지 않는 reclaim capability를 direct listener에 설치한다.
    /// broker 등 capability를 직접 소유하지 않는 링크는 false로 강등한다.
    fn replace_reclaim_token(&mut self, _token: &str) -> Result<bool, LinkError> {
        Ok(false)
    }
    /// Atomically acquire the current runtime generation's control lease when the concrete link
    /// owns a durable runtime capsule. The default is observation-only and cannot promote an
    /// available lease to held.
    fn acquire_control_lease(
        &mut self,
        _expected_launch_id: &str,
    ) -> Result<super::runtime::LeaseView, LinkError> {
        Ok(self.continuity().lease)
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
    /// Bounded public inventory of live managed generations in the configured direct listener
    /// range. This is discovery only: returning an entry never grants control or exposes a private
    /// reclaim capability.
    fn runtime_reservations(&self) -> Vec<Value> {
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
                memory_regions: vec![],
                breakpoint_kinds: vec![],
                contracts: ContractAdvertisement::Unreported,
                recording: None,
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
                memory_regions: vec![],
                breakpoint_kinds: vec![],
                contracts: ContractAdvertisement::Unreported,
                recording: None,
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
            Err(LinkError::Cancelled) => Err(LinkError::Cancelled),
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
