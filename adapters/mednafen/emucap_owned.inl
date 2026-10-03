// Included on the sole emulation owner after legacy handlers and wire replies.
// Native owned execution remains unadvertised until all service/stop paths and
// public cancellation/controller-loss witnesses are qualified.
using OwnedJson = EmucapControl::Json;
using OwnedResult = EmucapControl::OwnerResult;
std::unique_ptr<EmucapControl::Ownership> g_owned_owner;
std::unique_ptr<EmucapControl::Attachment> g_owned_attachment, g_owned_broker;
uint64_t g_owned_epoch = 0, g_owned_generation = 0, g_owned_stop_deadline = 0;
uint64_t g_owned_service_ms = 0;
bool g_owned_cancelled = false, g_owned_failed_stop = false;
unsigned g_owned_cpu_divider = 0;
struct OwnedReply {
  uint64_t id, epoch;
  EmucapControl::Attachment attachment;
};
struct OwnedChild {
  EmucapControl::Key key;
  OwnedReply reply;
  EmucapControl::NativeAdvance advance;
  uint64_t last_service_ms, max_service_gap_ms = 0, progress_ms;
  bool has_pc = false;
  uint32_t pc = 0;
  OwnedChild(EmucapControl::Key k, OwnedReply r, bool instructions, uint64_t count,
             EmucapControl::AdvanceObservation origin, bool cold)
      : key(std::move(k)), reply(std::move(r)), advance(instructions, count, origin, cold),
        last_service_ms(origin.host_ms), progress_ms(origin.host_ms) {}
};
std::unique_ptr<OwnedChild> g_owned_child;
std::unique_ptr<OwnedReply> g_owned_pause, g_owned_finish;
std::unique_ptr<EmucapControl::CleanupPlan> g_owned_cleanup_plan;
void owned_poll();
void owned_fail(const char* message);
bool owned_busy() {
  return g_owned_child || g_owned_pause || g_owned_cleanup_plan || g_owned_failed_stop;
}
bool owned_needs_cpu() {
  // Frame advances retain the core's ordinary execution path. Debug stepping
  // can change native timing as well as throughput (notably SS and PSX).
  // Pacing/frame service requests a CPU park only when an early stop is needed.
  return g_owned_pause || g_owned_cleanup_plan || g_owned_failed_stop ||
      (g_owned_child && (g_owned_child->advance.Instructions() || g_owned_child->advance.Stopping()));
}
EmucapControl::AdvanceObservation owned_observation_now() {
  return {g_native_control.Generation(), g_native_control.Frames(),
          g_native_control.Callbacks(), monotonic_millis()};
}
OwnedReply owned_reply_for(uint64_t id) { return {id, g_owned_epoch, *g_owned_attachment}; }
bool owned_reply_current(const OwnedReply& reply) {
  return reply.epoch == g_owned_epoch && g_owned_attachment && *g_owned_attachment == reply.attachment;
}
void owned_request_stop() {
  const bool arm = !g_frozen || !g_owned_stop_deadline;
  if (!g_owned_stop_deadline) g_owned_stop_deadline = monotonic_millis() + 5000;
  g_frozen = true;
  g_pacing_released = true;
  if (arm) rearm_breakpoints();
}

EmucapControl::Ownership& native_owner() {
  if (!g_owned_owner) g_owned_owner.reset(new EmucapControl::Ownership(runtime_generation()));
  return *g_owned_owner;
}
OwnedJson owned_key(const EmucapControl::Key& key) {
  return {{"runtime", key.runtime}, {"owner_id", key.owner_id}, {"operation_id", key.operation_id}};
}
void owned_ok(uint64_t id, const OwnedJson& value) {
  send_line(OwnedJson{{"id", id}, {"ok", true}, {"result", value}}.dump());
}
void owned_error(uint64_t id, const char* kind, const char* message) {
  send_line(OwnedJson{{"id", id}, {"ok", false},
      {"error", {{"kind", kind}, {"message", message}}}}.dump());
}
void owned_rejected(uint64_t id, OwnedResult result) {
  const char* kind = result == OwnedResult::Retired ? "temporal_unverified" :
      result == OwnedResult::Busy ? "busy" : "bad_state";
  owned_error(id, kind, "native parent ownership rejected");
}
OwnedJson owned_terminal(const EmucapControl::CleanupPlan& plan) {
  OwnedJson result = {{"status", "completed"}, {"parent", owned_key(plan.key)},
      {"cleanup_verified", true}, {"released_ports", plan.input_ports},
      {"effects_started", plan.effects_started}};
  if (plan.effects_started) result["state"] = "frozen";
  return result;
}
bool owned_stop_observed() {
  return !g_input_control_unverified && !g_pacing_control_unverified &&
      g_native_control.Generation() == g_owned_generation && native_parked() &&
      (!g_owned_stop_deadline || monotonic_millis() < g_owned_stop_deadline);
}
bool owned_cleanup(const EmucapControl::CleanupPlan& plan) {
  if (plan.effects_started && !g_owned_stop_deadline) owned_request_stop();
  EmucapControl::CleanupEvidence evidence;
  if (plan.effects_started) {
    if (!owned_stop_observed()) { native_owner().Retire(); return false; }
    for (const auto port : plan.input_ports) {
      if (port != 0 || monotonic_millis() >= g_owned_stop_deadline || !owned_stop_observed() ||
          !apply_native_input(false, 0)) {
        native_owner().Retire(); return false;
      }
      evidence.released_ports.insert(port);
    }
    evidence.stop_verified = owned_stop_observed() && monotonic_millis() < g_owned_stop_deadline;
  }
  const bool complete = native_owner().FinishCleanup(plan, evidence) == OwnedResult::Ok;
  if (complete) { g_owned_generation = 0; g_owned_stop_deadline = 0; g_owned_cancelled = false; }
  return complete;
}
void owned_disconnected() {
  if (!g_owned_attachment) return;
  try {
    std::unique_ptr<EmucapControl::CleanupPlan> plan;
    const auto result = native_owner().Detach(*g_owned_attachment, plan);
    g_owned_attachment.reset();
    if (result != OwnedResult::Ok) { native_owner().Retire(); return; }
    if (plan) {
      g_owned_cleanup_plan = std::move(plan);
      g_owned_finish.reset();
      if (g_owned_child) g_owned_child->advance.Cancel("connection_closed");
      if (g_owned_cleanup_plan->effects_started) owned_request_stop();
      owned_poll();
    }
  } catch (...) {
    // Socket teardown must finish even if native cleanup or evidence allocation
    // throws. Keep the unverified parent retired across physical reconnects.
    g_owned_attachment.reset();
    owned_fail("native disconnect cleanup failed");
  }
}
bool owned_has_parent() { return g_owned_owner && g_owned_owner->ActiveKey(); }
void owned_connected() {
  g_owned_broker.reset();
  if (g_owned_epoch == UINT64_MAX) native_owner().Retire();
  else ++g_owned_epoch;
}
bool owned_event(const EmucapControl::SessionEvent& event) {
  if (event.runtime != runtime_generation()) return false;
  if (g_owned_broker && (g_owned_broker->broker_instance != event.attachment.broker_instance ||
      g_owned_broker->registration != event.attachment.registration)) return false;
  if (!event.attach) {
    if (g_owned_attachment && *g_owned_attachment == event.attachment) owned_disconnected();
    return !native_owner().Retired();
  }
  if (!g_owned_broker) {
    // A broker bootstrap hello may precede its attach; a direct live parent may
    // never be silently transferred to that broker attachment.
    if (native_owner().ActiveKey() || owned_busy()) return false;
    owned_disconnected();
    g_owned_broker.reset(new EmucapControl::Attachment(event.attachment));
  }
  if (owned_busy() || (g_owned_attachment && !(*g_owned_attachment == event.attachment))) return false;
  if (native_owner().Attach(event.attachment) != OwnedResult::Ok) return false;
  g_owned_attachment.reset(new EmucapControl::Attachment(event.attachment));
  return true;
}
bool owned_observation(const std::string& method) {
  static const char* const names[] = {"hello", "status", "get_rom_info", "poll_events",
      "list_breakpoints", "get_state", "read_memory", "read_memory_batch", "screenshot",
      "disassemble", "call_stack", "get_trace"};
  for (const char* name : names) if (method == name) return true;
  return false;
}
void owned_fail(const char* message) {
  auto child = std::move(g_owned_child);
  auto pause = std::move(g_owned_pause);
  auto finish = std::move(g_owned_finish);
  g_owned_cleanup_plan.reset();
  native_owner().Retire();
  g_owned_failed_stop = true;
  owned_request_stop();
  for (const OwnedReply* reply : {child ? &child->reply : nullptr, pause.get(), finish.get()})
    if (reply && owned_reply_current(*reply)) owned_error(reply->id, "temporal_unverified", message);
}
void owned_poll() {
  if (native_owner().Retired()) {
    if (g_owned_failed_stop && native_parked()) { g_owned_failed_stop = false; rearm_breakpoints(); }
    return;
  }
  if (!owned_busy() && !g_owned_stop_deadline) return;
  try {
    const auto now = owned_observation_now();
    const bool needs_stop = g_owned_child || g_owned_pause ||
        (g_owned_cleanup_plan && g_owned_cleanup_plan->effects_started);
    if ((g_owned_stop_deadline && now.host_ms >= g_owned_stop_deadline) ||
        (needs_stop && (!g_native_control.Valid() || now.generation != g_owned_generation ||
        g_input_control_unverified || g_pacing_control_unverified))) {
      owned_fail("native stop proof changed or expired"); return;
    }
    if (g_owned_child) {
      auto& child = *g_owned_child;
      if (!child.advance.Observe(now)) { owned_fail("native progress observation regressed"); return; }
      if (child.advance.Stopping()) {
        if (child.advance.Reason() != "completed") g_owned_cancelled = true;
        owned_request_stop();
      }
      if (child.advance.Stopping() && native_parked()) {
        auto finished = std::move(g_owned_child);
        const bool complete = finished->advance.Count() == finished->advance.Requested();
        OwnedJson result = {{"status", complete ? "completed" : "interrupted"},
            {"unit", finished->advance.Instructions() ? "instructions" : "frames"},
            {"requested", finished->advance.Requested()}, {"count", finished->advance.Count()},
            {"state", "frozen"}, {"frame", g_frame}, {"generation", g_owned_generation},
            {"stopped_at_ms", now.host_ms}, {"control_service_max_gap_ms", finished->max_service_gap_ms}};
        if (!complete) result["reason"] = finished->advance.Reason();
        if (finished->has_pc) result["pc"] = finished->pc;
        if (g_owned_cancelled) result["stop_deadline_ms"] = g_owned_stop_deadline;
        else if (!g_owned_cleanup_plan && !g_owned_pause) g_owned_stop_deadline = 0;
        rearm_breakpoints();
        if (owned_reply_current(finished->reply)) owned_ok(finished->reply.id, result);
      }
    }
    if (g_owned_pause && native_parked()) {
      auto paused = std::move(g_owned_pause);
      if (!g_owned_cancelled && !g_owned_cleanup_plan) g_owned_stop_deadline = 0;
      rearm_breakpoints();
      if (owned_reply_current(*paused)) owned_ok(paused->id, {{"state", "frozen"}});
    }
    if (g_owned_cleanup_plan && (!g_owned_cleanup_plan->effects_started || native_parked())) {
      auto plan = std::move(g_owned_cleanup_plan);
      auto reply = std::move(g_owned_finish);
      if (!owned_cleanup(*plan)) { owned_fail("native input cleanup was not verified");
        if (reply && owned_reply_current(*reply)) owned_error(reply->id, "temporal_unverified", "native input cleanup was not verified");
        return;
      }
      rearm_breakpoints();
      if (reply && owned_reply_current(*reply)) owned_ok(reply->id, owned_terminal(*plan));
    }
  } catch (...) { owned_fail("native owned execution failed"); }
}
void owned_service() {
  const uint64_t now = monotonic_millis();
  if (g_owned_child) {
    if (now < g_owned_child->last_service_ms) { owned_fail("native service clock regressed"); return; }
    g_owned_child->max_service_gap_ms = std::max(g_owned_child->max_service_gap_ms,
        now - g_owned_child->last_service_ms);
    g_owned_child->last_service_ms = now;
  }
  const bool receive = now < g_owned_service_ms || now - g_owned_service_ms >= 5;
  if (receive) g_owned_service_ms = now;
  owned_poll();
  if (receive) serve_socket_once();
  owned_poll();
  if (g_owned_child && now >= g_owned_child->progress_ms && now - g_owned_child->progress_ms >= 1000) {
    g_owned_child->progress_ms = now;
    if (owned_reply_current(g_owned_child->reply)) owned_ok(g_owned_child->reply.id, {{"status", "working"}});
  }
}
bool owned_cpu_service() {
  const bool busy = owned_busy();
  if (!busy) return false;
  if (g_owned_pause || g_owned_cleanup_plan || g_owned_failed_stop ||
      (g_owned_child && (g_owned_child->advance.Stopping() ||
       g_owned_child->advance.TargetAt(g_native_control.Callbacks()))) ||
      (++g_owned_cpu_divider % 64 == 0)) owned_service();
  return g_frozen && (busy || owned_busy());
}
void owned_interrupt(uint32_t pc) {
  if (!g_owned_child) return;
  if (g_native_control.CurrentContext() == EmucapControl::NativeControl::Context::Device) {
    owned_fail("owned execution reached an unqualified device stop"); return;
  }
  g_owned_child->has_pc = true; g_owned_child->pc = pc;
  g_owned_child->advance.Cancel("breakpoint");
  g_owned_cancelled = true;
  owned_request_stop();
}
bool owned_advance_arguments(const EmucapControl::Request& request, bool& instructions, uint64_t& count) {
  const auto& p = request.params;
  if (p.contains("unit") && (!p["unit"].is_string() || (p["unit"] != "frames" && p["unit"] != "instructions"))) return false;
  instructions = request.method == "step_instructions" || (p.contains("unit") && p["unit"] == "instructions");
  const char* field = request.method == "step_instructions" ? "count" : "frames";
  count = 1;
  if (p.contains(field)) {
    if (!p[field].is_number()) return false;
    const double value = p[field].get<double>();
    if (!std::isfinite(value) || value < 1 || value > 5000 || std::floor(value) != value) return false;
    count = static_cast<uint64_t>(value);
  }
  return true;
}
// Returns true when this layer consumed/rejected the request; otherwise the
// authenticated, owner-stripped request can enter the existing native handler.
bool owned_dispatch(EmucapControl::Request& request) {
  auto& owner = native_owner();
  const bool control_method = request.method == "begin_temporal_operation" ||
      request.method == "finish_temporal_operation" || request.method == "cancel_operation" ||
      request.method == "step" || request.method == "step_instructions";
  if (request.id > static_cast<uint64_t>(LONG_MAX) && !control_method) {
    owned_error(request.id, "unsupported", "native request ID is not supported"); return true;
  }
  if ((request.method == "hello" || request.method == "status") &&
      !EmucapControl::HasControl(request) && !g_owned_broker &&
      (!g_owned_attachment || owner.Retired())) return false;
  if (owner.Retired()) { owned_rejected(request.id, OwnedResult::Retired); return true; }
  if (!g_owned_attachment && !g_owned_broker) {
    EmucapControl::Attachment direct("direct", 1, g_owned_epoch);
    const auto result = owner.Attach(direct);
    if (result != OwnedResult::Ok) { owned_rejected(request.id, result); return true; }
    g_owned_attachment.reset(new EmucapControl::Attachment(direct));
  }
  if (!g_owned_attachment || !EmucapControl::Authenticate(request, *g_owned_attachment, bool(g_owned_broker))) {
    owned_error(request.id, "bad_state", "wrong control attachment"); return true;
  }
  EmucapControl::Scope scope;
  if (!EmucapControl::ReadScope(request, scope)) {
    owned_error(request.id, "bad_params", "invalid native owner scope"); return true;
  }
  if (request.method == "begin_temporal_operation") {
    if (owned_busy() || owned_legacy_busy()) { owned_error(request.id, "busy", "native execution is active"); return true; }
    if (!g_native_control.Valid() || g_input_control_unverified || g_pacing_control_unverified ||
        g_native_control.CurrentContext() == EmucapControl::NativeControl::Context::Device) {
      owned_error(request.id, "unsupported", "native parent admission requires a qualified context"); return true;
    }
    const bool fresh = !owner.ActiveKey();
    const auto result = owner.Begin(*g_owned_attachment, *scope.parent);
    if (result != OwnedResult::Ok) { owned_rejected(request.id, result); return true; }
    if (fresh) { g_owned_generation = g_native_control.Generation(); g_owned_stop_deadline = 0; g_owned_cancelled = false; }
    owned_ok(request.id, {{"status", "admitted"}, {"parent", owned_key(*scope.parent)}}); return true;
  }
  if (request.method == "finish_temporal_operation") {
    if (const auto* terminal = owner.Terminal(*g_owned_attachment, *scope.parent)) {
      owned_ok(request.id, owned_terminal(*terminal)); return true;
    }
    if (g_owned_cleanup_plan) { owned_error(request.id, "busy", "native cleanup is active"); return true; }
    EmucapControl::CleanupPlan plan;
    const auto result = owner.StartCleanup(*g_owned_attachment, *scope.parent, plan);
    if (result != OwnedResult::Ok) { owned_rejected(request.id, result); return true; }
    g_owned_cleanup_plan.reset(new EmucapControl::CleanupPlan(plan));
    g_owned_finish.reset(new OwnedReply(owned_reply_for(request.id)));
    if (g_owned_child) g_owned_child->advance.Cancel("cancelled");
    if (plan.effects_started) owned_request_stop();
    owned_poll(); return true;
  }
  if (request.method == "cancel_operation") {
    EmucapControl::Key key;
    if (!EmucapControl::ReadKey(request.params, key)) {
      owned_error(request.id, "bad_params", "invalid cancel identity"); return true;
    }
    bool requested = false;
    if (g_owned_child && key == g_owned_child->key) {
      g_owned_child->advance.Cancel("cancelled"); requested = true;
    } else if (owner.ActiveKey() && key == *owner.ActiveKey() && (g_owned_pause || g_owned_cleanup_plan)) requested = true;
    if (requested) { g_owned_cancelled = true; owned_request_stop(); }
    owned_ok(request.id, {{"status", requested ? "requested" : "not_active"}}); return true;
  }
  if (owned_busy() && request.method != "hello" && request.method != "status") {
    owned_error(request.id, "busy", "native owned execution is active"); return true;
  }
  const bool observation = owned_observation(request.method);
  auto result = scope.parent ? owner.Authorize(*g_owned_attachment, *scope.parent)
                             : owner.AuthorizeUnscoped(*g_owned_attachment, observation);
  if (result != OwnedResult::Ok) { owned_rejected(request.id, result); return true; }
  const bool step = request.method == "step" || request.method == "step_instructions";
  if (scope.parent && step) {
    bool instructions = false; uint64_t count = 0;
    if (!scope.child || !owned_advance_arguments(request, instructions, count)) {
      owned_error(request.id, "bad_params", "invalid owned advance"); return true;
    }
    if (g_owned_cancelled) { owned_error(request.id, "cancelled", "parent is stopping"); return true; }
    if (!owned_stop_observed()) { owned_error(request.id, "not_frozen", "owned advance requires a verified park"); return true; }
    if (!owned_native_callbacks_available()) { owned_error(request.id, "unsupported", "native CPU control callbacks are unavailable"); return true; }
    auto child = std::unique_ptr<OwnedChild>(new OwnedChild(*scope.child, owned_reply_for(request.id), instructions, count,
        owned_observation_now(), g_native_control.CurrentContext() != EmucapControl::NativeControl::Context::Cpu));
    if (!child->advance.Valid()) { owned_error(request.id, "bad_state", "invalid native advance origin"); return true; }
    result = owner.Effect(*g_owned_attachment, *scope.parent);
    if (result != OwnedResult::Ok) { owned_rejected(request.id, result); return true; }
    g_owned_child = std::move(child); g_owned_service_ms = monotonic_millis(); g_owned_cpu_divider = 0;
    ++g_boundary_seq; g_frozen = false; rearm_breakpoints(); return true;
  }
  if (scope.parent && !observation && request.method != "pause" && request.method != "set_input") {
    owned_error(request.id, "unsupported", "method is outside native parent execution"); return true;
  }
  if (scope.parent && request.method == "pause") {
    if (!native_parked() && !owned_native_callbacks_available()) {
      owned_error(request.id, "unsupported", "native CPU control callbacks are unavailable"); return true;
    }
    result = owner.Effect(*g_owned_attachment, *scope.parent);
    if (result != OwnedResult::Ok) { owned_rejected(request.id, result); return true; }
    g_owned_pause.reset(new OwnedReply(owned_reply_for(request.id)));
    g_owned_service_ms = monotonic_millis();
    ++g_boundary_seq; owned_request_stop(); owned_poll(); return true;
  }
  if (scope.parent && !observation) {
    if (!owned_stop_observed()) {
      if (g_native_control.Generation() != g_owned_generation || !g_native_control.Valid() ||
          g_input_control_unverified || g_pacing_control_unverified ||
          (g_owned_stop_deadline && monotonic_millis() >= g_owned_stop_deadline)) {
        owner.Retire(); owned_rejected(request.id, OwnedResult::Retired);
      } else owned_error(request.id, "not_frozen", "scoped input requires a verified park");
      return true;
    }
    if (request.method == "set_input") {
      EmucapControl::InputRequest input;
      if (!preflight_input_request(request, input)) return true;
      if (g_owned_cancelled && input.mask != 0) { owned_error(request.id, "cancelled", "parent is stopping"); return true; }
      result = owner.InputAttempt(*g_owned_attachment, *scope.parent, 0);
    } else result = owner.Effect(*g_owned_attachment, *scope.parent);
    if (result != OwnedResult::Ok) { owned_rejected(request.id, result); return true; }
  }
  request.params = EmucapControl::NativeParams(request);
  return false;
}
