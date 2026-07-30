use std::sync::{Arc, Mutex};
use std::time::Duration;

use emucap::live::broker_link;
use emucap::live::continuity;
use emucap::live::link::{EmulatorLink, LinkError};
use emucap::live::tcp;
use emucap::live::tools::{self, ToolOutput};
use emucap::mcp_result::{
    boolean_outcome_result, error_result, link_error_result, tool_output_result,
};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};

#[path = "emucap-mcp/analysis_surface.rs"]
mod analysis_surface;
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
#[path = "emucap-mcp/status.rs"]
mod status;
#[path = "emucap-mcp/stop.rs"]
mod stop;

#[cfg(test)]
#[path = "emucap-mcp/tests.rs"]
mod tests;

use crate::args::*;
use crate::instructions::SERVER_INSTRUCTIONS;
use crate::launch::{apply_task_entry_transition, make_launch, make_launch_plan};
use crate::regression::{
    default_session_port, ensure_capabilities_loaded, parse_observe_spec, run_one_case,
    verify_determinism_core, DetOutcome,
};
use crate::status::{
    apply_capability_revision, make_bootstrap_value, normalize_rom_sha1, observe_control_state,
};
use crate::stop::make_stop;

const STATIC_MCP_METADATA_TTL_MS: u64 = 3_600_000;

type SharedLink = Arc<Mutex<dyn EmulatorLink + Send>>;

fn invalid_request_result(message: impl std::fmt::Display) -> CallToolResult {
    error_result("invalid_request", message)
}

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

    fn visible_tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// 포이즌 내성 lock. 한 도구가 lock을 쥔 채 panic하면 뮤텍스가 poisoned되는데, 이후 모든
    /// `lock().unwrap()`이 panic해 서버 전체가 죽고 세션 재시작을 강요한다. poison을 무시하고
    /// 가드를 회수해 서버를 살린다 — 링크 상태가 어긋났어도 다음 호출의 ensure_connected/raw_call이
    /// 재동기화한다(죽은 conn이면 비우고 재수락).
    fn link(&self) -> std::sync::MutexGuard<'_, dyn EmulatorLink + Send + 'static> {
        self.link.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[tool(
        description = "Start here. Establishes the listener and returns compact task routing. Use include for the full system catalog or installation paths."
    )]
    async fn bootstrap(&self, Parameters(a): Parameters<BootstrapArgs>) -> CallToolResult {
        let mut link = self.link();
        match make_bootstrap_value(
            &mut *link,
            a.includes(BootstrapDetail::Systems),
            a.includes(BootstrapDetail::Installation),
        ) {
            Ok(v) => tool_output_result(ToolOutput::Json(v)),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Plan which adapter should launch a ROM, disc, or disk. Returns a structured next_action that preserves known input, validated launch arguments, preconditions, and listening_port. Ambiguous media is never guessed."
    )]
    async fn launch_plan(&self, Parameters(a): Parameters<LaunchPlanArgs>) -> CallToolResult {
        let mut link = self.link();
        match make_bootstrap_value(&mut *link, false, false) {
            Ok(bootstrap) => {
                let port = bootstrap
                    .pointer("/listener/port")
                    .and_then(|v| v.as_u64())
                    .and_then(|p| u16::try_from(p).ok());
                let mut plan = make_launch_plan(port, &a);
                apply_task_entry_transition(&mut plan, &a, &bootstrap);
                tool_output_result(ToolOutput::Json(plan))
            }
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Launch with the resolved adapter, start its managed bridge if needed, wait for readiness, and return process identity and outcome."
    )]
    async fn launch(&self, Parameters(a): Parameters<LaunchArgs>) -> CallToolResult {
        let mut link = self.link();
        boolean_outcome_result(make_launch(&mut *link, &a), "launched")
    }

    #[tool(
        description = "Terminate one exact managed launch generation and all of its owned helper processes. Requires status.runtime_instance.launch_id, verifies the current generation, control lease, and process-start identities, preserves failure evidence, and never terminates by executable name. This ends the emulator process; use pause to freeze guest execution."
    )]
    async fn stop(&self, Parameters(a): Parameters<StopArgs>) -> CallToolResult {
        let mut link = self.link();
        boolean_outcome_result(make_stop(&mut *link, &a), "stopped")
    }

    #[tool(description = "Read a byte range from the running game's memory.")]
    async fn read_memory(&self, Parameters(a): Parameters<ReadMemoryArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::read_memory(&mut *link, &a.memory_type, a.address.get(), a.length.get()) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Atomically load a state, advance frames, and read memory. A breakpoint hit invalidates the measurement as status:interrupted."
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
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Search an adapter memory region for a hex pattern without transferring the whole region."
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
                    Ok(s) => tool_output_result(ToolOutput::Json(s)),
                    Err(e) => link_error_result(LinkError::Protocol(e)),
                },
                None => tool_output_result(ToolOutput::Json(v)),
            },
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Capture the current screen and return PNG data with SHA-256, byte length, and any frame, state, and freshness provenance supplied by the backend."
    )]
    async fn screenshot(&self, Parameters(a): Parameters<ScreenshotArgs>) -> CallToolResult {
        let mut link = self.link();
        let path = a.save_path.as_ref().map(std::path::Path::new);
        match tools::screenshot(&mut *link, path) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Read emulator state registers. Filter with groups or omit them for all state. Backends without group filtering ignore the filter and return all available state."
    )]
    async fn get_state(&self, Parameters(a): Parameters<StateArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::get_state(&mut *link, &a.groups, a.cpu.as_deref()) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Decode and return Saturn VDP2 video state by layer (NBG0-3, RBG, and common state). See the adapter README for returned fields, formulas, and character-base correction."
    )]
    async fn get_video_state(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::get_video_state(&mut *link) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Resolve a Saturn NBG screen coordinate to the cell's character-data address through scroll, map cell, pattern-name data, and character number. Returns intermediate values; see the adapter documentation for field formulas and character-base correction."
    )]
    async fn resolve_tile(&self, Parameters(a): Parameters<ResolveTileArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::resolve_tile(&mut *link, a.nbg, a.x, a.y) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Set or query the persistent video-layer mask. Omit layers and mask to query; restore the full set after analysis."
    )]
    async fn set_layer_enable(
        &self,
        Parameters(a): Parameters<SetLayerEnableArgs>,
    ) -> CallToolResult {
        let mut link = self.link();
        let layers = a.layers.unwrap_or_default();
        match tools::set_layer_enable(&mut *link, &layers, a.mask) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Read the running content identity: name, path, size, media_type, and normalized rom_sha1. Pass rom_sha1 unchanged to Tracking MCP run_start. Use a local SHA-1 fallback only when the backend does not provide it."
    )]
    async fn get_rom_info(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::get_rom_info(&mut *link) {
            Ok(ToolOutput::Json(mut v)) => {
                normalize_rom_sha1(&mut v);
                tool_output_result(ToolOutput::Json(v))
            }
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Read live state and capabilities. Pass the prior capability_revision to omit an unchanged catalog while retaining live state."
    )]
    async fn status(&self, Parameters(a): Parameters<StatusArgs>) -> CallToolResult {
        let mut link = self.link();
        match observe_control_state(&mut *link) {
            Ok(mut observation) => {
                if observation
                    .status
                    .get("connected")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    apply_capability_revision(
                        &mut observation.status,
                        a.known_capability_revision.as_deref(),
                    );
                }
                tool_output_result(ToolOutput::Json(observation.status))
            }
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Read the last known-good status and preserved transport or adapter failure capsule. This does not contact the emulator and remains available after disconnection."
    )]
    async fn get_failure_context(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut link = self.link();
        tool_output_result(ToolOutput::Json(link.failure_context()))
    }

    #[tool(
        description = "Dismiss a preserved fatal quarantine and continue the emulator's original termination path. Use only when status.methods includes dismiss_failure."
    )]
    async fn dismiss_failure(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut link = self.link();
        if !link
            .capabilities()
            .methods
            .iter()
            .any(|method| method == "dismiss_failure")
        {
            return link_error_result(LinkError::Emulator {
                kind: "unsupported".into(),
                message: "connected adapter does not advertise dismiss_failure".into(),
            });
        }
        match tools::dismiss_failure(&mut *link) {
            Ok(output) => tool_output_result(output),
            Err(error) => link_error_result(error),
        }
    }

    #[tool(
        description = "Write memory from exactly one source: inline hex or a bounded input_file slice. File input is snapshotted and hashed first."
    )]
    async fn write_memory(&self, Parameters(a): Parameters<WriteMemoryArgs>) -> CallToolResult {
        let generation = if a.input_file.is_some() {
            memory_write::generation_marker(self.link().capabilities())
        } else {
            None
        };
        let prepared = match memory_write::prepare_write(&a).await {
            Ok(prepared) => prepared,
            Err(error) => return link_error_result(error),
        };
        let mut l = self.link();
        if generation.is_some() && generation != memory_write::generation_marker(l.capabilities()) {
            return link_error_result(LinkError::Emulator {
                kind: "bad_state".into(),
                message: "runtime generation changed while staging file input; retry against the current status".into(),
            });
        }
        match tools::write_memory_bytes(&mut *l, &a.memory_type, a.address.get(), &prepared.bytes) {
            Ok(o) => tool_output_result(memory_write::with_provenance(o, &prepared)),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Hold controller or key input until released by set_input with an empty button array. Ownership persists in running and frozen states. Read valid button names from status.input_buttons."
    )]
    async fn set_input(&self, Parameters(a): Parameters<InputArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_input(&mut *l, a.port, &a.buttons) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Press buttons or keys for a real-time frame duration and then release them. Automatically resumes when frozen. Read names from status.input_buttons; use tap for deterministic frozen-state single actions."
    )]
    async fn press_buttons(&self, Parameters(a): Parameters<PressArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::press_buttons(&mut *l, a.port, &a.buttons, a.frames) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Control a lower touchscreen at x,y. release:true releases touch; frames performs a timed tap and releases; omitting both holds touch. Use only when this method appears in status.methods."
    )]
    async fn touch(&self, Parameters(a): Parameters<TouchArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::touch(&mut *l, a.port, a.x, a.y, a.frames, a.release) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Perform a frame-precise tap while frozen, then release input and remain frozen. Intended for one action below auto-repeat timing. Read button names from status.input_buttons."
    )]
    async fn tap(&self, Parameters(a): Parameters<TapArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::tap(&mut *l, a.port, &a.buttons, a.press_frames, a.after_frames) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
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
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Save emulator state to a file.")]
    async fn save_state(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::save_state(&mut *l, &a.path) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Load emulator state from a file.")]
    async fn load_state(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::load_state(&mut *l, &a.path) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Advance for N frames in running free-run mode. This is not frame-exact capture; use pause then step for exact advancement. A breakpoint hit returns status:interrupted; drain its event with poll_events."
    )]
    async fn run_frames(&self, Parameters(a): Parameters<RunFramesArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::run_frames(&mut *l, a.n) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Freeze at the next supported execution boundary.")]
    async fn pause(&self, Parameters(a): Parameters<CpuArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::pause(&mut *l, a.cpu.as_deref()) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Advance from frozen by frames or instructions and freeze again. Read allowed units and bounds from the live contract."
    )]
    async fn step(&self, Parameters(a): Parameters<StepArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::step(&mut *l, a.count, a.unit, a.cpu.as_deref()) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Resume normal execution from a frozen state.")]
    async fn resume(&self, Parameters(a): Parameters<CpuArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::resume(&mut *l, a.cpu.as_deref()) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Reset the game while preserving loaded ROM bytes. Adapters that recreate the control connection during reset return only after the new connection is ready and verified."
    )]
    async fn reset(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::reset(&mut *l) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
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
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
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
                    Ok(s) => tool_output_result(ToolOutput::Json(s)),
                    Err(e) => link_error_result(LinkError::Protocol(e)),
                },
                None => tool_output_result(ToolOutput::Json(v)),
            },
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Watch a register and optionally freeze on the instruction that moves it outside an inclusive range."
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
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Remove one breakpoint by ID.")]
    async fn clear_breakpoint(&self, Parameters(a): Parameters<ClearBpArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::clear_breakpoint(&mut *l, a.id) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "List active breakpoints with their IDs, kinds, and ranges.")]
    async fn list_breakpoints(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::list_breakpoints(&mut *l) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Remove all breakpoints during cleanup.")]
    async fn clear_all_breakpoints(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::clear_all_breakpoints(&mut *l) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Drain queued events such as breakpoint hits.")]
    async fn poll_events(&self, Parameters(a): Parameters<PollEventsArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::poll_events(&mut *l) {
            Ok(ToolOutput::Json(v)) => match a.output_path.as_deref() {
                Some(p) => match emucap::offload::offload_result(&v, std::path::Path::new(p)) {
                    Ok(s) => tool_output_result(ToolOutput::Json(s)),
                    Err(e) => link_error_result(LinkError::Protocol(e)),
                },
                None => tool_output_result(ToolOutput::Json(v)),
            },
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Enable or disable execution tracing and its call-stack and trace ring buffers. This observes every instruction; use it for bounded crash hunting and disable it afterward."
    )]
    async fn set_trace(&self, Parameters(a): Parameters<SetTraceArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_trace(&mut *l, a.enabled) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
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
                    Ok(s) => tool_output_result(ToolOutput::Json(s)),
                    Err(e) => link_error_result(LinkError::Protocol(e)),
                },
                None => tool_output_result(ToolOutput::Json(v)),
            },
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Return the current outer-to-inner call-site chain as [{pc,bank?}]. Call set_trace(true) first. Only pc is guaranteed; bank is present when the backend can identify a paged ROM bank. Check status.bank_tagging."
    )]
    async fn call_stack(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::call_stack(&mut *l) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Freeze when the game executes its reset handler, allowing automatic detection of watchdog resets or crash-to-reset paths."
    )]
    async fn break_on_reset(&self, Parameters(a): Parameters<BreakOnResetArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::break_on_reset(&mut *l, a.enabled) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Dump standard memory regions as .bin files plus regions.json and state.json for emucap diff. Read valid regions from status.memory_types."
    )]
    async fn dump_memory(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::dump_memory(&mut *l, &a.path) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Lazy entrypoint for optional live regression and reproducibility analysis. Call operation=describe first, then call this same tool with the selected operation and its returned argument schema. Execution stays in the current Control session."
    )]
    async fn analysis(&self, Parameters(a): Parameters<AnalysisArgs>) -> CallToolResult {
        match a.operation {
            AnalysisOperation::Describe => {
                if a.arguments
                    .as_ref()
                    .is_some_and(|arguments| !arguments.is_empty())
                {
                    return invalid_request_result("operation=describe does not accept arguments");
                }
                tool_output_result(ToolOutput::Json(analysis_surface::describe()))
            }
            AnalysisOperation::RegressionRun => {
                match analysis_surface::parse_arguments("regression_run", a.arguments) {
                    Ok(arguments) => self.regression_run_impl(arguments),
                    Err(error) => invalid_request_result(error),
                }
            }
            AnalysisOperation::VerifyDeterminism => {
                match analysis_surface::parse_arguments("verify_determinism", a.arguments) {
                    Ok(arguments) => self.verify_determinism_impl(arguments),
                    Err(error) => invalid_request_result(error),
                }
            }
        }
    }

    fn regression_run_impl(&self, a: RegressionRunArgs) -> CallToolResult {
        let suite = std::path::PathBuf::from(&a.suite_dir);
        let cases = match regression::load_suite(&suite) {
            Ok(c) => c,
            Err(e) => return link_error_result(LinkError::Protocol(e)),
        };
        let mut l = self.link();
        if let Err(e) = ensure_capabilities_loaded(&mut *l) {
            return link_error_result(e);
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
        tool_output_result(ToolOutput::Json(body))
    }

    fn verify_determinism_impl(&self, a: VerifyDeterminismArgs) -> CallToolResult {
        let replays = a.replays.unwrap_or(2);
        if !(2..=5).contains(&replays) {
            return invalid_request_result("replays must be between 2 and 5");
        }
        let observe = match parse_observe_spec(
            a.observe.as_deref(),
            a.memory_type.clone(),
            a.address.map(|n| n.get()),
            a.length.map(|n| n.get()),
        ) {
            Ok(o) => o,
            Err(e) => return invalid_request_result(e),
        };
        let dir = std::path::PathBuf::from(&a.case_dir);
        let case = match regression::load_case(&dir) {
            Ok(c) => c,
            Err(e) => return link_error_result(LinkError::Protocol(e)),
        };

        let result = {
            let mut l = self.link();
            if let Err(e) = ensure_capabilities_loaded(&mut *l) {
                return link_error_result(e);
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
        tool_output_result(ToolOutput::Json(body))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Emucap {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("emucap-mcp", env!("CARGO_PKG_VERSION")))
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
            rmcp::model::ListToolsResult::with_all_items(self.visible_tools())
                .with_ttl_ms(STATIC_MCP_METADATA_TTL_MS)
                .with_cache_scope(rmcp::model::CacheScope::Public),
        )
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        self.tool_router.get(name).cloned()
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
    let service = server.serve(emucap::mcp_stdio::bounded_stdio()).await?;
    service.waiting().await?;
    Ok(())
}
