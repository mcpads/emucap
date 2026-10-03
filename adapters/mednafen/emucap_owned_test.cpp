#include "emucap_control_wire.h"
#include "emucap_control_owner.h"
#include "emucap_native_control.h"
#include "emucap_owned_advance.h"
#include "emucap_input_request.h"
#include <cassert>
#include <climits>
#include <stdexcept>
using Json = EmucapControl::Json;
EmucapControl::NativeControl g_native_control;
bool g_input_control_unverified = false, g_pacing_control_unverified = false;
bool quiescent = true, fail_input = false;
bool g_frozen = true, g_pacing_released = false;
uint64_t g_frame = 0, g_boundary_seq = 0;
void rearm_breakpoints() {}
void serve_socket_once() {}
bool owned_legacy_busy() { return false; }
bool owned_native_callbacks_available() { return true; }
unsigned native_mask = 0;
uint64_t host_ms = 0, write_delay_ms = 0;
bool replace_during_write = false, throw_write = false;
std::vector<Json> replies;
std::string runtime_generation() { return "runtime"; }
uint64_t monotonic_millis() { return host_ms; }
bool native_parked() { return g_native_control.Parked(quiescent && g_frozen); }
void send_line(const std::string& text) { replies.push_back(Json::parse(text)); }
bool apply_native_input(bool engaged, uint16_t mask) {
  if (throw_write) throw std::runtime_error("injected native cleanup failure");
  host_ms += write_delay_ms;
  if (replace_during_write) g_native_control.GenerationChanged();
  if (fail_input) { g_input_control_unverified = true; return false; }
  native_mask = engaged ? mask : 0; return true;
}
bool preflight_input_request(const EmucapControl::Request& request, EmucapControl::InputRequest&) {
  return !request.params.contains("port") || request.params["port"] == 0;
}
#include "emucap_owned.inl"
EmucapControl::Key parent{"runtime", "owner", "parent"};
EmucapControl::Request request(const char* method, Json params = Json::object()) {
  EmucapControl::Request r; r.id = 9007199254740993ULL; r.method = method; r.params = params; return r;
}
void reset() {
  g_owned_owner.reset(); g_owned_attachment.reset(); g_owned_broker.reset();
  g_owned_child.reset(); g_owned_pause.reset(); g_owned_finish.reset(); g_owned_cleanup_plan.reset();
  g_owned_generation = g_owned_epoch = g_owned_stop_deadline = g_owned_service_ms = 0;
  g_owned_cancelled = g_owned_failed_stop = false;
  g_frozen = true; g_pacing_released = false;
  g_input_control_unverified = g_pacing_control_unverified = fail_input = false;
  host_ms = write_delay_ms = 0; replace_during_write = throw_write = false;
  quiescent = true; native_mask = 4; replies.clear(); owned_connected();
}
void begin() {
  auto r = request("begin_temporal_operation", {{"parent", owned_key(parent)}});
  assert(owned_dispatch(r)); assert(replies.back()["result"]["status"] == "admitted");
}
void input() {
  auto r = request("set_input", {{"buttons", {"a"}}, {"_temporal_owner", owned_key(parent)}});
  r.id = 1;
  assert(!owned_dispatch(r)); assert(!r.params.contains("_temporal_owner"));
  apply_native_input(true, 1);
}
int main() {
  // Partial intervals, replacement and clock regression cannot produce success.
  using EmucapControl::NativeAdvance;
  NativeAdvance partial(false, 5, {1, 10, 20, 100}, false);
  assert(partial.Observe({1, 12, 500, 120}) && partial.Count() == 2 && !partial.Stopping());
  partial.Cancel("cancelled");
  assert(partial.Observe({1, 12, 600, 130}) && partial.Count() == 2 && partial.Reason() == "cancelled");
  assert(!partial.Observe({2, 12, 600, 131}) && !partial.Valid());
  NativeAdvance regressed(true, 5, {1, 10, 20, 100}, false);
  assert(!regressed.Observe({1, 10, 21, 99}) && !regressed.TargetAt(100));
  NativeAdvance expired(false, 5, {1, 10, 20, 100}, false);
  assert(expired.Observe({1, 10, 20, 245100}) && expired.Reason() == "host_deadline" && expired.Count() == 0);
  EmucapControl::NativeControl::Scope frame(g_native_control, EmucapControl::NativeControl::Context::Frame);
  reset(); begin(); owned_disconnected(); assert(native_mask == 4 && !native_owner().ActiveKey());
  reset(); begin(); input(); owned_disconnected(); assert(native_mask == 0 && !native_owner().ActiveKey());
  reset(); begin(); input();
  auto finish = request("finish_temporal_operation", {{"parent", owned_key(parent)}});
  assert(owned_dispatch(finish)); const auto terminal = replies.back();
  assert(terminal["id"] == 9007199254740993ULL && terminal["result"]["released_ports"] == Json::array({0}));
  assert(native_mask == 0 && !native_owner().ActiveKey());
  assert(owned_dispatch(finish) && replies.back() == terminal);
  auto other = parent; other.operation_id = "next";
  auto replacement = request("begin_temporal_operation", {{"parent", owned_key(other)}});
  assert(owned_dispatch(replacement)); native_mask = 8;
  assert(owned_dispatch(finish) && !replies.back()["ok"].get<bool>() && native_mask == 8);
  auto cancel = request("cancel_operation", owned_key(other));
  assert(owned_dispatch(cancel) && replies.back()["result"]["status"] == "not_active");
  assert(native_owner().ActiveKey() && native_mask == 8);
  reset(); begin();
  auto wide = request("set_input", {{"_temporal_owner", owned_key(parent)}}); wide.id = UINT64_MAX;
  assert(owned_dispatch(wide) && replies.back()["id"] == UINT64_MAX);
  owned_disconnected(); assert(native_mask == 4);
  reset(); begin(); input(); g_native_control.GenerationChanged(); owned_disconnected();
  assert(native_owner().Retired() && native_mask == 1); // No release under changed authority.
  owned_connected(); assert(native_owner().Retired());
  auto retired_request = request("set_input", {{"buttons", {"a"}}}); retired_request.id = 1;
  assert(owned_dispatch(retired_request) && replies.back()["error"]["kind"] == "temporal_unverified");
  reset(); begin(); input(); throw_write = true; owned_disconnected();
  assert(native_owner().Retired() && !g_owned_attachment);
  owned_connected();
  assert(owned_dispatch(retired_request) && replies.back()["error"]["kind"] == "temporal_unverified");
  reset(); begin(); input(); fail_input = true; owned_disconnected(); assert(native_owner().Retired());
  reset(); begin(); fail_input = true; input();
  EmucapControl::CleanupPlan attempted;
  assert(native_owner().StartCleanup(*g_owned_attachment, parent, attempted) == OwnedResult::Ok);
  assert(attempted.effects_started && attempted.input_ports == std::set<uint64_t>{0});
  owned_disconnected(); assert(native_owner().Retired() && native_mask == 4);
  reset(); begin(); input(); write_delay_ms = 5000; owned_disconnected();
  assert(native_owner().Retired() && native_mask == 0);
  reset(); begin(); input(); replace_during_write = true; owned_disconnected();
  assert(native_owner().Retired() && native_mask == 0);
  reset(); quiescent = false;
  auto running = request("begin_temporal_operation", {{"parent", owned_key(parent)}});
  assert(owned_dispatch(running) && native_owner().ActiveKey());
  auto no_effect_finish = request("finish_temporal_operation", {{"parent", owned_key(parent)}});
  assert(owned_dispatch(no_effect_finish) && !native_owner().ActiveKey());
  reset();
  EmucapControl::SessionEvent attach;
  attach.attach = true; attach.runtime = "runtime"; attach.attachment = {"broker", UINT64_MAX, 9007199254740993ULL};
  assert(owned_event(attach));
  auto admitted = request("begin_temporal_operation", {{"parent", owned_key(parent)},
      {"_control_attachment", {{"broker_instance", "broker"}, {"registration", UINT64_MAX}, {"session", 9007199254740993ULL}}}});
  assert(owned_dispatch(admitted) && native_owner().ActiveKey());
  auto stale = attach; stale.attach = false; ++stale.attachment.session;
  assert(owned_event(stale) && native_owner().ActiveKey());
  auto spoof = request("set_input", {{"_temporal_owner", owned_key(parent)}}); spoof.id = 1;
  assert(owned_dispatch(spoof) && native_mask == 4 && native_owner().ActiveKey());
  attach.attach = false; assert(owned_event(attach) && !native_owner().ActiveKey());
  reset(); begin();
  const auto child_key = EmucapControl::Key{"runtime", "owner", "child"};
  auto step = request("step_instructions", {{"count", 3}, {"_temporal_owner", owned_key(parent)}, {"_control", owned_key(child_key)}});
  assert(owned_dispatch(step) && g_owned_child && !g_frozen && owned_needs_cpu());
  for (unsigned index = 0; index < 4; ++index) {
    EmucapControl::NativeControl::Scope cpu(g_native_control, EmucapControl::NativeControl::Context::Cpu);
    g_native_control.CpuCallback(); owned_poll();
    if (index < 3) assert(g_owned_child && !g_frozen);
  }
  assert(!g_owned_child && g_frozen && replies.back()["result"]["count"] == 3);
  assert(g_owned_stop_deadline == 0);
  reset(); begin(); input();
  auto frames = request("step", {{"frames", 100}, {"_temporal_owner", owned_key(parent)}, {"_control", owned_key(child_key)}});
  assert(owned_dispatch(frames) && g_owned_child && !owned_needs_cpu());
  {
    EmucapControl::NativeControl::Scope pacing(g_native_control, EmucapControl::NativeControl::Context::Pacing);
    host_ms = 100; auto abort = request("cancel_operation", owned_key(child_key));
    assert(owned_dispatch(abort) && replies.back()["result"]["status"] == "requested");
    owned_poll(); assert(g_owned_child && g_owned_stop_deadline == 5100 && owned_needs_cpu());
  }
  owned_poll(); assert(!g_owned_child && replies.back()["result"]["count"] == 0);
  assert(replies.back()["result"]["reason"] == "cancelled" && g_owned_stop_deadline == 5100);
  assert(owned_dispatch(frames) && replies.back()["error"]["kind"] == "cancelled");
  host_ms = 5099; write_delay_ms = 1;
  assert(owned_dispatch(finish) && native_owner().Retired());
  reset(); quiescent = false; g_frozen = false; begin();
  auto pause = request("pause", {{"_temporal_owner", owned_key(parent)}}); pause.id = 1;
  assert(owned_dispatch(pause) && g_owned_pause && g_owned_stop_deadline == 5000);
  host_ms = 4900; quiescent = true; owned_poll();
  assert(!g_owned_pause && g_owned_stop_deadline == 0 && g_frozen);
  host_ms = 100000;
  assert(owned_dispatch(frames) && g_owned_child && !owned_needs_cpu()); // Initial pause did not consume the later stop budget.
  {
    EmucapControl::NativeControl::Scope pacing(g_native_control, EmucapControl::NativeControl::Context::Pacing);
    owned_disconnected(); assert(g_owned_child && g_owned_cleanup_plan && native_mask == 4);
  }
  owned_poll(); assert(!g_owned_child && !native_owner().ActiveKey() && native_mask == 4);

}
