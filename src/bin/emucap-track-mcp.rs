//! emucap 추적 MCP(`emucap-track-mcp`) — 실험 기록 저장 서버.
//!
//! 이 서버는 `.emucap/`에 run을 저장하고 검색하며 에뮬레이터를 제어하지 않는다.
//! rom_sha1과 connection_ref는 제어 MCP의 get_rom_info/status에서 읽어 인자로 넘긴다.
//! 두 MCP는 서로 호출하지 않는다. 도구 동작은 `emucap::track::mcp_ops`에 있고, 여기서는
//! 현재 run 선택과 Value→CallToolResult 변환을 담당한다.

use std::path::Path;
use std::sync::{Arc, Mutex};

use emucap::mcp_result::{error_result, json_result};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

const STATIC_MCP_METADATA_TTL_MS: u64 = 3_600_000;

/// 추적 서버 상태 — link 없음(emulator-less). active_run만 in-memory로 들고, 원장 쓰기는
/// 모두 이 한 프로세스 안에서 직렬화된다(run.json RMW 동시성이 한 프로세스에 갇힘).
#[derive(Clone)]
struct EmucapTrack {
    active_run: Arc<Mutex<Option<ActiveRun>>>,
    tool_router: ToolRouter<EmucapTrack>,
}

/// in-memory 활성 run 바인딩. connection_ref는 제어 MCP에서 받아 넘긴 표식(어느 세션 run인지)일 뿐
/// 이 서버가 연결을 들고 있지 않다 — 자동 도출 없음.
#[derive(Clone)]
struct ActiveRun {
    rom_sha1: String,
    run_id: String,
    connection_ref: Option<String>,
}

// ── 도구 Args ────────────────────────────────────────────────────────────────

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrackRunStartArgs {
    /// Required opaque ROM identifier from Control MCP `get_rom_info.rom_sha1`.
    /// This server does not inspect the emulator or infer the value.
    rom_sha1: String,
    /// Optional session marker: Control MCP `status.emulator_identity.name` or
    /// `"port:" + status.listening_port`. It is used to resume or supersede the
    /// previous unfinished run for that connection.
    #[serde(default)]
    connection_ref: Option<String>,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunResumeArgs {
    /// Globally unique run ID to rebind. Only a stored run with status=running
    /// can be resumed; start a new run after a finished run.
    run_id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunFinishArgs {
    /// done|aborted|error (default: done).
    #[serde(default)]
    status: Option<String>,
    /// Finish a globally unique run by ID. When omitted, finish the active run.
    /// This also supports recovery of orphaned running records after a restart.
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogMetricArgs {
    key: String,
    value: f64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogGateArgs {
    name: String,
    /// machine | judgment.
    kind: String,
    passed: Option<bool>,
    evidence_ref: Option<String>,
    detail: Option<String>,
    case_ref: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogArtifactArgs {
    kind: String,
    /// Path to an existing captured file. Relative paths are resolved from the
    /// working repository root, not the MCP server's current directory.
    path: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetReproArgs {
    base: Option<String>,
    movie_ref: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogFindingArgs {
    /// ROM identifier. When omitted, use the active run's rom_sha1. The call
    /// fails if neither is available.
    rom_sha1: Option<String>,
    claim: String,
    #[serde(default)]
    evidence_refs: Vec<String>,
    #[serde(default)]
    promoted: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogInterventionArgs {
    /// Free-form intervention label such as write_memory, load_state, reset, or
    /// input_burst. Control MCP mutations are not logged automatically.
    op: String,
    /// Structured operation arguments, for example
    /// {memory_type,address,hex} for write_memory. Defaults to null.
    #[serde(default)]
    args: Option<serde_json::Value>,
    /// Optional frame at which the intervention occurred.
    #[serde(default)]
    at_frame: Option<u64>,
    /// Optional reference to the event that triggered the intervention.
    #[serde(default)]
    at_event: Option<String>,
    /// True when the intervention occurred in a frozen context. Default: false.
    #[serde(default)]
    frozen_context: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryRunsArgs {
    rom_sha1: Option<String>,
    goal: Option<String>,
    status: Option<String>,
    /// Write JSON results to this path and return only a summary. Omit to return
    /// the full result inline.
    #[serde(default)]
    output_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetRunArgs {
    rom_sha1: String,
    run_id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompareRunsArgs {
    /// Baseline run ID (A).
    run_id_a: String,
    /// Comparison run ID (B).
    run_id_b: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SummarizeRunsArgs {
    /// Exact goal filter. Omit for no goal restriction.
    #[serde(default)]
    goal: Option<String>,
    /// Exact tag-element filter. Omit for no tag restriction.
    #[serde(default)]
    tag: Option<String>,
    /// ROM identifier filter. Omit for no ROM restriction.
    #[serde(default)]
    rom_sha1: Option<String>,
    /// Write JSON results to this path and return only a summary. Omit to return
    /// the full result inline.
    #[serde(default)]
    output_path: Option<String>,
}

// ── 공통 헬퍼 ────────────────────────────────────────────────────────────────

/// 추적 도구 공통: ok json
fn track_ok(v: serde_json::Value) -> CallToolResult {
    json_result(v)
}

enum TrackReplyError {
    Classified(emucap::track::mcp_ops::TrackingToolError),
    Message(String),
}

impl From<emucap::track::mcp_ops::TrackingToolError> for TrackReplyError {
    fn from(error: emucap::track::mcp_ops::TrackingToolError) -> Self {
        Self::Classified(error)
    }
}

impl From<String> for TrackReplyError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for TrackReplyError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_string())
    }
}

/// Preserve classified tracking failures while keeping local lifecycle messages generic.
fn track_err(error: impl Into<TrackReplyError>) -> CallToolResult {
    match error.into() {
        TrackReplyError::Classified(error) => error_result(error.code(), error),
        TrackReplyError::Message(message) => error_result("tracking_error", message),
    }
}

/// Self-contained guidance shown to every Tracking MCP consumer.
const SERVER_INSTRUCTIONS: &str = r#"emucap Tracking MCP stores experiment records under `.emucap/` so runs can be reproduced and compared. It does not control an emulator. Use Control MCP (`emucap-mcp`) for memory, execution, screenshots, and input; pass the required values between the two servers explicitly.

[Identity from Control MCP]
- Pass `get_rom_info.rom_sha1` unchanged to `run_start` and ROM-scoped queries. Despite the legacy field name, treat it as an opaque Tracking identifier. When Control MCP returns `content_identity`, that identity covers the complete composite input. Never substitute a hash of only a CUE or another descriptor.
- Treat `run_id`, `finding_id`, and ROM identifiers as opaque returned values. Do not invent paths or normalize them; stored identifiers use ASCII letters and digits separated only by single hyphens.
- `connection_ref` is optional. Use `status.emulator_identity.name`, or `"port:" + status.listening_port`. It lets `run_start` resume the same unfinished run or supersede an older run for that connection.
- Record Control MCP analysis results with `log_gate` or `log_metric`.
- Mutations such as `write_memory`, `load_state`, `reset`, and input are not recorded automatically. Call `log_intervention` when they matter to reproduction.

[Storage and ownership]
The ledger root is `EMUCAP_TRACK_ROOT`, otherwise the nearest Git repository's `.emucap`, otherwise the current directory's `.emucap`. `bootstrap` returns `ledger_path`, `ledger_path_source`, and a warning for the current-directory fallback. Keep a single writer for `run.json`: do not run a separate writer such as `emucap track import` against the same live ledger. Give concurrent broker sessions separate ledger roots.

[Run lifecycle]
- `run_start(rom_sha1, connection_ref?, goal?, description?, tags?)` selects a new active run. With the same `connection_ref` and ROM, it resumes an unfinished run and returns `resumed:true`. With a different ROM, it supersedes the previous unfinished run for that connection.
- `run_resume(run_id)` reselects a stored running run after an MCP reconnect. It never creates a new run. A finished run cannot be resumed.
- `log_metric`, `log_gate`, `log_artifact`, `set_reproduction`, and `log_intervention` require an active run. `log_finding` accepts either an active run or an explicit `rom_sha1`.
- `run_finish(status=done|aborted|error, run_id?)` finishes the selected run or a run named by ID. Resume a run that will continue; finish only a run that will not.

[Records]
- `log_metric` stores a numeric observation.
- `log_gate` stores machine or judgment evidence; omitted `passed` means pending.
- `log_artifact` registers an existing file and computes its SHA-256. It does not capture a new artifact.
- `set_reproduction` sets the base and movie reference; reproduction status is derived.
- `log_finding` stores a ROM-scoped claim. `promoted:true` marks a confirmed finding.
- `log_intervention` stores a state-changing operation and its context.

[Queries]
- `query_runs` lists filtered runs, newest first. Corrupt JSON is counted as skipped instead of aborting the query.
- `get_run` returns a stored `run.json` and its ledger path.
- Missing runs return `run_not_found`; malformed identifiers return `invalid_identifier`. Filesystem diagnostics are reserved for actual ledger failures.
- `compare_runs` compares metrics, gates, reproduction, interventions, and files.
- `summarize_runs` aggregates status, reproduction, gates, interventions, and per-run summaries.
The server reports stored evidence; it does not decide whether an experiment succeeded.

[Large results]
Use `output_path` with `query_runs` or `summarize_runs` to write JSON to a file and receive a compact summary. Full memory dumps belong to Control MCP `dump_memory`; Tracking MCP has no memory-dump tool.

[CLI]
`emucap track ls|show|compare|summarize|reindex|import` reads the same ledger."#;

// ── 도구 구현 ────────────────────────────────────────────────────────────────

#[tool_router(router = tool_router)]
impl EmucapTrack {
    fn new() -> Self {
        Self {
            active_run: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    /// 활성 run에 mcp_ops를 적용하는 공통 래퍼(UlidGen·now·root 주입). 로직은 lib(mcp_ops)에 있고
    /// 여기선 active_run 상태 해소 + Value→CallToolResult 변환만 한다.
    fn with_active<F>(&self, f: F) -> CallToolResult
    where
        F: FnOnce(
            &Path,
            &ActiveRun,
            &emucap::track::id::UlidGen,
            &str,
        ) -> Result<serde_json::Value, emucap::track::mcp_ops::TrackingToolError>,
    {
        let active = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(ar) = active else {
            return track_err("no active run; call run_start or run_resume first");
        };
        let root = emucap::track::store::root_from_env();
        let now = emucap::track::clock::now_rfc3339();
        match f(&root, &ar, &emucap::track::id::UlidGen, &now) {
            Ok(v) => track_ok(v),
            Err(e) => track_err(e),
        }
    }

    /// resume 공통: binding을 in-memory active로 재바인딩한다(supersede+새 run이 아니라 디스크의
    /// still-running run을 다시 active로 잡는 것이라 새 run을 만들지 않는다). 드물게 다른 active가
    /// 이미 바인딩돼 있으면 그 run을 aborted(superseded)로 마감해 단일-active 불변식을 지킨다.
    /// 반환에 `resumed:true`. run_start의 resume 경로와 run_resume가 공유한다.
    fn rebind_active(
        &self,
        root: &Path,
        now: &str,
        binding: emucap::track::mcp_ops::ResumeBinding,
        caller_supplied_meta: bool,
    ) -> CallToolResult {
        let mut g = self.active_run.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = g.as_ref() {
            if prev.run_id != binding.run_id {
                let _ = emucap::track::ops::finish_run(
                    root,
                    &prev.rom_sha1,
                    &prev.run_id,
                    emucap::track::model::RunStatus::Aborted,
                    now,
                );
            }
        }
        let mut resp = serde_json::json!({
            "run_id": binding.run_id.clone(),
            "rom_sha1": binding.rom_sha1.clone(),
            "ledger_path": root.display().to_string(),
            "resumed": true,
        });
        // 침묵 폐기 방지: resume는 기존 run 메타를 유지하므로, 이 호출이 넘긴 goal/description/tags는
        // 적용되지 않는다 — 응답에 명시해 "새 goal로 새 실험" 의도가 옛 run에 흡수되는 걸 가시화한다.
        if caller_supplied_meta {
            resp["note"] = serde_json::json!("Resumed the existing run. This call's goal, description, and tags were ignored so the stored metadata remains unchanged. Finish it and call run_start to begin a new experiment with new metadata.");
        }
        *g = Some(ActiveRun {
            rom_sha1: binding.rom_sha1,
            run_id: binding.run_id,
            connection_ref: binding.connection_ref,
        });
        track_ok(resp)
    }

    #[tool(
        description = "Start here. Returns the ledger path, active run, stored unfinished runs, and available record/query operations. This server stores `.emucap/` experiment records and never controls an emulator. Obtain rom_sha1 from Control MCP get_rom_info and pass it to run_start."
    )]
    async fn bootstrap(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        track_ok(self.make_bootstrap_value())
    }

    #[tool(
        description = "Start or resume an experiment run without contacting an emulator. rom_sha1 is required and must come from Control MCP get_rom_info. With connection_ref and a stored running run for the same ROM, rebind that run and return resumed:true. With a different ROM, supersede the previous unfinished run for that connection and create a new one. Subsequent log_* calls target the active run."
    )]
    async fn run_start(&self, Parameters(a): Parameters<TrackRunStartArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        let now = emucap::track::clock::now_rfc3339();
        // resume(재연결 복원): connection_ref가 있고 디스크에 그 connection_ref + 같은 rom의 still-running
        // run이 있으면 supersede+새 run이 아니라 그 run을 active로 재바인딩한다(파편화 0). rom이 다르거나
        // 일치 running이 없으면 None → 아래 supersede 경로(start_run의 finish_stale_running)가 직전 run을
        // 마감하고 새 run을 만든다. best-effort: 조회 에러는 fall-through해 start_run이 노출한다.
        if let Some(cref) = a.connection_ref.as_deref() {
            if let Ok(Some(binding)) =
                emucap::track::mcp_ops::find_resumable_run(&root, cref, &a.rom_sha1)
            {
                return self.rebind_active(
                    &root,
                    &now,
                    binding,
                    a.goal.is_some() || a.description.is_some() || !a.tags.is_empty(),
                );
            }
        }
        // 원장 위생: 새 run 전 직전 in-memory 활성 run을 aborted(superseded)로 정리한다.
        // 같은 connection의 디스크 고아 running 정리(서버 재시작 복구)는 mcp_ops::start_run이 맡는다.
        if let Some(ar) = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = emucap::track::ops::finish_run(
                &root,
                &ar.rom_sha1,
                &ar.run_id,
                emucap::track::model::RunStatus::Aborted,
                &now,
            );
        }
        match emucap::track::mcp_ops::start_run(
            &root,
            &emucap::track::id::UlidGen,
            &now,
            &a.rom_sha1,
            a.connection_ref.clone(),
            a.goal,
            a.description,
            a.tags,
        ) {
            Ok(v) => {
                // start_run이 만든 run_id로 active를 바인딩한다. run_id가 없으면(있을 수 없는 내부
                // 불변식 위반) 조용히 성공시키지 않고 에러로 노출한다.
                match v.get("run_id").and_then(|s| s.as_str()) {
                    Some(run_id) => {
                        *self.active_run.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(ActiveRun {
                                rom_sha1: a.rom_sha1.clone(),
                                run_id: run_id.to_string(),
                                connection_ref: a.connection_ref,
                            });
                        track_ok(v)
                    }
                    None => track_err("internal error: start_run response has no run_id"),
                }
            }
            Err(e) => track_err(e),
        }
    }

    #[tool(
        description = "Rebind a stored running run as the in-memory active run. Use its run_id from bootstrap.running_runs after an MCP reconnect. This returns resumed:true and does not create a new run. A finished run is rejected. run_start with the same connection_ref can perform the same resume automatically."
    )]
    async fn run_resume(&self, Parameters(a): Parameters<RunResumeArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        let now = emucap::track::clock::now_rfc3339();
        match emucap::track::mcp_ops::resume_run_by_id(&root, &a.run_id) {
            Ok(binding) => self.rebind_active(&root, &now, binding, false),
            Err(e) => track_err(e),
        }
    }

    #[tool(
        description = "Finish a run with status done, aborted, or error. With run_id, finish that stored run even when it is not active; this supports orphan recovery after a restart. Without run_id, finish the active run. run_start already supersedes an older unfinished run for the same connection when necessary."
    )]
    async fn run_finish(&self, Parameters(a): Parameters<RunFinishArgs>) -> CallToolResult {
        let status =
            match emucap::track::mcp_ops::parse_run_status(a.status.as_deref().unwrap_or("done")) {
                Ok(s) => s,
                Err(e) => return track_err(e),
            };
        let root = emucap::track::store::root_from_env();
        let now = emucap::track::clock::now_rfc3339();
        // run_id 지정: in-memory 활성 상태에 의존하지 않고 디스크에서 직접 종료(서버 재시작 등 고아 복구).
        if let Some(rid) = a.run_id.as_deref() {
            return match emucap::track::mcp_ops::finish_run_by_id(&root, rid, status, &now) {
                Ok(v) => {
                    if let Some(id) = v.get("finished").and_then(|s| s.as_str()) {
                        let mut g = self.active_run.lock().unwrap_or_else(|e| e.into_inner());
                        if g.as_ref().map(|ar| ar.run_id == id).unwrap_or(false) {
                            *g = None;
                        }
                    }
                    track_ok(v)
                }
                Err(e) => track_err(e),
            };
        }
        let active = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(ar) = active else {
            return track_err(
                "no active run; call run_start first or provide run_id to finish a stored run",
            );
        };
        match emucap::track::mcp_ops::finish_active_run(
            &root,
            &ar.rom_sha1,
            &ar.run_id,
            status,
            &now,
        ) {
            Ok(v) => {
                *self.active_run.lock().unwrap_or_else(|e| e.into_inner()) = None;
                track_ok(v)
            }
            Err(e) => track_err(e),
        }
    }

    #[tool(description = "Record a numeric metric on the active run.")]
    async fn log_metric(&self, Parameters(a): Parameters<LogMetricArgs>) -> CallToolResult {
        self.with_active(|root, ar, gen, now| {
            emucap::track::mcp_ops::log_metric(
                root,
                &ar.rom_sha1,
                &ar.run_id,
                gen,
                now,
                &a.key,
                a.value,
            )
        })
    }

    #[tool(
        description = "Record verification evidence on the active run. kind must be machine or judgment; omit passed for a pending result. Use this for Control MCP analysis results."
    )]
    async fn log_gate(&self, Parameters(a): Parameters<LogGateArgs>) -> CallToolResult {
        // kind 검증을 active 검사보다 먼저(에러 우선순위 보존) — 로직은 mcp_ops::log_gate가 재검증·기록.
        if let Err(e) = emucap::track::mcp_ops::parse_gate_kind(&a.kind) {
            return track_err(e);
        }
        self.with_active(|root, ar, gen, now| {
            emucap::track::mcp_ops::log_gate(
                root,
                &ar.rom_sha1,
                &ar.run_id,
                gen,
                now,
                &a.name,
                &a.kind,
                a.passed,
                a.evidence_ref.clone(),
                a.detail.clone(),
                a.case_ref.clone(),
            )
        })
    }

    #[tool(
        description = "Register an existing file as an artifact of the active run and compute its SHA-256. This tool does not capture a new artifact."
    )]
    async fn log_artifact(&self, Parameters(a): Parameters<LogArtifactArgs>) -> CallToolResult {
        let active = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(ar) = active else {
            return track_err("no active run; call run_start or run_resume first");
        };
        let root = emucap::track::store::root_from_env();
        // 상대경로는 MCP 서버 cwd가 아니라 *작업 repo* 루트 기준으로 해소(최소놀람·재현성).
        let git_root = emucap::track::store::nearest_git_root();
        match emucap::track::mcp_ops::log_artifact(
            &root,
            &ar.rom_sha1,
            &ar.run_id,
            &emucap::track::id::UlidGen,
            &a.kind,
            Path::new(&a.path),
            git_root.as_deref(),
            None,
        ) {
            Ok(v) => track_ok(v),
            Err(e) => track_err(e),
        }
    }

    #[tool(
        description = "Set the active run's reproduction base and movie reference. reproduction status is derived automatically."
    )]
    async fn set_reproduction(&self, Parameters(a): Parameters<SetReproArgs>) -> CallToolResult {
        self.with_active(|root, ar, _gen, _now| {
            emucap::track::mcp_ops::set_reproduction(
                root,
                &ar.rom_sha1,
                &ar.run_id,
                a.base.clone(),
                a.movie_ref.clone(),
            )
        })
    }

    #[tool(
        description = "Record a state-changing operation on the active run, such as write_memory, load_state, reset, or input_burst. Control MCP does not log mutations automatically, so record interventions needed for reproduction explicitly."
    )]
    async fn log_intervention(
        &self,
        Parameters(a): Parameters<LogInterventionArgs>,
    ) -> CallToolResult {
        self.with_active(|root, ar, gen, now| {
            emucap::track::mcp_ops::log_intervention(
                root,
                &ar.rom_sha1,
                &ar.run_id,
                gen,
                now,
                a.at_frame,
                a.at_event.clone(),
                a.frozen_context,
                &a.op,
                a.args.clone().unwrap_or(serde_json::Value::Null),
            )
        })
    }

    #[tool(
        description = "Record a ROM-scoped finding. promoted:true marks it as confirmed. When rom_sha1 is omitted, use the active run's ROM identifier."
    )]
    async fn log_finding(&self, Parameters(a): Parameters<LogFindingArgs>) -> CallToolResult {
        let active = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rom_sha1 = match a
            .rom_sha1
            .clone()
            .or_else(|| active.as_ref().map(|r| r.rom_sha1.clone()))
        {
            Some(s) => s,
            None => return track_err("rom_sha1 was omitted and there is no active run"),
        };
        let run_id = active.as_ref().map(|r| r.run_id.clone());
        let root = emucap::track::store::root_from_env();
        let now = emucap::track::clock::now_rfc3339();
        match emucap::track::mcp_ops::log_finding(
            &root,
            &rom_sha1,
            &emucap::track::id::UlidGen,
            &now,
            &a.claim,
            run_id,
            a.evidence_refs,
            a.promoted,
        ) {
            Ok(v) => track_ok(v),
            Err(e) => track_err(e),
        }
    }

    #[tool(description = "Query stored runs using optional rom_sha1, goal, and status filters.")]
    async fn query_runs(&self, Parameters(a): Parameters<QueryRunsArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        match emucap::track::mcp_ops::query_runs(
            &root,
            emucap::track::query::RunFilter {
                rom_sha1: a.rom_sha1,
                goal: a.goal,
                status: a.status,
            },
        ) {
            Ok(v) => match a.output_path.as_deref() {
                Some(p) => match emucap::offload::offload_result(&v, std::path::Path::new(p)) {
                    Ok(s) => track_ok(s),
                    Err(e) => track_err(e),
                },
                None => track_ok(v),
            },
            Err(e) => track_err(e),
        }
    }

    #[tool(description = "Return the stored run.json record for a run.")]
    async fn get_run(&self, Parameters(a): Parameters<GetRunArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        match emucap::track::mcp_ops::get_run(&root, &a.rom_sha1, &a.run_id) {
            Ok(v) => track_ok(v),
            Err(e) => track_err(e),
        }
    }

    #[tool(
        description = "Compare two runs and return changes in metrics, gates, reproduction, interventions, and files without contacting an emulator. Run IDs are globally unique. For repeated gate or metric names, the latest value is representative and the occurrence count is included."
    )]
    async fn compare_runs(&self, Parameters(a): Parameters<CompareRunsArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        match emucap::track::mcp_ops::compare_runs(&root, &a.run_id_a, &a.run_id_b) {
            Ok(v) => track_ok(v),
            Err(e) => track_err(e),
        }
    }

    #[tool(
        description = "Summarize runs filtered by goal, tag, or ROM: status and reproduction distributions, gate outcomes, intervention kinds, metric names, and per-run summaries. This does not contact an emulator or decide success. Corrupt runs are skipped and counted."
    )]
    async fn summarize_runs(&self, Parameters(a): Parameters<SummarizeRunsArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        let filter = emucap::track::summary::SummaryFilter {
            goal: a.goal,
            tag: a.tag,
            rom_sha1: a.rom_sha1,
        };
        match emucap::track::mcp_ops::summarize_runs(&root, filter) {
            Ok(v) => match a.output_path.as_deref() {
                Some(p) => match emucap::offload::offload_result(&v, std::path::Path::new(p)) {
                    Ok(s) => track_ok(s),
                    Err(e) => track_err(e),
                },
                None => track_ok(v),
            },
            Err(e) => track_err(e),
        }
    }
}

impl EmucapTrack {
    /// bootstrap 응답 생성: ledger_path, 현재 run, 저장된 미종료 run, 기록·검색 안내를 담는다.
    /// running run 검색은 best-effort이며 저장소가 없거나 손상돼도 bootstrap은 성공한다.
    fn make_bootstrap_value(&self) -> serde_json::Value {
        let (root, root_source) = emucap::track::store::root_from_env_with_source();
        let active = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let active_json = match &active {
            Some(ar) => serde_json::json!({
                "rom_sha1": ar.rom_sha1,
                "run_id": ar.run_id,
                "connection_ref": ar.connection_ref,
            }),
            None => serde_json::Value::Null,
        };
        // 디스크의 미종료(running) run을 노출해 고아 복구(run_finish(run_id))를 돕는다. best-effort.
        let running = match emucap::track::mcp_ops::query_runs(
            &root,
            emucap::track::query::RunFilter {
                status: Some("running".into()),
                ..Default::default()
            },
        ) {
            Ok(v) => v.get("runs").cloned().unwrap_or(serde_json::json!([])),
            Err(_) => serde_json::json!([]),
        };
        let mut out = serde_json::json!({
            "ok": true,
            "start_here": true,
            "first_tool": "bootstrap",
            "server": "emucap-track-mcp",
            "emulator_less": true,
            "ledger_path": root.display().to_string(),
            "ledger_path_source": root_source.as_str(),
            "ledger_root_env": "EMUCAP_TRACK_ROOT",
            "active_run": active_json,
            "running_runs": running,
            "assembly": {
                "note": "This MCP has no emulator connection. Pass rom_sha1 and connection_ref from Control MCP (emucap-mcp).",
                "rom_sha1": "Pass the normalized get_rom_info.rom_sha1 value unchanged to run_start. For composite media, require Control MCP content_identity and never hash only the descriptor.",
                "connection_ref": "Optionally use the Control MCP status connection name or `port:N`. run_start resumes a stored running run for the same connection and ROM.",
                "analysis_verbs": "Control MCP analysis operations regression_run and verify_determinism return results only. Record relevant results here with log_gate or log_metric.",
                "interventions": "Control MCP does not automatically record write_memory, load_state, reset, or input. Record relevant mutations with log_intervention."
            },
            "supported_queries": ["query_runs", "get_run", "compare_runs", "summarize_runs"],
            "resume": "After a reconnect, select this session's run from running_runs and call run_resume(run_id=...), or call run_start with the same connection_ref for automatic resume. No new run is created.",
            "orphan_recovery": "Use run_finish(run_id=...) only for a genuinely abandoned run. Resume a run that will continue.",
            "next_action": "If active_run is null and running_runs contains this session, call run_resume. Otherwise call run_start with rom_sha1. Finish only a genuinely abandoned run."
        });
        // ledger 경로 모호 케이스: cwd_fallback이면 위치가 서버 cwd에 의존하니 경고를 단다.
        if let Some(w) = root_source.warning() {
            if let Some(obj) = out.as_object_mut() {
                obj.insert(
                    "ledger_path_warning".into(),
                    serde_json::Value::String(w.to_string()),
                );
            }
        }
        out
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for EmucapTrack {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "emucap-track-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(SERVER_INSTRUCTIONS)
    }

    async fn discover(
        &self,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::DiscoverResult, rmcp::ErrorData> {
        Ok(rmcp::model::DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        )
        .with_ttl_ms(STATIC_MCP_METADATA_TTL_MS)
        .with_cache_scope(rmcp::model::CacheScope::Public))
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        Ok(
            rmcp::model::ListToolsResult::with_all_items(self.tool_router.list_all())
                .with_ttl_ms(STATIC_MCP_METADATA_TTL_MS)
                .with_cache_scope(rmcp::model::CacheScope::Public),
        )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = EmucapTrack::new();
    let service = server.serve(emucap::mcp_stdio::bounded_stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
// 테스트는 프로세스 전역 env(EMUCAP_TRACK_ROOT)를 직렬화하려 ENV_LOCK 가드를 .await 너머로 든다.
// tokio::test는 current-thread 런타임이고 추적 도구 future는 yield하지 않아 실제 경합은 없다 — 의도된 lint.
#[allow(clippy::await_holding_lock)]
#[path = "tests/emucap_track_mcp_tests.rs"]
mod tests;
