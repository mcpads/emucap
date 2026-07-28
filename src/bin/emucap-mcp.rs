use std::sync::{Arc, Mutex};
use std::time::Duration;

use emucap::live::broker_link;
use emucap::live::continuity;
use emucap::live::link::{EmulatorLink, LinkError};
use emucap::live::tcp;
use emucap::live::tools::{self, ToolOutput};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};

#[path = "emucap-mcp/args.rs"]
mod args;
#[path = "emucap-mcp/instructions.rs"]
mod instructions;
#[path = "emucap-mcp/launch.rs"]
mod launch;
#[path = "emucap-mcp/memory_write.rs"]
mod memory_write;
#[path = "emucap-mcp/regression.rs"]
mod regression;
#[path = "emucap-mcp/result.rs"]
mod result;
#[path = "emucap-mcp/status.rs"]
mod status;
#[path = "emucap-mcp/stop.rs"]
mod stop;

#[cfg(test)]
#[path = "emucap-mcp/tests.rs"]
mod tests;

use crate::args::*;
use crate::instructions::SERVER_INSTRUCTIONS;
use crate::launch::{make_launch, make_launch_plan, occupied_graceful};
use crate::regression::{
    default_session_port, ensure_capabilities_loaded, parse_observe_spec, run_one_case,
    verify_determinism_core, DetOutcome,
};
use crate::result::{err_result, output_result, track_err};
use crate::status::{
    enrich_breakpoint_kinds, enrich_link_status, enrich_status_value, make_bootstrap_value,
    normalize_rom_sha1,
};
use crate::stop::make_stop;

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
        description = "Start here for emucap work, especially when the content or system is unknown. Establishes a listener even without an emulator and returns listening_port, runtime_paths, supported systems, and the next question when user input is required."
    )]
    async fn bootstrap(&self) -> CallToolResult {
        let mut link = self.link();
        match make_bootstrap_value(&mut *link) {
            Ok(v) => output_result(ToolOutput::Json(v)),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Plan which adapter should launch a ROM, disc, or disk. Returns the absolute launcher path, argv, and listening_port. For ambiguous media, returns a question instead of guessing."
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
                            "Another emulator occupies this listening_port; inspect bootstrap.occupant and follow bootstrap.recovery before executing this launch plan."
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
        description = "Launch an emulator with the resolved adapter. The cross-platform Rust launcher starts the supported emulator and any required bridge as detached processes, waits for adapter readiness, and returns process identity and launch outcome."
    )]
    async fn launch(&self, Parameters(a): Parameters<LaunchArgs>) -> CallToolResult {
        let mut link = self.link();
        output_result(ToolOutput::Json(make_launch(&mut *link, &a)))
    }

    #[tool(
        description = "Terminate one exact managed launch generation and all of its owned helper processes. Requires status.runtime_instance.launch_id, verifies the current generation, control lease, and process-start identities, preserves failure evidence, and never terminates by executable name. This ends the emulator process; use pause to freeze guest execution."
    )]
    async fn stop(&self, Parameters(a): Parameters<StopArgs>) -> CallToolResult {
        let mut link = self.link();
        output_result(ToolOutput::Json(make_stop(&mut *link, &a)))
    }

    #[tool(description = "Read a byte range from the running game's memory.")]
    async fn read_memory(&self, Parameters(a): Parameters<ReadMemoryArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::read_memory(&mut *link, &a.memory_type, a.address.get(), a.length.get()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Load a savestate, advance by a frame count, and read memory as one adapter operation. This is the replay path for frame-boundary search and regression. A breakpoint hit invalidates the measurement and returns status:interrupted."
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
        description = "Scan a memory region inside the adapter for a hexadecimal pattern and return matching offsets. Use it to locate runtime strings, buffers, or tables without transferring the whole region through read_memory."
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

    #[tool(
        description = "Capture the current screen and return PNG data with SHA-256, byte length, and any frame, state, and freshness provenance supplied by the backend."
    )]
    async fn screenshot(&self, Parameters(a): Parameters<ScreenshotArgs>) -> CallToolResult {
        let mut link = self.link();
        let path = a.save_path.as_ref().map(std::path::Path::new);
        match tools::screenshot(&mut *link, path) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Read emulator state registers. Filter with groups or omit them for all state. Backends without group filtering ignore the filter and return all available state."
    )]
    async fn get_state(&self, Parameters(a): Parameters<StateArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::get_state(&mut *link, &a.groups, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Decode and return Saturn VDP2 video state by layer (NBG0-3, RBG, and common state). See the adapter README for returned fields, formulas, and character-base correction."
    )]
    async fn get_video_state(&self) -> CallToolResult {
        let mut link = self.link();
        match tools::get_video_state(&mut *link) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Resolve a Saturn NBG screen coordinate to the cell's character-data address through scroll, map cell, pattern-name data, and character number. Returns intermediate values; see the adapter documentation for field formulas and character-base correction."
    )]
    async fn resolve_tile(&self, Parameters(a): Parameters<ResolveTileArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::resolve_tile(&mut *link, a.nbg, a.x, a.y) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Set or query the video-layer enable mask for routing analysis and clean-plate capture. Omit both layers and mask to query. This is a persistent override; restore the complete layer set after analysis."
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
        description = "Read the running content identity: name, path, size, media_type, and normalized rom_sha1. Pass rom_sha1 unchanged to Tracking MCP run_start. Use a local SHA-1 fallback only when the backend does not provide it."
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
        description = "Read the current emulator connection state. When disconnected, returns listening_port and runtime_paths. For a new task or unknown content, call bootstrap before status."
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
                let breakpoint_kinds = link.capabilities().breakpoint_kinds.clone();
                let contracts = link.capabilities().contracts.clone();
                enrich_status_value(&mut v, &methods, &memory_types, identity.system.as_deref());
                enrich_breakpoint_kinds(&mut v, &breakpoint_kinds);
                status::enrich_contract_status(&mut v, &identity, &contracts);
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
                let unknown_content_question = status::unknown_content_question();
                let mut v = serde_json::json!({
                    "connected": false,
                    "server_build": status::BUILD_HASH,
                    "listening_port": port,
                    "first_tool_if_unknown": "bootstrap",
                    "start_new_task_with": "bootstrap",
                    "required_user_input_if_content_unknown": status::required_unknown_content_input(),
                    "question_to_user_if_content_unknown": unknown_content_question.clone(),
                    "workflow": {
                        "unknown_content": {
                            "ask_user": unknown_content_question,
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
                    "next_action": "For connection inspection, use runtime_paths. For a new launch with known content_path, call launch_plan(content_path, system?). If content or system is unknown, ask question_to_user_if_content_unknown.",
                    "hint": port.map(|p| format!(
                        "When content is unknown, do not infer a launch from status alone. Call bootstrap() or launch_plan(content_path, system?) first. \
                         Keep the current listener port ({p}) and call launch(content_path, system?, name?) immediately before execution. \
                         launch_plan returns preferred_launcher.args and legacy_fallback_*; prefer the MCP launch tool and use a legacy script only when Rust launch is unavailable on this host. \
                         This status call establishes the listener and prepares background accept and hello, so do not skip it. \
                         The MCP launcher and legacy launcher pass the per-port token from status.identity_guard.session_token_file.path. \
                         A missing token or a stale emulator from another session fails the handshake. \
                         Do not assemble a raw nohup command. Inspect launcher logs and results, and never use a broad process kill."
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
        description = "Read the last known-good status and preserved transport or adapter failure capsule. This does not contact the emulator and remains available after disconnection."
    )]
    async fn get_failure_context(&self) -> CallToolResult {
        let mut link = self.link();
        output_result(ToolOutput::Json(link.failure_context()))
    }

    #[tool(
        description = "Dismiss a preserved fatal quarantine and continue the emulator's original termination path. Use only when status.methods includes dismiss_failure."
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

    #[tool(
        description = "Write bytes to emulator memory. Provide exactly one of hex for a short value or input_file(path, offset, length, sha256?) for a raw binary slice. Control MCP reads and fixes the bounded file slice once and returns its SHA-256."
    )]
    async fn write_memory(&self, Parameters(a): Parameters<WriteMemoryArgs>) -> CallToolResult {
        let generation = if a.input_file.is_some() {
            memory_write::generation_marker(self.link().capabilities())
        } else {
            None
        };
        let prepared = match memory_write::prepare_write(&a).await {
            Ok(prepared) => prepared,
            Err(error) => return err_result(error),
        };
        let mut l = self.link();
        if generation.is_some() && generation != memory_write::generation_marker(l.capabilities()) {
            return err_result(LinkError::Emulator {
                kind: "bad_state".into(),
                message: "runtime generation changed while staging file input; retry against the current status".into(),
            });
        }
        match tools::write_memory_bytes(&mut *l, &a.memory_type, a.address.get(), &prepared.bytes) {
            Ok(o) => output_result(memory_write::with_provenance(o, &prepared)),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Hold controller or key input until released by set_input with an empty button array. Ownership persists in running and frozen states. Read valid button names from status.input_buttons."
    )]
    async fn set_input(&self, Parameters(a): Parameters<InputArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_input(&mut *l, a.port, &a.buttons) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Press buttons or keys for a real-time frame duration and then release them. Automatically resumes when frozen. Read names from status.input_buttons; use tap for deterministic frozen-state single actions."
    )]
    async fn press_buttons(&self, Parameters(a): Parameters<PressArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::press_buttons(&mut *l, a.port, &a.buttons, a.frames) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Control a lower touchscreen at x,y. release:true releases touch; frames performs a timed tap and releases; omitting both holds touch. Use only when this method appears in status.methods."
    )]
    async fn touch(&self, Parameters(a): Parameters<TouchArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::touch(&mut *l, a.port, a.x, a.y, a.frames, a.release) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Perform a frame-precise tap while frozen, then release input and remain frozen. Intended for one action below auto-repeat timing. Read button names from status.input_buttons."
    )]
    async fn tap(&self, Parameters(a): Parameters<TapArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::tap(&mut *l, a.port, &a.buttons, a.press_frames, a.after_frames) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Advance while frozen with buttons held, stop when watched memory changes, and release input. Use for deterministic movement with memory feedback."
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

    #[tool(description = "Save emulator state to a file.")]
    async fn save_state(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::save_state(&mut *l, &a.path) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "Load emulator state from a file.")]
    async fn load_state(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::load_state(&mut *l, &a.path) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Advance for N frames in running free-run mode. This is not frame-exact capture; use pause then step for exact advancement. A breakpoint hit returns status:interrupted; drain its event with poll_events."
    )]
    async fn run_frames(&self, Parameters(a): Parameters<RunFramesArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::run_frames(&mut *l, a.n) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "Freeze at the next supported execution boundary.")]
    async fn pause(&self, Parameters(a): Parameters<CpuArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::pause(&mut *l, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Advance a frozen emulator by count units and freeze again. unit is frames (default) or instructions, with optional cpu selection. Read allowed units from status.contracts.constraints execution.step.units; absence means both units are supported."
    )]
    async fn step(&self, Parameters(a): Parameters<StepArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::step(&mut *l, a.count, a.unit, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "Resume normal execution from a frozen state.")]
    async fn resume(&self, Parameters(a): Parameters<CpuArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::resume(&mut *l, a.cpu.as_deref()) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Reset the game while preserving loaded ROM bytes. Adapters that recreate the control connection during reset return only after the new connection is ready and verified."
    )]
    async fn reset(&self) -> CallToolResult {
        let mut l = self.link();
        match tools::reset(&mut *l) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Set a breakpoint on memory access, execution, or a supported device boundary and optionally freeze on hit. See argument schemas for kind, range, PC/value filters, and atomic snapshots."
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
        description = "Decode count instructions from address using the connected core's ISA. Use it to inspect instructions around a breakpoint hit PC."
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
        description = "Watch a register and freeze on the instruction that moves it outside [min,max], for example a runaway stack pointer. This checks every instruction; use it for bounded hunting and clear it afterward."
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

    #[tool(description = "Remove one breakpoint by ID.")]
    async fn clear_breakpoint(&self, Parameters(a): Parameters<ClearBpArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::clear_breakpoint(&mut *l, a.id) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "List active breakpoints with their IDs, kinds, and ranges.")]
    async fn list_breakpoints(&self) -> CallToolResult {
        let mut l = self.link();
        match tools::list_breakpoints(&mut *l) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "Remove all breakpoints during cleanup.")]
    async fn clear_all_breakpoints(&self) -> CallToolResult {
        let mut l = self.link();
        match tools::clear_all_breakpoints(&mut *l) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(description = "Drain queued events such as breakpoint hits.")]
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
        description = "Enable or disable execution tracing and its call-stack and trace ring buffers. This observes every instruction; use it for bounded crash hunting and disable it afterward."
    )]
    async fn set_trace(&self, Parameters(a): Parameters<SetTraceArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_trace(&mut *l, a.enabled) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Return the N most recent executed instructions in chronological order as [{pc,op,bank?}]. Call set_trace(true) first. bank identifies a paged ROM bank when supported; missing or null means unresolved. Check status.bank_tagging."
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
        description = "Return the current outer-to-inner call-site chain as [{pc,bank?}]. Call set_trace(true) first. Only pc is guaranteed; bank is present when the backend can identify a paged ROM bank. Check status.bank_tagging."
    )]
    async fn call_stack(&self) -> CallToolResult {
        let mut l = self.link();
        match tools::call_stack(&mut *l) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Freeze when the game executes its reset handler, allowing automatic detection of watchdog resets or crash-to-reset paths."
    )]
    async fn break_on_reset(&self, Parameters(a): Parameters<BreakOnResetArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::break_on_reset(&mut *l, a.enabled) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Dump standard memory regions as .bin files plus regions.json and state.json for emucap diff. Read valid regions from status.memory_types."
    )]
    async fn dump_memory(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::dump_memory(&mut *l, &a.path) {
            Ok(o) => output_result(o),
            Err(e) => err_result(e),
        }
    }

    #[tool(
        description = "Replay a regression suite and summarize each case as pass, fail, or invalid. Results are not stored automatically; use Tracking MCP log_gate or log_metric to preserve them."
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
        description = "Replay a case recipe N times and compare observation hashes to measure reproducibility of this execution procedure, not determinism of the game or engine. Results are not stored automatically; preserve them with Tracking MCP log_gate."
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
            return track_err("replays must be between 2 and 5");
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
            "note": "Scope: reproducibility of this harness path, not game or engine determinism. Startup gaps and same-process entropy remain limitations. Record the result with Tracking MCP log_gate(name=determinism_replay, kind=machine, passed).",
        });
        CallToolResult::success(vec![Content::text(body.to_string())])
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Emucap {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("emucap-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(SERVER_INSTRUCTIONS)
    }
}

fn broker_session_accepting(address: &str) -> bool {
    address
        .parse()
        .ok()
        .and_then(|endpoint| {
            std::net::TcpStream::connect_timeout(&endpoint, Duration::from_millis(100)).ok()
        })
        .is_some()
}

fn spawn_reaped(mut command: std::process::Command) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = command.status();
    })
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
        // 직접 모드로 폴백하지 않는다 — broker를 선택한 세션은 그 연결 경로만 사용한다.
        if !broker_session_accepting(&sess_addr) {
            if let Ok(exe) = std::env::current_exe() {
                let broker_bin = exe.with_file_name("emucap-broker");
                let command = std::process::Command::new(broker_bin);
                let _ = spawn_reaped(command);
            }
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
