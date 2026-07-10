use std::sync::{Arc, Mutex};
use std::time::Duration;

use emucap::analysis::bisect::{self, CmpOp, Predicate};
use emucap::live::broker_link;
use emucap::live::continuity;
use emucap::live::link::{EmulatorLink, LinkError};
use emucap::live::tcp;
use emucap::live::tools::{self, ToolOutput};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};

#[path = "emucap-mcp/args.rs"]
mod args;
#[path = "emucap-mcp/instructions.rs"]
mod instructions;
#[path = "emucap-mcp/launch.rs"]
mod launch;
#[path = "emucap-mcp/regression.rs"]
mod regression;
#[path = "emucap-mcp/result.rs"]
mod result;
#[path = "emucap-mcp/status.rs"]
mod status;

#[cfg(test)]
#[path = "emucap-mcp/tests.rs"]
mod tests;

use crate::args::*;
use crate::instructions::SERVER_INSTRUCTIONS;
use crate::launch::{make_launch, make_launch_plan, occupied_graceful};
use crate::regression::{
    default_session_port, ensure_capabilities_loaded, parse_observe_spec, require_method,
    run_one_case, verify_determinism_core, DetOutcome,
};
use crate::result::{err_result, output_result, track_err};
use crate::status::{
    enrich_link_status, enrich_status_value, make_bootstrap_value, normalize_rom_sha1,
};

type SharedLink = Arc<Mutex<dyn EmulatorLink + Send>>;

#[derive(Clone)]
struct Emucap {
    link: SharedLink,
    tool_router: ToolRouter<Emucap>,
}

#[tool_router(router = tool_router)]
impl Emucap {
    fn new(link: SharedLink) -> Self {
        Self {
            link,
            tool_router: Self::tool_router(),
        }
    }

    /// 포이즌 내성 lock. 한 도구가 lock을 쥔 채 panic하면 뮤텍스가 poisoned되는데, 이후 모든
    /// `lock().unwrap()`이 panic해 서버 전체가 죽고 세션 재시작을 강요한다. poison을 무시하고
    /// 가드를 회수해 서버를 살린다 — 링크 상태가 어긋났어도 다음 호출의 ensure_connected/raw_call이
    /// 재동기화한다(죽은 conn이면 비우고 재수락).
    fn link(&self) -> std::sync::MutexGuard<'_, dyn EmulatorLink + Send + 'static> {
        self.link.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[tool(
        description = "emucap 작업의 첫 진입점 — 무엇을 켤지 모를 때 가장 먼저 호출한다. 에뮬레이터가 없어도 listener를 세우고 listening_port·runtime_paths·지원 시스템·물어볼 다음 질문을 반환한다."
    )]
    async fn bootstrap(&self) -> CallToolResult {
        let mut link = self.link();
        match make_bootstrap_value(&mut *link) {
            Ok(v) => output_result(ToolOutput::Json(v)),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "ROM/disc/disk를 어느 adapter로 띄울지 계획한다 — launcher 절대경로·argv·listening_port를 반환한다(미디어가 애매하면 추측 대신 물어볼 질문을 반환)."
    )]
    async fn launch_plan(&self, Parameters(a): Parameters<LaunchPlanArgs>) -> CallToolResult {
        let mut link = self.link();
        match make_bootstrap_value(&mut *link) {
            Ok(bootstrap) => {
                let port = bootstrap
                    .get("listening_port")
                    .and_then(|v| v.as_u64())
                    .and_then(|p| u16::try_from(p).ok());
                let mut plan = make_launch_plan(port, &a);
                if let Some(obj) = plan.as_object_mut() {
                    if bootstrap
                        .get("occupied_by_foreign")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        obj.insert("warning".into(), serde_json::json!(
                            "이 listening_port를 다른 에뮬레이터가 점유 중이다(bootstrap.occupant 참조). 이 plan의 launch를 그대로 실행하기 전에 bootstrap.recovery를 따라 점유를 해소하라."
                        ));
                    }
                    obj.insert("bootstrap".into(), bootstrap);
                }
                output_result(ToolOutput::Json(plan))
            }
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "resolved 어댑터로 에뮬레이터를 직접 띄운다(크로스플랫폼 Rust 런처) — Mesen·Mednafen·Flycast·MAME PC-98을 detached spawn하고 pid를 반환한다. 몇 초 뒤 status로 connected를 확인하라."
    )]
    async fn launch(&self, Parameters(a): Parameters<LaunchArgs>) -> CallToolResult {
        let mut link = self.link();
        output_result(ToolOutput::Json(make_launch(&mut *link, &a)))
    }

    #[tool(description = "실행 중 게임의 메모리 범위를 읽는다")]
    async fn read_memory(&self, Parameters(a): Parameters<ReadMemoryArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::read_memory(&mut *link, &a.memory_type, a.address.get(), a.length.get()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "세이브스테이트 로드→frame 진행→메모리 읽기를 한 어댑터 명령으로 원자 수행한다(bisect/regression의 결정론 경로). 진행 중 BP 히트 시 측정 무효라 {status:interrupted}로 닫는다."
    )]
    async fn probe(&self, Parameters(a): Parameters<ProbeArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::probe(
            &mut *link,
            &a.state,
            a.frame,
            &a.memory_type,
            a.address.get(),
            a.length.get(),
        ) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "메모리 영역에서 hex 패턴을 어댑터 내부 스캔해 매칭 오프셋만 반환한다 — 런타임 문자열/버퍼/테이블 위치 특정(대용량을 read로 안 뜨고)."
    )]
    async fn find_pattern(&self, Parameters(a): Parameters<FindPatternArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::find_pattern(
            &mut *link,
            &a.memory_type,
            &a.hex,
            a.start.map(Num::get).unwrap_or(0),
            a.length.map(Num::get),
            a.max_matches,
            a.align,
        ) {
            Ok(ToolOutput::Json(v)) => match a.output_path.as_deref() {
                Some(p) => match emucap::offload::offload_result(&v, std::path::Path::new(p)) {
                    Ok(s) => output_result(ToolOutput::Json(s)),
                    Err(e) => err_result(LinkError::Protocol(e)),
                },
                None => output_result(ToolOutput::Json(v)),
            },
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "현재 화면을 캡처한다")]
    async fn screenshot(&self, Parameters(a): Parameters<ScreenshotArgs>) -> CallToolResult {
        let mut link = self.link();
        let path = a.save_path.as_ref().map(std::path::Path::new);
        match tools::screenshot(&mut *link, path) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "에뮬레이터 상태 레지스터를 읽는다(groups로 필터, 생략 시 전체 — groups 미지원 백엔드는 무시하고 전체 반환)."
    )]
    async fn get_state(&self, Parameters(a): Parameters<StateArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::get_state(&mut *link, &a.groups, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Saturn VDP2 비디오 상태를 레이어별(NBG0~3·RBG·common)로 디코드해 반환한다. 반환 필드·공식·char base 보정은 어댑터 README 참조."
    )]
    async fn get_video_state(&self) -> CallToolResult {
        let mut link = self.link();
        match tools::get_video_state(&mut *link) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Saturn 화면좌표(NBG, x, y)를 그 셀의 char 데이터 주소로 푼다(스크롤→맵셀→PNT→charno→주소를 렌더러 공식으로 접는다). 중간값 동봉; 필드·공식·char-base 보정은 usage."
    )]
    async fn resolve_tile(&self, Parameters(a): Parameters<ResolveTileArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::resolve_tile(&mut *link, a.nbg, a.x, a.y) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "비디오 레이어 enable 마스크를 토글한다 — 텍스트 라우팅 판정·클린플레이트 캡처용(layers·mask 둘 다 생략하면 현재 상태 조회). ⚠ override라 지속되니 분석 후 layers에 전체 이름을 줘 반드시 복원하라."
    )]
    async fn set_layer_enable(
        &self,
        Parameters(a): Parameters<SetLayerEnableArgs>,
    ) -> CallToolResult {
        let mut link = self.link();
        let layers = a.layers.unwrap_or_default();
        match tools::set_layer_enable(&mut *link, &layers, a.mask) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "실행 중 콘텐츠 신원을 읽는다 — name/path/size/media_type + 균일 `rom_sha1` 해시. 그 `rom_sha1`을 추적 MCP run_start에 그대로 넘긴다(없으면 `shasum -a1 <content>` 폴백)."
    )]
    async fn get_rom_info(&self) -> CallToolResult {
        let mut link = self.link();
        match tools::get_rom_info(&mut *link) {
            Ok(ToolOutput::Json(mut v)) => {
                normalize_rom_sha1(&mut v);
                output_result(ToolOutput::Json(v))
            }
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "떠 있는 에뮬레이터 연결 상태를 읽는다(미연결이면 listening_port·runtime_paths 반환). 새 작업이나 무엇을 켤지 모르면 status가 아니라 bootstrap을 먼저 호출한다."
    )]
    async fn status(&self) -> CallToolResult {
        let mut link = self.link();
        match tools::status(&mut *link) {
            Ok(ToolOutput::Json(mut v)) => {
                let port = link.endpoint_port();
                let token = link.session_token().map(str::to_string);
                let identity = link.capabilities().identity.clone();
                let methods = link.capabilities().methods.clone();
                let memory_types = link.capabilities().memory_types.clone();
                enrich_status_value(&mut v, &methods, &memory_types, identity.system.as_deref());
                enrich_link_status(&mut v, port, token.as_deref(), Some(&identity));
                status::enrich_continuity(&mut v, &*link);
                v["request_succeeded"] = serde_json::json!(true);
                output_result(ToolOutput::Json(v))
            }
            Ok(o) => output_result(o),
            Err(LinkError::NotConnected) => {
                // 미연결: 이 서버가 잡은 포트를 알려준다(에이전트가 거기로 에뮬레이터를 띄우게).
                let port = link.endpoint_port();
                let token = link.session_token().map(str::to_string);
                let mut v = serde_json::json!({
                    "connected": false,
                    "server_build": status::BUILD_HASH,
                    "listening_port": port,
                    "first_tool_if_unknown": "bootstrap",
                    "start_new_task_with": "bootstrap",
                    "required_user_input_if_content_unknown": "실행할 content_path와 시스템(snes/saturn/psx/pce/md/pc98/dc)을 물어본 뒤 launch_plan(content_path, system)을 호출하라",
                    "question_to_user_if_content_unknown": "어떤 ROM/disc/disk 경로를 어떤 시스템(snes/saturn/psx/pce/md/pc98/dc)으로 실행할까요?",
                    "workflow": {
                        "unknown_content": {
                            "ask_user": "어떤 ROM/disc/disk 경로를 어떤 시스템(snes/saturn/psx/pce/md/pc98/dc)으로 실행할까요?",
                            "then_call": "launch_plan",
                            "required_args": ["content_path", "system"]
                        },
                        "known_content": {
                            "then_call": "launch_plan",
                            "required_args": ["content_path"],
                            "optional_args": ["system"]
                        },
                        "connected_check_only": {
                            "then_call": "status"
                        }
                    },
                    "next_action": "연결 확인만이면 runtime_paths를 참고한다. 새 launch이고 content_path가 있으면 launch_plan(content_path, system?)을 호출한다. content_path/system을 모르면 사용자에게 question_to_user_if_content_unknown을 물어본다.",
                    "hint": port.map(|p| format!(
                        "무엇을 켤지 모르면 status 응답만으로 추측 실행하지 말고 bootstrap() 또는 launch_plan(content_path, system?)을 먼저 호출하라. \
                         launch 직전에 받은 이 포트({p})를 유지한 상태에서 launch(content_path, system?, name?) 도구를 호출하라. \
                         launch_plan은 preferred_launcher.args와 legacy_fallback_*를 함께 준다. 기본은 MCP launch 도구이고, legacy script는 Rust launch가 해당 호스트에서 막힐 때만 쓴다. \
                         이 status 호출이 listener를 세우고 background accept/hello를 준비하므로 생략하지 말 것. \
                         launch 도구와 legacy launcher는 status.identity_guard.session_token_file.path의 포트 토큰을 전달해 이 MCP 세션 토큰을 맞춘다. \
                         세션 토큰이 없거나 다른 오래된 에뮬레이터 연결은 handshake에서 hard fail 된다. \
                         raw nohup 명령을 직접 조립하지 말고 launch 도구의 로그/결과를 확인하라. 광역 kill 금지."
                    )),
                });
                enrich_link_status(&mut v, port, token.as_deref(), None);
                status::enrich_continuity(&mut v, &*link);
                v["request_succeeded"] = serde_json::json!(false);
                output_result(ToolOutput::Json(v))
            }
            Err(LinkError::IdentityMismatch { identity, .. }) => {
                // 포트를 다른 세션 에뮬이 점유 — 하드에러 대신 graceful(잠금 방지·진입점 계약 유지).
                let port = link.endpoint_port();
                let token = link.session_token().map(str::to_string);
                let mut value = occupied_graceful(&identity, port, token.as_deref());
                status::enrich_continuity(&mut value, &*link);
                output_result(ToolOutput::Json(value))
            }
            Err(e) if status::is_observation_failure(&e) => {
                let port = link.endpoint_port();
                let token = link.session_token().map(str::to_string);
                let mut v = serde_json::json!({
                    "connected": false,
                    "request_succeeded": false,
                    "error_kind": e.kind(),
                    "error": e.to_string(),
                    "listening_port": port,
                });
                enrich_link_status(&mut v, port, token.as_deref(), None);
                status::enrich_continuity(&mut v, &*link);
                output_result(ToolOutput::Json(v))
            }
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "마지막 정상 상태와 transport/adapter 실패 캡슐을 읽는다. 에뮬레이터에 요청하지 않으므로 연결이 끊긴 뒤에도 동작한다."
    )]
    async fn get_failure_context(&self) -> CallToolResult {
        let mut link = self.link();
        output_result(ToolOutput::Json(link.failure_context()))
    }

    #[tool(
        description = "fatal quarantine의 보존 상태를 해제하고 에뮬레이터의 기존 종료 경로를 계속한다. status.methods에 dismiss_failure가 있을 때만 사용한다."
    )]
    async fn dismiss_failure(&self) -> CallToolResult {
        let mut link = self.link();
        if !link
            .capabilities()
            .methods
            .iter()
            .any(|method| method == "dismiss_failure")
        {
            return err_result(LinkError::Emulator {
                kind: "unsupported".into(),
                message: "connected adapter does not advertise dismiss_failure".into(),
            });
        }
        match tools::dismiss_failure(&mut *link) {
            Ok(output) => output_result(output),
            Err(error) => err_result(error),
        }
    }

    #[tool(description = "메모리에 바이트(hex)를 쓴다")]
    async fn write_memory(&self, Parameters(a): Parameters<WriteMemoryArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::write_memory(&mut *l, &a.memory_type, a.address.get(), &a.hex) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "컨트롤러/키 입력을 누른 채 유지한다 — 빈 배열 set_input으로 해제할 때까지 지속(running·frozen 무관). 버튼명은 status.input_buttons가 정본."
    )]
    async fn set_input(&self, Parameters(a): Parameters<InputArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_input(&mut *l, a.port, &a.buttons) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "버튼/키를 frames만큼 실시간으로 눌렀다 뗀다(frozen이면 자동 resume). 버튼명은 status.input_buttons. frozen 유지 결정론 1칸/1회는 tap."
    )]
    async fn press_buttons(&self, Parameters(a): Parameters<PressArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::press_buttons(&mut *l, a.port, &a.buttons, a.frames) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "하단 터치스크린을 (x,y)에서 터치한다 — release=true면 뗀다, frames면 그만큼 눌렀다 자동으로 뗀다(탭), 둘 다 없으면 hold. 터치스크린 있는 시스템(NDS 등)에서만 동작; status.methods 정본."
    )]
    async fn touch(&self, Parameters(a): Parameters<TouchArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::touch(&mut *l, a.port, a.x, a.y, a.frames, a.release) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "프레임 단위 정밀 탭 — frozen에서 auto-repeat 없이 1칸/1회 입력 후 뗀다(호출 후 frozen 유지). 버튼명은 status.input_buttons."
    )]
    async fn tap(&self, Parameters(a): Parameters<TapArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::tap(&mut *l, a.port, &a.buttons, a.press_frames, a.after_frames) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "여러 탭을 한 콜에 순차 입력한다(메뉴/텍스트 네비게이션 왕복 절감; 전부 frozen 결정론, 호출 후 frozen 유지)."
    )]
    async fn tap_sequence(&self, Parameters(a): Parameters<TapSequenceArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::tap_sequence(&mut *l, a.port, &a.steps, a.press_frames) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "버튼을 누른 채 frozen 진행하며 watch 메모리가 바뀌면 멈추고 뗀다 — 타일/커서 이동을 결정론적으로(입력 효과 피드백)."
    )]
    async fn hold_until(&self, Parameters(a): Parameters<HoldUntilArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::hold_until(
            &mut *l,
            a.port,
            &a.buttons,
            &a.memory_type,
            a.address.get(),
            a.length.get(),
            a.max_frames,
        ) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "세이브스테이트를 파일로 저장한다")]
    async fn save_state(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::save_state(&mut *l, &a.path) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "파일에서 세이브스테이트를 로드한다")]
    async fn load_state(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::load_state(&mut *l, &a.path) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "N프레임 진행한다 — 항상 running으로 free-run이라 정밀 캡처엔 부적합(정확 N프레임은 pause→step). 진행 중 BP 히트 시 {status:interrupted}로 반환(poll_events로 드레인)."
    )]
    async fn run_frames(&self, Parameters(a): Parameters<RunFramesArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::run_frames(&mut *l, a.n) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "다음 프레임 경계에서 일시정지(freeze)한다")]
    async fn pause(&self, Parameters(a): Parameters<CpuArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::pause(&mut *l, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "일시정지 상태에서 N프레임 진행 후 재정지한다(frozen에서만). 명령 단위로 좁히려면 step_instructions"
    )]
    async fn step(&self, Parameters(a): Parameters<StepArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::step(&mut *l, a.frames, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "일시정지 상태에서 N개 CPU 명령 진행 후 재정지한다(frozen에서만 — 가용성 status.methods). derail 직전을 1명령씩 좁힐 때 — 1프레임은 수천 명령이라 프레임 step으론 부족하다."
    )]
    async fn step_instructions(
        &self,
        Parameters(a): Parameters<StepInstructionsArgs>,
    ) -> CallToolResult {
        let mut l = self.link();
        match tools::step_instructions(&mut *l, a.count, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "정상 실행으로 복귀한다")]
    async fn resume(&self, Parameters(a): Parameters<CpuArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::resume(&mut *l, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "게임을 리셋한다(처음부터 재시작; 로드된 ROM 바이트는 그대로).")]
    async fn reset(&self) -> CallToolResult {
        let mut l = self.link();
        match tools::reset(&mut *l) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "메모리 접근/실행에 브레이크포인트를 건다(히트 시 freeze). kind·pc/value 필터·snapshot 등 옵션은 인자 doc 참조(미지원 kind는 거부 에러)."
    )]
    async fn set_breakpoint(&self, Parameters(a): Parameters<BreakpointArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_breakpoint(
            &mut *l,
            &a.kind,
            &a.memory_type,
            a.start.get(),
            a.end.get(),
            a.pause_on_hit,
            a.auto_savestate,
            a.pc_min.map(Num::get),
            a.pc_max.map(Num::get),
            a.value.map(Num::get),
            a.value_mask.map(Num::get),
            a.value_len.map(Num::get),
            &a.snapshot,
        ) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "address부터 count개 명령을 디코드한다(연결된 코어의 ISA) — BP 히트 PC 주변 명령 확인용."
    )]
    async fn disassemble(&self, Parameters(a): Parameters<DisassembleArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::disassemble(&mut *l, a.address.get(), a.count) {
            Ok(ToolOutput::Json(v)) => match a.output_path.as_deref() {
                Some(p) => match emucap::offload::offload_result(&v, std::path::Path::new(p)) {
                    Ok(s) => output_result(ToolOutput::Json(s)),
                    Err(e) => err_result(LinkError::Protocol(e)),
                },
                None => output_result(ToolOutput::Json(v)),
            },
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "register가 [min,max]를 벗어나는 명령에서 freeze한다(SP 폭주 등 derail 포착). 매 명령 검사라 hunting 전용 — 끝나면 clear."
    )]
    async fn watch_register(&self, Parameters(a): Parameters<WatchRegisterArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::watch_register(
            &mut *l,
            &a.register,
            a.min.get(),
            a.max.get(),
            a.pause_on_hit,
            a.max_instructions,
        ) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "브레이크포인트를 해제한다")]
    async fn clear_breakpoint(&self, Parameters(a): Parameters<ClearBpArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::clear_breakpoint(&mut *l, a.id) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "활성 브레이크포인트 목록을 반환한다(id·kind·범위).")]
    async fn list_breakpoints(&self) -> CallToolResult {
        let mut l = self.link();
        match tools::list_breakpoints(&mut *l) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "모든 브레이크포인트를 해제한다(정리용)")]
    async fn clear_all_breakpoints(&self) -> CallToolResult {
        let mut l = self.link();
        match tools::clear_all_breakpoints(&mut *l) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "쌓인 이벤트(브레이크포인트 히트 등)를 드레인한다")]
    async fn poll_events(&self, Parameters(a): Parameters<PollEventsArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::poll_events(&mut *l) {
            Ok(ToolOutput::Json(v)) => match a.output_path.as_deref() {
                Some(p) => match emucap::offload::offload_result(&v, std::path::Path::new(p)) {
                    Ok(s) => output_result(ToolOutput::Json(s)),
                    Err(e) => err_result(LinkError::Protocol(e)),
                },
                None => output_result(ToolOutput::Json(v)),
            },
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "실행추적을 켜고/끈다 — 콜스택·트레이스 링버퍼 유지(크래시 추적용). 매 명령이라 hunting 전용, 끝나면 끈다."
    )]
    async fn set_trace(&self, Parameters(a): Parameters<SetTraceArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_trace(&mut *l, a.enabled) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "최근 N개 실행 명령을 시간순으로 반환한다(`[{pc, op, bank?}]`; set_trace(true) 선행). `bank`은 pc가 페이징된 ROM 뱅크(Mesen GG/GB만). 없거나 null이면 뱅크 미확정(MBC1 mode-1 저역·비표준 매퍼 등). 카트가 태깅하는지는 `status.bank_tagging`."
    )]
    async fn get_trace(&self, Parameters(a): Parameters<GetTraceArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::get_trace(&mut *l, a.count) {
            Ok(ToolOutput::Json(v)) => match a.output_path.as_deref() {
                Some(p) => match emucap::offload::offload_result(&v, std::path::Path::new(p)) {
                    Ok(s) => output_result(ToolOutput::Json(s)),
                    Err(e) => err_result(LinkError::Protocol(e)),
                },
                None => output_result(ToolOutput::Json(v)),
            },
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "현재 콜스택(호출지 프레임 체인 `[{pc, bank}]`, 바깥→안)을 반환한다 — \"어떻게 여기 왔나\" 즉답(set_trace(true) 선행). `bank`은 pc가 페이징된 ROM 뱅크(Mesen GG/GB만). `bank`이 없거나 null이면 그 주소의 뱅크 미확정(SNES는 24비트 pc 안, 또는 MBC1 mode-1 저역·비표준 매퍼라 추정불가). 카트가 뱅크를 태깅하는지는 `status.bank_tagging`. `.pc`가 유일 보장 필드."
    )]
    async fn call_stack(&self) -> CallToolResult {
        let mut l = self.link();
        match tools::call_stack(&mut *l) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "게임이 리셋 핸들러를 실행하면 freeze한다 — 워치독 리셋·하드 크래시→리셋 자동 감지."
    )]
    async fn break_on_reset(&self, Parameters(a): Parameters<BreakOnResetArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::break_on_reset(&mut *l, a.enabled) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "표준 메모리 리전을 .bin+regions.json+state.json으로 덤프한다(emucap diff 입력). 유효 리전은 status.memory_types."
    )]
    async fn dump_memory(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::dump_memory(&mut *l, &a.path) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "베이스 세이브스테이트에서 타깃이 처음 나빠지는 프레임을 이분 탐색한다")]
    async fn bisect(&self, Parameters(a): Parameters<BisectArgs>) -> CallToolResult {
        let op = match CmpOp::parse(&a.op) {
            Ok(o) => o,
            Err(e) => return err_result(LinkError::Protocol(e)),
        };
        let pred = Predicate {
            memory_type: a.memory_type,
            address: a.address.get(),
            length: a.length,
            op,
            value: a.value.get(),
        };
        let mut l = self.link();
        if let Err(e) = require_method(&mut *l, "probe", "bisect") {
            return err_result(e);
        }
        match bisect::run_bisect(&mut *l, &a.state, a.lo, a.hi, &pred) {
            Ok(r) => CallToolResult::success(vec![Content::text(
                serde_json::to_string(&r).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
            )]),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "회귀 스위트를 일괄 재생해 케이스별 PASS/FAIL·무효 버킷을 요약해 반환한다(원장 기록 안 함). 결과를 남기려면 추적 MCP의 log_gate/log_metric으로 기록하라."
    )]
    async fn regression_run(&self, Parameters(a): Parameters<RegressionRunArgs>) -> CallToolResult {
        let suite = std::path::PathBuf::from(&a.suite_dir);
        let cases = match regression::load_suite(&suite) {
            Ok(c) => c,
            Err(e) => return err_result(LinkError::Protocol(e)),
        };
        let mut l = self.link();
        if let Err(e) = ensure_capabilities_loaded(&mut *l) {
            return err_result(e);
        }
        let rom_check_unsupported = !l.capabilities().methods.iter().any(|m| m == "get_rom_info");
        let mut results = Vec::new();
        for (dir, case) in &cases {
            let verdict = run_one_case(&mut *l, dir, case);
            results.push(regression::CaseResult {
                id: case.id.clone(),
                verdict,
            });
        }
        // 스위트 종료 후 실행 상태로 복원 (frozen 상태 정리)
        let _ = l.call("resume", serde_json::json!({}));
        let summary = regression::Summary::from_results(results);
        let body = serde_json::json!({
            "passed": summary.passed, "failed": summary.failed, "invalid": summary.invalid,
            "ok": summary.ok(),
            "rom_check_unsupported": rom_check_unsupported,
            "cases": summary.results.iter()
                .map(|r| serde_json::json!({"id": r.id, "verdict": r.verdict.code()}))
                .collect::<Vec<_>>(),
        });
        CallToolResult::success(vec![Content::text(body.to_string())])
    }

    #[tool(
        description = "케이스 재현 레시피를 N회 재생해 관측 해시 일치로 harness 재현성을 잰다(게임/엔진 결정론 아님). 원장 기록 안 함 — 결과는 log_gate로. observe·한계는 인자 doc·usage."
    )]
    async fn verify_determinism(
        &self,
        Parameters(a): Parameters<VerifyDeterminismArgs>,
    ) -> CallToolResult {
        self.verify_determinism_impl(a)
    }

    fn verify_determinism_impl(&self, a: VerifyDeterminismArgs) -> CallToolResult {
        let replays = a.replays.unwrap_or(2);
        if !(2..=5).contains(&replays) {
            return track_err("replays는 2~5");
        }
        let observe = match parse_observe_spec(
            a.observe.as_deref(),
            a.memory_type.clone(),
            a.address.map(|n| n.get()),
            a.length.map(|n| n.get()),
        ) {
            Ok(o) => o,
            Err(e) => return track_err(e),
        };
        let dir = std::path::PathBuf::from(&a.case_dir);
        let case = match regression::load_case(&dir) {
            Ok(c) => c,
            Err(e) => return err_result(LinkError::Protocol(e)),
        };

        let result = {
            let mut l = self.link();
            if let Err(e) = ensure_capabilities_loaded(&mut *l) {
                return err_result(e);
            }
            let r = verify_determinism_core(&mut *l, &dir, &case, &observe, replays);
            // frozen 정리(실행 상태 복원)
            let _ = l.call("resume", serde_json::json!({}));
            r
        };

        // 단일-writer: 원장에 쓰지 않고 결과만 반환한다. 에이전트가 추적 MCP의
        // log_gate(determinism_replay, machine, passed)로 기록한다.
        let body = serde_json::json!({
            "outcome": result.outcome.code(),
            "reproducible": result.outcome == DetOutcome::Reproducible,
            "passed": result.outcome.passed(),
            "observe_kind": result.observe_kind,
            "replays": result.replays,
            "hashes": result.hashes,
            "case_id": case.id,
            "note": "측정 범위: 이 harness 경로의 재현성(게임/엔진 결정론 아님; 시작-gap·동일 프로세스 엔트로피 한계). 결과 기록은 추적 MCP의 log_gate(name=determinism_replay, kind=machine, passed)로.",
        });
        CallToolResult::success(vec![Content::text(body.to_string())])
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Emucap {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(SERVER_INSTRUCTIONS.into());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let emu_port: u16 = std::env::var("EMUCAP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(47800);
    let broker_mode = std::env::var("EMUCAP_BROKER")
        .map(|v| v == "1")
        .unwrap_or(false);

    let link: SharedLink = if broker_mode {
        let sess_port: u16 = std::env::var("EMUCAP_BROKER_SESSION_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default_session_port(emu_port));
        let sess_addr = format!("127.0.0.1:{sess_port}");
        let name = std::env::var("EMUCAP_NAME").ok();
        // broker 없으면 auto-spawn 후 lazy link로 접속을 미룬다.
        // 직접 모드로 폴백하지 않는다 — opt-in했으면 broker가 정본(스펙).
        if let Ok(exe) = std::env::current_exe() {
            let broker_bin = exe.with_file_name("emucap-broker");
            let _ = std::process::Command::new(broker_bin).spawn();
        }
        Arc::new(Mutex::new(continuity::observed(broker_link::lazy(
            &sess_addr,
            name,
            Duration::from_secs(5),
        ))))
    } else {
        // 직접 모드(기본): 지연 바인드로 포트를 즉시 잡지 않아 MCP 핸드셰이크가 항상 성공하고,
        // 다른 인스턴스가 포트를 쥐고 있어도 서버가 죽지 않는다.
        Arc::new(Mutex::new(continuity::observed(tcp::lazy(
            &format!("127.0.0.1:{emu_port}"),
            Duration::from_secs(5),
        ))))
    };

    let server = Emucap::new(link);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
