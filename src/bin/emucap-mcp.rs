use std::sync::{Arc, Mutex};
use std::time::Duration;

use emucap::live::broker_link;
use emucap::live::continuity;
use emucap::live::link::{EmulatorIdentity, EmulatorLink, LinkError};
use emucap::live::tcp;
use emucap::live::tools::{self, ToolOutput};
use emucap::mcp_result::{
    boolean_outcome_result, error_result, link_error_result, tool_output_result,
};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};

#[path = "emucap-mcp/analysis_surface.rs"]
mod analysis_surface;
#[path = "emucap-mcp/args.rs"]
mod args;
#[path = "emucap-mcp/debug_surface.rs"]
mod debug_surface;
#[path = "emucap-mcp/input_surface.rs"]
mod input_surface;
#[path = "emucap-mcp/instructions.rs"]
mod instructions;
#[path = "emucap-mcp/launch.rs"]
mod launch;
#[path = "emucap-mcp/memory_write.rs"]
mod memory_write;
#[path = "emucap-mcp/reattach.rs"]
mod reattach;
#[path = "emucap-mcp/recording.rs"]
mod recording;
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
use crate::reattach::make_reattach;
use crate::regression::{
    default_session_port, ensure_capabilities_loaded, parse_observe_spec, run_one_case,
    verify_determinism_core, DetOutcome,
};
use crate::status::{
    apply_capability_revision, content_identity_for_rom_info, make_bootstrap_value,
    normalize_rom_sha1, observe_control_state,
};
use crate::stop::make_stop;

const STATIC_MCP_METADATA_TTL_MS: u64 = 3_600_000;

type SharedLink = Arc<Mutex<dyn EmulatorLink + Send>>;

fn invalid_request_result(message: impl std::fmt::Display) -> CallToolResult {
    error_result("invalid_request", message)
}

#[derive(Clone)]
struct SurfaceStatusCache {
    identity: EmulatorIdentity,
    status: serde_json::Value,
}

#[derive(Clone)]
struct Emucap {
    link: SharedLink,
    last_full_surface_status: Arc<Mutex<Option<SurfaceStatusCache>>>,
    tool_router: ToolRouter<Emucap>,
}

#[tool_router(router = tool_router)]
impl Emucap {
    fn new(link: SharedLink) -> Self {
        Self {
            link,
            last_full_surface_status: Arc::new(Mutex::new(None)),
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

    fn current_surface_status(&self) -> Result<serde_json::Value, CallToolResult> {
        let mut link = self.link();
        match observe_control_state(&mut *link) {
            Ok(mut observation) => {
                apply_capability_revision(&mut observation.status, None);
                if observation
                    .status
                    .get("connected")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    self.remember_surface_status(
                        &link.capabilities().identity,
                        &observation.status,
                    );
                }
                Ok(observation.status)
            }
            Err(error) => Err(link_error_result(error)),
        }
    }

    fn remember_surface_status(&self, identity: &EmulatorIdentity, status: &serde_json::Value) {
        *self
            .last_full_surface_status
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(SurfaceStatusCache {
            identity: identity.clone(),
            status: status.clone(),
        });
    }

    fn surface_description_status(&self) -> serde_json::Value {
        let link = self.link();
        let connected = link.continuity().transport.state
            == emucap::live::continuity::TransportState::Connected;
        if connected {
            let current_identity = &link.capabilities().identity;
            if let Some(cached) = self
                .last_full_surface_status
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .filter(|cached| &cached.identity == current_identity)
            {
                return cached.status.clone();
            }
        }
        let mut status = serde_json::json!({
            "connected": false,
            "methods": [],
            "contracts": {
                "catalog": emucap::contracts::CATALOG_ID,
                "state": "unreported"
            }
        });
        apply_capability_revision(&mut status, None);
        status
    }

    fn validate_routed_operation(
        &self,
        status: &serde_json::Value,
        surface: &str,
        operation: &str,
        known_revision: Option<&str>,
        supported: bool,
        advertised: bool,
    ) -> Result<(), CallToolResult> {
        if !supported {
            return Err(invalid_request_result(format!(
                "unknown {surface} operation: {operation}; call operation=describe"
            )));
        }
        let Some(current_revision) = status
            .get("capability_revision")
            .and_then(serde_json::Value::as_str)
        else {
            return Err(invalid_request_result(
                "current capability revision is unavailable; call status and describe again",
            ));
        };
        if known_revision != Some(current_revision) {
            return Err(link_error_result(LinkError::Emulator {
                kind: "bad_state".into(),
                message: format!(
                    "{surface} capability revision changed or was omitted; call operation=describe and retry with known_capability_revision={current_revision}"
                ),
            }));
        }
        if !advertised {
            return Err(link_error_result(LinkError::Emulator {
                kind: "unsupported".into(),
                message: format!(
                    "{operation} is not available in capability revision {current_revision}; call {surface}(operation=describe)"
                ),
            }));
        }
        Ok(())
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
            a.includes(BootstrapDetail::Runtimes),
        ) {
            Ok(v) => tool_output_result(ToolOutput::Json(v)),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Plan an adapter launch without starting it; returns validated arguments or required user input."
    )]
    async fn launch_plan(&self, Parameters(a): Parameters<LaunchPlanArgs>) -> CallToolResult {
        let mut link = self.link();
        match make_bootstrap_value(&mut *link, false, false, false) {
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
        description = "Reattach one exact managed generation after its former control lease has been returned."
    )]
    async fn reattach(&self, Parameters(a): Parameters<ReattachArgs>) -> CallToolResult {
        let mut link = self.link();
        boolean_outcome_result(make_reattach(&mut *link, &a), "reattached")
    }

    #[tool(description = "Terminate the exact managed launch generation after ownership checks.")]
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
        description = "Capture the screen with a hash and available frame and freshness provenance."
    )]
    async fn screenshot(&self, Parameters(a): Parameters<ScreenshotArgs>) -> CallToolResult {
        let mut link = self.link();
        let path = a.save_path.as_ref().map(std::path::Path::new);
        match tools::screenshot(&mut *link, path) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Read emulator state registers, optionally filtered by group.")]
    async fn get_state(&self, Parameters(a): Parameters<StateArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::get_state(&mut *link, &a.groups, a.cpu.as_deref()) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    async fn get_video_state(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::get_video_state(&mut *link) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    async fn resolve_tile(&self, Parameters(a): Parameters<ResolveTileArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::resolve_tile(&mut *link, a.nbg, a.x, a.y) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

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
        description = "Read the running content identity and its normalized Tracking identifier."
    )]
    async fn get_rom_info(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let (result, endpoint_port, live_launch_id) = {
            let mut link = self.link();
            let result = tools::get_rom_info(&mut *link);
            let endpoint_port = link.endpoint_port();
            let live_launch_id = link.capabilities().identity.launch_id.clone();
            (result, endpoint_port, live_launch_id)
        };
        match result {
            Ok(ToolOutput::Json(mut v)) => {
                let content_identity = match content_identity_for_rom_info(
                    &v,
                    endpoint_port,
                    live_launch_id.as_deref(),
                ) {
                    Ok(identity) => identity,
                    Err(error) => return error_result("content_identity_error", error),
                };
                normalize_rom_sha1(&mut v, content_identity.as_ref());
                tool_output_result(ToolOutput::Json(v))
            }
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Mount or eject one live media device while frozen. Read device IDs from status.media_devices; provide exactly one of path or eject=true."
    )]
    async fn change_media(&self, Parameters(a): Parameters<ChangeMediaArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::change_media(
            &mut *link,
            &a.device,
            a.path.as_deref(),
            a.eject,
            a.expected_sha1.as_deref(),
        ) {
            Ok(output) => tool_output_result(output),
            Err(error) => link_error_result(error),
        }
    }

    #[tool(
        description = "Read live state and capabilities. Pass the prior capability_revision to omit an unchanged catalog while retaining live state."
    )]
    async fn status(&self, Parameters(a): Parameters<StatusArgs>) -> CallToolResult {
        let mut link = self.link();
        match observe_control_state(&mut *link) {
            Ok(mut observation) => {
                let connected = observation
                    .status
                    .get("connected")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if connected {
                    match status::reconcile_connected_recording(&mut *link) {
                        Ok(true) => match observe_control_state(&mut *link) {
                            Ok(refreshed) => observation = refreshed,
                            Err(error) => {
                                observation.status["recording_recovery"] = serde_json::json!({
                                    "state": "reconciled_but_refresh_failed",
                                    "error": error.to_string(),
                                    "next_action": "Call status again; do not edit capture metadata by hand."
                                });
                            }
                        },
                        Ok(false) => {}
                        Err(error) => {
                            observation.status["recording_recovery"] = serde_json::json!({
                                "state": "blocked",
                                "error": error.to_string(),
                                "next_action": "Keep the exact generation isolated and call status again after its former controller has exited or cleanup becomes verifiable."
                            });
                        }
                    }
                    let mut full_status = observation.status.clone();
                    apply_capability_revision(&mut full_status, None);
                    self.remember_surface_status(&link.capabilities().identity, &full_status);
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
        description = "Write bounded bytes from inline hex or an MCP-host raw-file slice to an advertised memory region."
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

    async fn set_input(&self, Parameters(a): Parameters<InputArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_controller_state(&mut *l, a.port, &a.buttons, &a.axes) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    async fn pulse_while_running(&self, Parameters(a): Parameters<PressArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::press_buttons(&mut *l, a.port, &a.buttons, a.frames) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    async fn move_pointer(&self, Parameters(a): Parameters<MovePointerArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::move_pointer(&mut *link, a.port, a.dx, a.dy, a.frames) {
            Ok(output) => tool_output_result(output),
            Err(error) => link_error_result(error),
        }
    }

    async fn click_pointer(&self, Parameters(a): Parameters<ClickPointerArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::click_pointer(
            &mut *link,
            a.port,
            &a.button,
            a.press_frames,
            a.after_frames,
        ) {
            Ok(output) => tool_output_result(output),
            Err(error) => link_error_result(error),
        }
    }

    async fn drag_pointer(&self, Parameters(a): Parameters<DragPointerArgs>) -> CallToolResult {
        let mut link = self.link();
        match tools::drag_pointer(
            &mut *link,
            a.port,
            &a.button,
            a.dx,
            a.dy,
            a.move_frames,
            a.after_frames,
        ) {
            Ok(output) => tool_output_result(output),
            Err(error) => link_error_result(error),
        }
    }

    #[tool(
        description = "Open persistent, running-time, or device-specific input controls. Call operation=describe first; execution stays in this Control session and requires the returned capability revision."
    )]
    async fn input_control(
        &self,
        Parameters(a): Parameters<RoutedOperationArgs>,
    ) -> CallToolResult {
        input_surface::execute(self, a).await
    }

    async fn touch(&self, Parameters(a): Parameters<TouchArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::touch(&mut *l, a.port, a.x, a.y, a.frames, a.release) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Tap buttons for an exact frame count and return frozen with input released. Read button names from full status."
    )]
    async fn tap(&self, Parameters(a): Parameters<TapArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::tap(&mut *l, a.port, &a.buttons, a.press_frames, a.after_frames) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

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

    #[tool(
        description = "Save emulator state to a file. Set preserve_for_recording=true only when an advertised state-backed recording needs a producer-managed receipt; only a proven frame-boundary receipt can start that origin."
    )]
    async fn save_state(&self, Parameters(a): Parameters<SaveStateArgs>) -> CallToolResult {
        let mut l = self.link();
        let result = if a.preserve_for_recording {
            tools::save_state_for_recording(&mut *l, &a.path)
        } else {
            tools::save_state(&mut *l, &a.path)
        };
        match result {
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

    #[tool(description = "Freeze at the next supported execution boundary.")]
    async fn pause(&self, Parameters(a): Parameters<CpuArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::pause(&mut *l, a.cpu.as_deref()) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Advance by an exact number of advertised frame or instruction boundaries and return frozen. A configured pausing debugger stop preempts the advance and returns status=interrupted with its stop evidence. Read allowed units and bounds from the live contract."
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

    async fn power_cycle(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::power_cycle(&mut *l) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Install one advertised breakpoint kind.")]
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

    #[tool(description = "Decode instructions from an advertised address domain.")]
    async fn disassemble(&self, Parameters(a): Parameters<DisassembleArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::disassemble(
            &mut *l,
            a.address.get(),
            a.count,
            a.cpu.as_deref(),
            a.mode.as_deref(),
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

    #[tool(description = "List active breakpoints.")]
    async fn list_breakpoints(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::list_breakpoints(&mut *l) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Remove every active breakpoint during cleanup.")]
    async fn clear_all_breakpoints(&self, Parameters(_): Parameters<EmptyArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::clear_all_breakpoints(&mut *l) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(description = "Drain queued debugger events once.")]
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

    async fn set_trace(&self, Parameters(a): Parameters<SetTraceArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::set_trace(&mut *l, a.enabled) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

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

    #[tool(description = "Read the current call chain at its advertised authority level.")]
    async fn call_stack(&self, Parameters(a): Parameters<CallStackArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::call_stack(&mut *l, a.cpu.as_deref()) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    async fn break_on_reset(&self, Parameters(a): Parameters<BreakOnResetArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::break_on_reset(&mut *l, a.enabled) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    async fn dump_memory(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let mut l = self.link();
        match tools::dump_memory(&mut *l, &a.path) {
            Ok(o) => tool_output_result(o),
            Err(e) => link_error_result(e),
        }
    }

    #[tool(
        description = "Open optional, data-local, or device-specific debugger operations for the current runtime. Call operation=describe first; execution stays in this Control session and requires the returned capability revision."
    )]
    async fn debug(
        &self,
        Parameters(a): Parameters<RoutedOperationArgs>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        debug_surface::execute(self, a, context).await
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
