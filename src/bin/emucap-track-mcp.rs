//! emucap 추적 MCP(`emucap-track-mcp`) — 실험 원장 전용 서버(에뮬레이터 무관).
//!
//! 이 서버는 `.emucap/` 원장에 run을 기록·질의할 뿐 에뮬레이터를 모른다(framework-agnostic). 에뮬
//! context(rom_sha1·connection_ref)는
//! 제어 MCP의 get_rom_info/status에서 에이전트가 읽어 인자로 넘긴다 — 두 MCP는 서로 호출하지 않는다.
//! 도구 로직은 전부 `emucap::track::mcp_ops`(lib)에 있고 여기선 active_run 상태 해소 + Value→CallToolResult
//! 변환만 한다(로직을 lib로 둔 목적은 단위 테스트 가능성).

use std::path::Path;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

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

#[derive(Deserialize, JsonSchema)]
struct TrackRunStartArgs {
    /// 필수 — 제어 MCP `get_rom_info`의 균일 `rom_sha1` 필드를 받아 전달한다(어댑터가 어떤 해시를 쓰든 canonical로 정규화돼 나오는 opaque 그룹핑 키). 이 MCP는 에뮬레이터를 모르므로 추론하지 않는다.
    rom_sha1: String,
    /// 선택 — 어느 세션/연결의 run인지 표식(제어 MCP `status.emulator_identity.name` 또는 `"port:"`+
    /// `status.listening_port`). 같은 connection_ref의 직전 미종료 run을 자동 마감(superseded)하는 데 쓴다.
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
struct RunResumeArgs {
    /// 재바인딩할 run_id(전역 유일). 디스크에서 status=running일 때만 resume된다(종료된 run은 새 run_start로).
    run_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct RunFinishArgs {
    /// done|aborted|error (기본 done)
    #[serde(default)]
    status: Option<String>,
    /// 특정 run을 id로 종료(전역 유일). 생략 시 활성 run. 서버 재시작 등으로 고아화된 run 복구용.
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct LogMetricArgs {
    key: String,
    value: f64,
}

#[derive(Deserialize, JsonSchema)]
struct LogGateArgs {
    name: String,
    /// machine | judgment
    kind: String,
    passed: Option<bool>,
    evidence_ref: Option<String>,
    detail: Option<String>,
    case_ref: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct LogArtifactArgs {
    kind: String,
    /// 이미 캡처된 파일 경로. 상대경로는 작업 repo git root 기준으로 해소된다(MCP 서버 cwd 아님).
    path: String,
}

#[derive(Deserialize, JsonSchema)]
struct SetReproArgs {
    base: Option<String>,
    movie_ref: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct LogFindingArgs {
    /// 생략 시 활성 run의 rom_sha1. 둘 다 없으면 에러.
    rom_sha1: Option<String>,
    claim: String,
    #[serde(default)]
    evidence_refs: Vec<String>,
    #[serde(default)]
    promoted: bool,
}

#[derive(Deserialize, JsonSchema)]
struct LogInterventionArgs {
    /// 개입 종류 — write_memory|load_state|reset|input_burst 등 자유 라벨. 제어 MCP가 더는 자동
    /// 기록하지 않으므로 에이전트가 상태변경을 직접 기록해 repro_status 충실도를 유지한다.
    op: String,
    /// 개입의 구조화 인자(예: write_memory면 {memory_type,address,hex}). 생략 시 null.
    #[serde(default)]
    args: Option<serde_json::Value>,
    /// 개입 시점 프레임(선택).
    #[serde(default)]
    at_frame: Option<u64>,
    /// 개입을 유발한 이벤트 참조(선택).
    #[serde(default)]
    at_event: Option<String>,
    /// frozen 컨텍스트에서의 개입이면 true(기본 false).
    #[serde(default)]
    frozen_context: bool,
}

#[derive(Deserialize, JsonSchema)]
struct QueryRunsArgs {
    rom_sha1: Option<String>,
    goal: Option<String>,
    status: Option<String>,
    /// 결과를 이 경로에 JSON으로 저장하고 요약만 반환(큰 결과가 예상될 때 context 절약). 생략 시 인라인.
    #[serde(default)]
    output_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct GetRunArgs {
    rom_sha1: String,
    run_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct CompareRunsArgs {
    /// 비교 기준 run_id(A)
    run_id_a: String,
    /// 비교 대상 run_id(B)
    run_id_b: String,
}

#[derive(Deserialize, JsonSchema)]
struct SummarizeRunsArgs {
    /// goal 정확 일치 필터(생략 시 무제약)
    #[serde(default)]
    goal: Option<String>,
    /// tag 정확 원소 일치 필터(생략 시 무제약)
    #[serde(default)]
    tag: Option<String>,
    /// rom_sha1 필터(생략 시 무제약)
    #[serde(default)]
    rom_sha1: Option<String>,
    /// 결과를 이 경로에 JSON으로 저장하고 요약만 반환(큰 결과가 예상될 때 context 절약). 생략 시 인라인.
    #[serde(default)]
    output_path: Option<String>,
}

// ── 공통 헬퍼 ────────────────────────────────────────────────────────────────

/// 추적 도구 공통: ok json
fn track_ok(v: serde_json::Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(v.to_string())])
}
/// 추적 도구 공통: 에러 텍스트
fn track_err(msg: impl std::fmt::Display) -> CallToolResult {
    let mut r = CallToolResult::success(vec![Content::text(format!("{msg}"))]);
    r.is_error = Some(true);
    r
}

/// MCP 서버 사용 가이드. 에이전트가 항상 보는 유일한 문서이므로 자기완결적이어야 한다.
const SERVER_INSTRUCTIONS: &str = r#"emucap 실험 추적 MCP — 시도를 기록·재현·비교해 "어떤 경우 패치가 성공하나"를 쌓는 경량 FS 원장(.emucap/, gitignore)이다. **이 서버는 에뮬레이터를 모른다(emulator-less, framework-agnostic).** 메모리/상태/화면/입력 같은 라이브 제어는 별도 제어 MCP(emucap-mcp)가 한다 — 이 둘은 서로 호출하지 않고, 에이전트가 조립한다(비유: MLflow ↔ TensorFlow).

[조립 — 제어 MCP에서 받아 넘길 것]
  • rom_sha1: 이 MCP는 ROM을 모른다. 제어 MCP `get_rom_info`의 균일 `rom_sha1` 필드로 구해 run_start/get_run/query_runs/log_finding에 넘긴다(어댑터가 어떤 해시를 쓰든 canonical로 정규화돼 나온다). `rom_sha1`이 없는 백엔드(콘텐츠 해시 미반환)만 `shasum -a1 <content>` 폴백.
  • connection_ref(선택): 제어 MCP `status.emulator_identity.name`, 또는 `"port:" + status.listening_port`. run_start에 넘기면 같은 connection의 직전 미종료 run을 자동 마감(superseded)한다.
  • 분석 verb(regression_run/verify_determinism)는 제어 MCP가 에뮬을 구동해 verdict를 *반환만* 한다(원장에 쓰지 않음). 그 결과를 받아 log_gate/log_metric으로 여기 기록한다(bisect는 프레임만 반환).
  • 상태변경(write_memory/load_state/reset/입력)은 제어 MCP가 자동 기록하지 않는다. 재현 충실도(repro_status)를 위해 에이전트가 log_intervention으로 명시 기록한다.

[원장] EMUCAP_TRACK_ROOT(명시) > 작업 repo git root의 .emucap > ./.emucap(폴백). bootstrap이 ledger_path와 ledger_path_source(env|git_root|cwd_fallback)를 반환한다 — cwd_fallback(비-git working dir)이면 경로가 서버 cwd에 의존해 모호하니 ledger_path_warning을 함께 반환한다(EMUCAP_TRACK_ROOT 명시나 git init 권장). .emucap/의 유일 writer가 이 서버이므로 run.json 동시성은 한 프로세스 안에서 직렬화된다 — 라이브 세션 중 `emucap track import` 같은 별 프로세스 write는 피한다. ⚠ broker 다중 세션은 세션마다 추적 MCP가 떠 같은 git-root .emucap을 N-writer로 쓰니 동시 write에 주의(세션별 EMUCAP_TRACK_ROOT 분리 권장).

[run 수명]
  run_start(rom_sha1 필수, connection_ref/goal/description/tags 선택) → in-memory active run 바인딩. 반환 {run_id, rom_sha1, ledger_path}. **resume**: connection_ref가 있고 디스크에 그 connection_ref + 같은 rom의 still-running run이 있으면 새 run을 만들지 않고 그 run을 active로 재바인딩한다(반환 resumed:true) — /mcp 재연결로 active 바인딩이 끊겨도 같은 run을 이어써 한 세션이 run 여러 개로 파편화되지 않는다. rom이 다르면 같은 connection_ref의 직전 미종료 run을 자동 마감(superseded)하고 새 run을 만든다(#56).
  run_resume(run_id): 특정 running run을 직접 active로 재바인딩한다(반환 resumed:true). 재연결 후 bootstrap의 running_runs에서 이 세션 run을 골라 이어쓸 때. status가 running이 아니면(이미 종료) 에러.
  log_metric/log_gate/log_artifact/set_reproduction/log_intervention은 active run이 있어야 한다(없으면 즉시 에러 — run_start 또는 run_resume 먼저). log_finding은 active run 또는 명시 rom_sha1이 있으면 기록한다.
  run_finish(status=done|aborted|error 기본 done, run_id 선택): run_id를 주면 active 상태와 무관하게 그 run을 디스크에서 직접 종료한다(서버 재시작 등으로 고아화된 running run 복구용). 생략 시 active run을 종료한다. 이어쓸 run은 run_finish가 아니라 resume이다 — 정말 버릴 고아만 finish.

[기록 도구]
  log_metric(key, value): 정량 메트릭 1건.
  log_gate(name, kind=machine|judgment, passed?/evidence_ref?/detail?/case_ref?): 검증 게이트(passed 생략=pending). 제어 MCP 분석 verb의 결과를 여기로 기록하는 1순위 도구.
  log_artifact(kind, path): 이미 캡처된 파일을 등록(sha256 계산, 새 캡처 안 함). 상대경로는 작업 repo git root 기준으로 해소된다.
  set_reproduction(base?, movie_ref?): active run의 재현 base/movie 설정(repro_status 자동 도출).
  log_finding(claim, rom_sha1?/evidence_refs?/promoted?): 발견을 ROM 스코프로 기록(promoted=true면 승격).
  log_intervention(op, args?/at_frame?/at_event?/frozen_context?): active run에 상태변경 lineage를 명시 기록.

[질의 — 순수 읽기]
  query_runs(rom_sha1?/goal?/status?): 필터로 run 목록(최근 우선). 손상 JSON은 skipped로 세고 죽지 않는다.
  get_run(rom_sha1, run_id): run 상세(run.json 정본 + ledger_path). run_id는 전역 유일이나 원장이 rom 디렉터리로 샤딩돼 위치 특정에 rom_sha1이 필요하다.
  compare_runs(run_id_a, run_id_b): 두 run의 메트릭 delta·게이트 변화·재현성·개입·산출물 diff.
  summarize_runs(goal?/tag?/rom_sha1?): run 묶음 횡단 rollup(상태·재현성 분포·게이트 통과율·개입 op 빈도·per-run 캡슐).
  **성공 판정은 하지 않는다 — 사실만 펼치고 "어떤 경우 성공인가" 패턴은 에이전트가 추론한다.**

[셸 CLI] emucap track ls|show|compare|summarize|reindex|import (같은 .emucap/ 원장을 읽는다).

[거대 결과] query_runs/summarize_runs는 output_path를 줘 파일로 받고 요약+경로만 받아라(컨텍스트 절약). 벌크 메모리 덤프는 *제어 MCP*의 dump_memory(이 추적 MCP엔 없다)."#;

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
        ) -> Result<serde_json::Value, String>,
    {
        let active = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(ar) = active else {
            return track_err("활성 run 없음 — run_start 먼저");
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
            resp["note"] = serde_json::json!("기존 run을 resume했다 — 이 호출의 goal/description/tags는 무시됐다(기존 run 메타 유지). 새 goal로 *새 실험*을 시작하려면 run_finish 후 run_start하라.");
        }
        *g = Some(ActiveRun {
            rom_sha1: binding.rom_sha1,
            run_id: binding.run_id,
            connection_ref: binding.connection_ref,
        });
        track_ok(resp)
    }

    #[tool(
        description = "추적 MCP의 첫 진입점. 이 서버는 에뮬레이터를 모르는 실험 원장 전용(.emucap/)이다. ledger_path, 현재 in-memory active run, 디스크의 미종료(running) run(고아 복구 후보), 지원 질의/조립 안내를 반환한다. rom_sha1은 제어 MCP(emucap-mcp)의 get_rom_info에서 읽어 run_start에 넘긴다"
    )]
    async fn bootstrap(&self) -> CallToolResult {
        track_ok(self.make_bootstrap_value())
    }

    #[tool(
        description = "실험 Run을 시작한다(메타 전용, 에뮬레이터 무통신). rom_sha1은 필수 — 제어 MCP의 get_rom_info에서 읽어 전달하라(이 MCP는 에뮬레이터를 모른다). connection_ref는 선택(어느 세션 run인지 표식). **resume**: connection_ref가 있고 디스크에 그 connection_ref + 같은 rom의 still-running run이 있으면 새 run을 만들지 않고 그 run을 active로 재바인딩한다(반환 resumed:true) — /mcp 재연결로 active가 끊겨도 같은 run을 이어써 파편화를 막는다. rom이 다르면 같은 connection_ref의 직전 미종료 run을 자동 마감하고 새 run을 만든다(#56). 이후 log_*가 이 run에 기록된다."
    )]
    async fn run_start(&self, Parameters(a): Parameters<TrackRunStartArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        let now = emucap::track::clock::now_rfc3339();
        // resume(재연결 복원): connection_ref가 있고 디스크에 그 connection_ref + 같은 rom의 still-running
        // run이 있으면 supersede+새 run이 아니라 그 run을 active로 재바인딩한다(파편화 0). rom이 다르거나
        // 일치 running이 없으면 None → 아래 supersede 경로(start_run의 finish_stale_running)가 직전 run을
        // 마감하고 새 run을 만든다(#56 보존). best-effort: 조회 에러는 fall-through해 start_run이 정직하게 노출.
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
        // 원장 위생(#56): 새 run 전 직전 in-memory 활성 run을 aborted(superseded)로 정리한다.
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
                // 불변식 위반) 조용히 성공시키지 않고 정직하게 에러로 노출한다.
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
                    None => track_err("내부 오류: start_run 응답에 run_id 없음"),
                }
            }
            Err(e) => track_err(e),
        }
    }

    #[tool(
        description = "특정 running Run을 in-memory active로 다시 바인딩한다(resume). /mcp 재연결 등으로 active 바인딩이 끊겼을 때, bootstrap의 running_runs에서 이 세션 run을 골라 run_id로 이어쓴다 — 새 run을 만들지 않아 파편화가 없다(반환 resumed:true). status가 running이 아니면(이미 종료) 에러. connection_ref가 있으면 run_start(같은 connection_ref)로도 같은 resume이 일어난다."
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
        description = "활성 Run을 종료한다(status=done|aborted|error). run_id를 주면 활성 상태와 무관하게 그 run을 직접 종료한다(서버 재시작 등으로 고아화된 running run 복구용). run_start는 새 run 시작 시 같은 연결의 직전 미종료 run을 자동 마감하므로 보통은 명시 종료만 신경쓰면 된다."
    )]
    async fn run_finish(&self, Parameters(a): Parameters<RunFinishArgs>) -> CallToolResult {
        let status =
            match emucap::track::mcp_ops::parse_run_status(a.status.as_deref().unwrap_or("done")) {
                Ok(s) => s,
                Err(e) => return track_err(e),
            };
        let root = emucap::track::store::root_from_env();
        let now = emucap::track::clock::now_rfc3339();
        // run_id 지정: in-memory 활성 상태에 의존하지 않고 디스크에서 직접 종료(서버 재시작 등 고아 복구, #56).
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
            return track_err("활성 run 없음 — run_start 먼저(또는 run_id로 특정 run 종료)");
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

    #[tool(description = "활성 Run에 정량 메트릭을 기록한다(메타 전용).")]
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
        description = "활성 Run에 검증 게이트를 기록한다(kind=machine|judgment, passed 생략=pending). 제어 MCP의 분석 verb(bisect/regression_run/verify_determinism) 결과를 원장에 남기는 1순위 도구."
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
        description = "이미 캡처된 파일을 활성 Run의 artifact로 등록한다(sha256 계산, 새 캡처 안 함)."
    )]
    async fn log_artifact(&self, Parameters(a): Parameters<LogArtifactArgs>) -> CallToolResult {
        let active = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(ar) = active else {
            return track_err("활성 run 없음 — run_start 먼저");
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

    #[tool(description = "활성 Run의 재현 base/movie를 설정한다(repro_status는 자동 도출).")]
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
        description = "활성 Run에 상태변경 개입(intervention)을 명시 기록한다(op=write_memory|load_state|reset|input_burst 등). 제어 MCP는 자동 기록하지 않으므로(에뮬→원장 의존 금지) 에이전트가 재현 충실도(repro_status)를 위해 직접 기록한다."
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
        description = "발견을 ROM 스코프로 기록한다(promoted=true면 승격). rom_sha1 생략 시 활성 run의 것을 쓴다."
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
            None => return track_err("rom_sha1 미지정 + 활성 run 없음"),
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

    #[tool(description = "추적 원장에서 Run을 질의한다(rom_sha1/goal/status 필터).")]
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

    #[tool(description = "Run 상세(run.json 정본)를 반환한다.")]
    async fn get_run(&self, Parameters(a): Parameters<GetRunArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        match emucap::track::mcp_ops::get_run(&root, &a.rom_sha1, &a.run_id) {
            Ok(v) => track_ok(v),
            Err(e) => track_err(e),
        }
    }

    #[tool(
        description = "두 run을 구조화 diff한다(메트릭 delta·게이트 변화·재현성·개입·산출물 집계). 순수 추적 읽기, 에뮬레이터 무통신. run_id는 전역 유일. gates/metrics가 같은 이름으로 여러 번 기록됐으면 삽입순 마지막을 대표로 집고 발생 횟수를 함께 반환한다."
    )]
    async fn compare_runs(&self, Parameters(a): Parameters<CompareRunsArgs>) -> CallToolResult {
        let root = emucap::track::store::root_from_env();
        match emucap::track::mcp_ops::compare_runs(&root, &a.run_id_a, &a.run_id_b) {
            Ok(v) => track_ok(v),
            Err(e) => track_err(e),
        }
    }

    #[tool(
        description = "goal/tag/rom로 묶은 run들의 횡단 rollup을 낸다: 상태·재현성 분포, 게이트 name별 통과/실패/미결 수, 개입 op 빈도, 메트릭 key, per-run 캡슐. 순수 추적 읽기(에뮬레이터 무통신). 성공 판정은 하지 않는다 — '어떤 경우 성공인가'의 패턴은 이 사실들을 보고 직접 추론한다. 손상 run은 건너뛰고 skipped로 센다."
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
    /// bootstrap 응답 조립(순수): ledger_path, in-memory active run, 디스크 미종료 run(고아 복구 후보),
    /// 조립/질의 안내. running 조회는 best-effort — 원장이 없거나 손상돼도 bootstrap을 죽이지 않는다.
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
                "note": "이 MCP는 에뮬레이터를 모른다. rom_sha1·connection_ref는 제어 MCP(emucap-mcp)에서 읽어 넘긴다.",
                "rom_sha1": "제어 MCP `get_rom_info`의 균일 `rom_sha1` 필드로 구해 run_start에 넘겨라(없는 백엔드만 `shasum -a1 <content>`)",
                "connection_ref": "제어 MCP status의 connection 이름 또는 \"port:N\"(선택; 같은 connection + 같은 rom의 still-running run은 run_start가 새 run 대신 resume한다)",
                "analysis_verbs": "bisect/regression_run/verify_determinism은 제어 MCP가 결과를 반환만 한다 — 그 결과를 log_gate/log_metric으로 여기 기록하라",
                "interventions": "write_memory/load_state/reset/입력은 제어 MCP가 자동 기록하지 않는다 — log_intervention으로 명시 기록하라"
            },
            "supported_queries": ["query_runs", "get_run", "compare_runs", "summarize_runs"],
            "resume": "재연결로 active_run이 끊겼으면 running_runs에서 이 세션 run을 골라 run_resume(run_id=...)로 이어쓴다(또는 같은 connection_ref로 run_start하면 자동 resume). 새 run을 만들지 않아 파편화가 없다.",
            "orphan_recovery": "정말 죽은 고아만 run_finish(run_id=...)로 종료한다(이어쓸 run은 resume, 버릴 run만 finish).",
            "next_action": "active_run이 null이고 running_runs에 이 세션 run이 있으면 run_resume(run_id=...)로 이어쓴다. 없으면 run_start(rom_sha1=...)로 시작한다. 진짜 고아만 run_finish로 정리한다."
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
        let mut info = ServerInfo::default();
        info.instructions = Some(SERVER_INSTRUCTIONS.into());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = EmucapTrack::new();
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
// 테스트는 프로세스 전역 env(EMUCAP_TRACK_ROOT)를 직렬화하려 ENV_LOCK 가드를 .await 너머로 든다.
// tokio::test는 current-thread 런타임이고 추적 도구 future는 yield하지 않아 실제 경합은 없다 — 의도된 lint.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use std::sync::{Mutex as StdMutex, MutexGuard};
    use tempfile::TempDir;

    /// EMUCAP_TRACK_ROOT를 임시 디렉터리로 둔다. 환경변수는 프로세스 전역이라 직렬화 lock으로
    /// 테스트 간 간섭을 막는다(반환한 guard가 살아있는 동안 단독 점유). guard와 TempDir를 함께
    /// 돌려줘 .await을 지나도 유효하다.
    fn temp_env() -> (TempDir, MutexGuard<'static, ()>) {
        static ENV_LOCK: StdMutex<()> = StdMutex::new(());
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        std::env::set_var("EMUCAP_TRACK_ROOT", dir.path());
        (dir, guard)
    }

    /// CallToolResult의 텍스트 본문을 추출한다(검증용).
    fn body_text(r: &CallToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("")
    }

    #[tokio::test]
    async fn run_start_binds_active_and_log_metric_round_trips() {
        let (dir, _g) = temp_env();
        let root = dir.path();
        let s = EmucapTrack::new();
        // log_metric before run_start → 활성 run 없음 에러
        let r = s
            .log_metric(Parameters(LogMetricArgs {
                key: "k".into(),
                value: 1.0,
            }))
            .await;
        assert_eq!(r.is_error, Some(true));

        // run_start binds active
        let r = s
            .run_start(Parameters(TrackRunStartArgs {
                rom_sha1: "sha_a".into(),
                connection_ref: Some("port:1".into()),
                goal: Some("font".into()),
                description: None,
                tags: vec!["t".into()],
            }))
            .await;
        assert_ne!(r.is_error, Some(true));
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let run_id = v["run_id"].as_str().unwrap().to_string();
        assert_eq!(v["rom_sha1"], "sha_a");
        // active 바인딩 확인
        assert_eq!(
            s.active_run.lock().unwrap().as_ref().unwrap().run_id,
            run_id
        );

        // now log_metric succeeds
        let r = s
            .log_metric(Parameters(LogMetricArgs {
                key: "frames".into(),
                value: 42.0,
            }))
            .await;
        assert_ne!(r.is_error, Some(true));

        // disk 정본 확인
        let run = emucap::track::store::load_run(root, "sha_a", &run_id).unwrap();
        assert_eq!(run.status, emucap::track::model::RunStatus::Running);
        assert!(run
            .metrics
            .iter()
            .any(|m| m.key == "frames" && m.value == 42.0));
    }

    #[tokio::test]
    async fn run_finish_clears_active() {
        let (dir, _g) = temp_env();
        let root = dir.path();
        let s = EmucapTrack::new();
        let r = s
            .run_start(Parameters(TrackRunStartArgs {
                rom_sha1: "sha_b".into(),
                connection_ref: None,
                goal: None,
                description: None,
                tags: vec![],
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let run_id = v["run_id"].as_str().unwrap().to_string();

        let r = s
            .run_finish(Parameters(RunFinishArgs {
                status: Some("done".into()),
                run_id: None,
            }))
            .await;
        assert_ne!(r.is_error, Some(true));
        assert!(s.active_run.lock().unwrap().is_none());
        let run = emucap::track::store::load_run(root, "sha_b", &run_id).unwrap();
        assert_eq!(run.status, emucap::track::model::RunStatus::Done);
    }

    #[tokio::test]
    async fn run_finish_by_id_recovers_orphan() {
        let (dir, _g) = temp_env();
        let root = dir.path();
        // 디스크에 직접 running run을 만들고(고아), 새 서버 인스턴스로 id 종료
        let now = emucap::track::clock::now_rfc3339();
        let run = emucap::track::ops::create_run(
            root,
            &emucap::track::id::UlidGen,
            &now,
            "sha_c",
            None,
            None,
            vec![],
            None,
        )
        .unwrap();
        let s = EmucapTrack::new(); // active 없음
        let r = s
            .run_finish(Parameters(RunFinishArgs {
                status: Some("aborted".into()),
                run_id: Some(run.id.clone()),
            }))
            .await;
        assert_ne!(r.is_error, Some(true));
        let loaded = emucap::track::store::load_run(root, "sha_c", &run.id).unwrap();
        assert_eq!(loaded.status, emucap::track::model::RunStatus::Aborted);
    }

    #[tokio::test]
    async fn log_intervention_records_to_active_run() {
        let (dir, _g) = temp_env();
        let root = dir.path();
        let s = EmucapTrack::new();
        let r = s
            .run_start(Parameters(TrackRunStartArgs {
                rom_sha1: "sha_d".into(),
                connection_ref: None,
                goal: None,
                description: None,
                tags: vec![],
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let run_id = v["run_id"].as_str().unwrap().to_string();

        let r = s
            .log_intervention(Parameters(LogInterventionArgs {
                op: "write_memory".into(),
                args: Some(serde_json::json!({"memory_type": "snesWorkRam", "address": 104})),
                at_frame: Some(7),
                at_event: None,
                frozen_context: true,
            }))
            .await;
        assert_ne!(r.is_error, Some(true));
        let run = emucap::track::store::load_run(root, "sha_d", &run_id).unwrap();
        assert_eq!(run.interventions.len(), 1);
        assert_eq!(run.interventions[0].op, "write_memory");
        assert_eq!(run.interventions[0].at_frame, Some(7));
    }

    #[tokio::test]
    async fn bootstrap_reports_ledger_active_and_orphans() {
        let (dir, _g) = temp_env();
        let root = dir.path();
        // 고아 running run 하나
        let now = emucap::track::clock::now_rfc3339();
        emucap::track::ops::create_run(
            root,
            &emucap::track::id::UlidGen,
            &now,
            "sha_e",
            None,
            None,
            vec![],
            None,
        )
        .unwrap();
        let s = EmucapTrack::new();
        let r = s.bootstrap().await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(v["server"], "emucap-track-mcp");
        assert_eq!(v["emulator_less"], true);
        assert_eq!(v["ledger_path"], root.display().to_string());
        assert_eq!(v["active_run"], serde_json::Value::Null);
        // 고아 running이 노출돼야 한다(복구 후보)
        assert_eq!(v["running_runs"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn run_start_resumes_same_connection_and_rom_without_new_run() {
        let (dir, _g) = temp_env();
        let root = dir.path();
        // 세션 시작: run R1
        let s1 = EmucapTrack::new();
        let r = s1
            .run_start(Parameters(TrackRunStartArgs {
                rom_sha1: "sha_a".into(),
                connection_ref: Some("port:1".into()),
                goal: None,
                description: None,
                tags: vec![],
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let r1 = v["run_id"].as_str().unwrap().to_string();
        assert!(v.get("resumed").is_none(), "첫 run_start은 resume 아님");

        // /mcp 재연결 흉내: in-memory active가 사라진 새 서버 인스턴스(같은 원장)
        let s2 = EmucapTrack::new();
        let r = s2
            .run_start(Parameters(TrackRunStartArgs {
                rom_sha1: "sha_a".into(),
                connection_ref: Some("port:1".into()),
                goal: None,
                description: None,
                tags: vec![],
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // 같은 run을 resume — 새 run 만들지 않음
        assert_eq!(v["resumed"], true);
        assert_eq!(v["run_id"], r1);
        assert_eq!(s2.active_run.lock().unwrap().as_ref().unwrap().run_id, r1);
        // 디스크: run 1개뿐이고 여전히 running(파편화·supersede 없음)
        let runs = emucap::track::store::walk_runs(root).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, emucap::track::model::RunStatus::Running);

        // 이어쓰기 동작 확인: resume 후 log_metric 성공
        let r = s2
            .log_metric(Parameters(LogMetricArgs {
                key: "frames".into(),
                value: 9.0,
            }))
            .await;
        assert_ne!(r.is_error, Some(true));
    }

    #[tokio::test]
    async fn run_start_supersedes_on_rom_mismatch_same_connection() {
        let (dir, _g) = temp_env();
        let root = dir.path();
        let s1 = EmucapTrack::new();
        let r = s1
            .run_start(Parameters(TrackRunStartArgs {
                rom_sha1: "sha_a".into(),
                connection_ref: Some("port:1".into()),
                goal: None,
                description: None,
                tags: vec![],
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let r1 = v["run_id"].as_str().unwrap().to_string();

        // 같은 connection이지만 다른 rom → resume 아님(기존 supersede 경로 #56)
        let s2 = EmucapTrack::new();
        let r = s2
            .run_start(Parameters(TrackRunStartArgs {
                rom_sha1: "sha_b".into(),
                connection_ref: Some("port:1".into()),
                goal: None,
                description: None,
                tags: vec![],
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let r2 = v["run_id"].as_str().unwrap().to_string();
        assert!(v.get("resumed").is_none(), "rom 다르면 resume 아님");
        assert_ne!(r1, r2);
        // R1은 superseded(aborted), R2는 running
        assert_eq!(
            emucap::track::store::load_run(root, "sha_a", &r1)
                .unwrap()
                .status,
            emucap::track::model::RunStatus::Aborted
        );
        assert_eq!(
            emucap::track::store::load_run(root, "sha_b", &r2)
                .unwrap()
                .status,
            emucap::track::model::RunStatus::Running
        );
    }

    #[tokio::test]
    async fn run_resume_rebinds_running_run_and_rejects_finished() {
        let (dir, _g) = temp_env();
        let root = dir.path();
        let s1 = EmucapTrack::new();
        let r = s1
            .run_start(Parameters(TrackRunStartArgs {
                rom_sha1: "sha_a".into(),
                connection_ref: Some("port:1".into()),
                goal: None,
                description: None,
                tags: vec![],
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let r1 = v["run_id"].as_str().unwrap().to_string();

        // 재연결: 새 서버, active 없음 → log_metric 에러
        let s2 = EmucapTrack::new();
        let r = s2
            .log_metric(Parameters(LogMetricArgs {
                key: "k".into(),
                value: 1.0,
            }))
            .await;
        assert_eq!(r.is_error, Some(true));

        // run_resume로 명시 재바인딩
        let r = s2
            .run_resume(Parameters(RunResumeArgs { run_id: r1.clone() }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(v["resumed"], true);
        assert_eq!(v["run_id"], r1);
        // 이제 log_metric 성공(이어쓰기)
        let r = s2
            .log_metric(Parameters(LogMetricArgs {
                key: "frames".into(),
                value: 5.0,
            }))
            .await;
        assert_ne!(r.is_error, Some(true));
        let run = emucap::track::store::load_run(root, "sha_a", &r1).unwrap();
        assert!(run
            .metrics
            .iter()
            .any(|m| m.key == "frames" && m.value == 5.0));

        // 종료된 run은 resume 거부
        s2.run_finish(Parameters(RunFinishArgs {
            status: Some("done".into()),
            run_id: None,
        }))
        .await;
        let r = s2
            .run_resume(Parameters(RunResumeArgs { run_id: r1.clone() }))
            .await;
        assert_eq!(r.is_error, Some(true));
    }

    #[tokio::test]
    async fn bootstrap_reports_ledger_path_source() {
        // temp_env는 EMUCAP_TRACK_ROOT를 설정하므로 source=env, 경고 없음
        let (_dir, _g) = temp_env();
        let s = EmucapTrack::new();
        let r = s.bootstrap().await;
        let v: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(v["ledger_path_source"], "env");
        assert!(v.get("ledger_path_warning").is_none());
    }

    #[tokio::test]
    async fn log_finding_requires_rom_or_active() {
        let (_dir, _g) = temp_env();
        let s = EmucapTrack::new();
        // active도 rom_sha1도 없으면 에러
        let r = s
            .log_finding(Parameters(LogFindingArgs {
                rom_sha1: None,
                claim: "x".into(),
                evidence_refs: vec![],
                promoted: false,
            }))
            .await;
        assert_eq!(r.is_error, Some(true));
        // 명시 rom_sha1이면 active 없어도 기록
        let r = s
            .log_finding(Parameters(LogFindingArgs {
                rom_sha1: Some("sha_f".into()),
                claim: "promoted claim".into(),
                evidence_refs: vec![],
                promoted: true,
            }))
            .await;
        assert_ne!(r.is_error, Some(true));
    }
}
