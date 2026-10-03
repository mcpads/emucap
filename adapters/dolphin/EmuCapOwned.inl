// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
// Included inside the native adapter namespace after its handlers and socket writer.
using OwnedJson = Temporal::Json;
using OwnedClock = std::chrono::steady_clock;
constexpr auto OWNED_STOP_BUDGET = std::chrono::seconds(5);
constexpr size_t OWNED_MAX_FRAME = EmuCap::MAX_NDJSON_FRAME_BYTES;

bool OwnedSocketRetry()
{
#ifdef _WIN32
  const int error = WSAGetLastError();
  return error == WSAEWOULDBLOCK || error == WSAEINTR;
#else
  return errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR;
#endif
}

// The session reader is the only socket owner; the native worker never writes responses.
// Nonblocking I/O preserves one deadline across partial writes.
bool OwnedWrite(SOCKET sock, const OwnedJson& value)
{
#ifndef _WIN32
  if (sock < 0 || sock >= FD_SETSIZE) return false;
#endif
  const std::string bytes = value.dump() + "\n";
  if (bytes.size() > OWNED_MAX_FRAME + 1) return false;
  const auto deadline = OwnedClock::now() + std::chrono::milliseconds(25);
  size_t offset = 0;
  while (offset < bytes.size() && OwnedClock::now() < deadline)
  {
    fd_set writable;
    FD_ZERO(&writable); FD_SET(sock, &writable);
    timeval wait{0, 1000};
    const int ready = select(static_cast<int>(sock + 1), nullptr, &writable, nullptr, &wait);
    if (ready < 0) { if (OwnedSocketRetry()) continue; return false; }
    if (!ready) continue;
#ifdef MSG_NOSIGNAL
    constexpr int flags = MSG_NOSIGNAL;
#else
    constexpr int flags = 0;
#endif
    const int sent = send(sock, bytes.data() + offset,
                          static_cast<int>(bytes.size() - offset), flags);
    if (sent < 0 && OwnedSocketRetry()) continue;
    if (sent <= 0) return false;
    offset += static_cast<size_t>(sent);
  }
  return offset == bytes.size();
}
enum class OwnedRead { Ready, Idle, Closed };
OwnedRead ReadOwned(SOCKET sock, std::string& pending, OwnedJson& value)
{
#ifndef _WIN32
  if (sock < 0 || sock >= FD_SETSIZE) return OwnedRead::Closed;
#endif
  auto newline = pending.find('\n');
  if (newline == std::string::npos)
  {
    if (pending.size() > OWNED_MAX_FRAME) return OwnedRead::Closed;
    fd_set readable;
    FD_ZERO(&readable); FD_SET(sock, &readable);
    timeval wait{0, 10000};
    const int ready = select(static_cast<int>(sock + 1), &readable, nullptr, nullptr, &wait);
    if (ready < 0) return OwnedSocketRetry() ? OwnedRead::Idle : OwnedRead::Closed;
    if (!ready) return OwnedRead::Idle;
    char bytes[4096];
    const int size = recv(sock, bytes, sizeof(bytes), 0);
    if (size < 0 && OwnedSocketRetry()) return OwnedRead::Idle;
    if (size <= 0) return OwnedRead::Closed;
    pending.append(bytes, static_cast<size_t>(size));
    newline = pending.find('\n');
    if (newline == std::string::npos)
      return pending.size() > OWNED_MAX_FRAME ? OwnedRead::Closed : OwnedRead::Idle;
  }
  if (newline > OWNED_MAX_FRAME) return OwnedRead::Closed;
  const std::string line = pending.substr(0, newline);
  pending.erase(0, newline + 1);
  if (line.find_first_not_of(" \r\t") == std::string::npos) return OwnedRead::Idle;
  value = OwnedJson::parse(line, nullptr, false);
  return value.is_discarded() ? OwnedRead::Closed : OwnedRead::Ready;
}

struct OwnedSession
{
  std::string runtime;
  Temporal::ProducerOwnership owner;
  std::optional<Temporal::Attachment> current;
  std::optional<std::pair<std::string, uint64_t>> broker;
  std::optional<OwnedClock::time_point> stop_deadline;
  explicit OwnedSession(std::string id) : runtime(std::move(id)), owner(runtime) {}

  void RememberCancellation()
  {
    std::lock_guard lock(s_cancel_mutex);
    if (s_cancel_origin)
    {
      const auto end = *s_cancel_origin + OWNED_STOP_BUDGET;
      stop_deadline = stop_deadline ? std::min(*stop_deadline, end) : end;
    }
  }
  bool Cleanup(const Temporal::CleanupPlan& plan)
  {
    RememberCancellation();
    if (!stop_deadline) stop_deadline = OwnedClock::now() + OWNED_STOP_BUDGET;
    const auto deadline = *stop_deadline;
    auto job = std::make_shared<Temporal::HostJob>(deadline);
    if (plan.effects_started)
    {
      Core::QueueHostJob([job, plan](Core::System& system) {
        job->Execute([] { return !s_control_retired.load(); }, [&] {
          Core::CancelFrameStep(system);
          SafeAccess access(system);
          if (Core::GetState(system) != Core::State::Paused) return false;
          // These overrides are the authoritative source consumed by the native pad hooks.
          // Release exactly the attempted ports, preserving unscoped/other-port input.
          std::lock_guard lock(s_input_mutex);
          const bool wii = IsWiiSystem(EnvOr("EMUCAP_SYSTEM", "gamecube"));
          for (uint64_t port : plan.input_ports)
          {
            if (port != 0) return false;
            if (wii)
            {
              s_wii_input = {};
              if (s_wii_input.engaged || s_wii_input.buttons != 0) return false;
            }
            else
            {
              s_gamecube_input[port] = {};
              if (s_gamecube_input[port].engaged) return false;
            }
          }
          return true;
        });
      });
    }
    const bool verified = !s_control_retired.load() &&
        (!plan.effects_started || (job->Wait() == Temporal::HostJobOutcome::Completed &&
          AwaitMemoryPark(Core::System::GetInstance(), deadline))) &&
        OwnedClock::now() < deadline;
    Temporal::CleanupEvidence proof;
    if (verified) { proof.stop_verified = true; proof.released_ports = plan.input_ports; }
    const auto result = owner.FinishCleanup(plan, proof);
    if (!verified || result != Temporal::OwnerResult::Ok)
    {
      s_control_retired.store(true);
      return false;
    }
    stop_deadline.reset();
    return true;
  }
  bool Close()
  {
    if (!current) return !s_control_retired.load();
    std::optional<Temporal::CleanupPlan> plan;
    const auto result = owner.Detach(*current, plan);
    current.reset();
    return result == Temporal::OwnerResult::Ok && (!plan || Cleanup(*plan));
  }
  bool Event(const Temporal::SessionEvent& event)
  {
    if (event.runtime != runtime) return false;
    if (!event.attach) return current != event.attachment || Close();
    if (!broker)
    {
      if (!Close()) return false;
      broker = std::pair(event.attachment.broker_instance, event.attachment.registration);
    }
    if (*broker != std::pair(event.attachment.broker_instance, event.attachment.registration) ||
        (current && current != event.attachment)) return false;
    if (owner.Attach(event.attachment) != Temporal::OwnerResult::Ok) return false;
    current = event.attachment;
    return true;
  }
  OwnedJson Dispatch(Core::System& system, const Temporal::Request& request,
                     const std::shared_ptr<StateCall>& state_call)
  {
    const auto failure = [&](Temporal::OwnerResult result) {
      return Temporal::Error(request.id, Temporal::OwnerError(result), "native parent ownership rejected");
    };
    if (!current || s_control_retired.load()) return failure(Temporal::OwnerResult::Retired);
    if (request.method == "finish_temporal_operation")
    {
      if (!request.parent) return failure(Temporal::OwnerResult::InvalidIdentity);
      if (const auto* last = owner.Terminal(*current, *request.parent))
        return Temporal::Success(request.id, Temporal::TerminalJson(*last));
      Temporal::CleanupPlan plan;
      const auto result = owner.StartCleanup(*current, *request.parent, plan);
      if (result != Temporal::OwnerResult::Ok) return failure(result);
      if (!Cleanup(plan)) return failure(Temporal::OwnerResult::Retired);
      return Temporal::Success(request.id, Temporal::TerminalJson(plan));
    }
    if (s_request_cancelled.load())
    {
      RememberCancellation();
      return Temporal::Error(request.id, "cancelled", "request cancelled before native admission");
    }
    if (request.method == "begin_temporal_operation")
    {
      if (!request.parent) return failure(Temporal::OwnerResult::InvalidIdentity);
      const bool fresh = !owner.ActiveKey();
      const auto result = owner.Begin(*current, *request.parent);
      if (result != Temporal::OwnerResult::Ok) return failure(result);
      if (fresh) stop_deadline.reset();
      return Temporal::Success(request.id, {{"status", "admitted"}, {"parent", Temporal::KeyJson(*request.parent)}});
    }
    const bool observation = ObservationMethod(request.method) &&
        request.method != "execution_speed" && request.method != "save_state" &&
        request.method != "list_breakpoints";
    if (request.parent)
    {
      auto result = owner.Authorize(*current, *request.parent);
      if (result != Temporal::OwnerResult::Ok) return failure(result);
      if (!observation)
      {
        if (request.method != "pause" && request.method != "step" && request.method != "set_input")
          return Temporal::Error(request.id, "unsupported", "method is outside the native parent operation");
        if (request.method == "step" && !request.child)
          return failure(Temporal::OwnerResult::InvalidIdentity);
        if (request.child && (request.child->runtime != request.parent->runtime ||
            request.child->owner_id != request.parent->owner_id || *request.child == *request.parent))
          return failure(Temporal::OwnerResult::InvalidIdentity);
        if (request.method == "set_input")
        {
          // The public Dolphin input contract supports port zero. Validate before acquiring it.
          const auto& params = request.params;
          for (const char* field : {"port", "pad"})
            if (params.contains(field) && params[field] != 0)
              return Temporal::Error(request.id, "bad_params", "only controller port 0 is supported");
          result = owner.Effect(*current, *request.parent, 0);
        }
        else result = owner.Effect(*current, *request.parent);
        if (result != Temporal::OwnerResult::Ok) return failure(result);
      }
    }
    else
    {
      const auto result = owner.AuthorizeUnscoped(*current, observation);
      if (result != Temporal::OwnerResult::Ok) return failure(result);
    }
    if (request.child && (request.child->runtime != runtime || request.method != "step"))
      return failure(Temporal::OwnerResult::InvalidIdentity);
    if (request.child && request.params.contains("unit") && request.params["unit"] != "frames")
      return Temporal::Error(request.id, "unsupported", "cancellation requires frame stepping");
    Handler handler = Lookup(request.method);
    if (!handler) return Temporal::Error(request.id, "unknown_method", request.method);
    picojson::value params;
    const std::string parse_error = picojson::parse(params, request.params.dump());
    if (!parse_error.empty() || !params.is<picojson::object>())
      return failure(Temporal::OwnerResult::InvalidIdentity);
    s_handler_error.clear(); s_handler_error_kind.clear();
    if (!ObservationMethod(request.method)) ++s_boundary_seq;
    auto result = state_call ? state_call->Run(system, handler, params.get<picojson::object>()) :
                               handler(system, params.get<picojson::object>());
    RememberCancellation();
    if (s_handler_error.empty() && !state_call)
      VerifyFrozenPublication(system, result, !ObservationMethod(request.method));
    if (!s_handler_error.empty())
      return Temporal::Error(request.id, s_handler_error_kind.empty() ? "emulator_error" :
                             s_handler_error_kind.c_str(), s_handler_error);
    auto value = OwnedJson::parse(picojson::value(result).serialize(), nullptr, false);
    if (value.is_discarded()) return Temporal::Error(request.id, "protocol_error", "invalid native reply");
    if (request.method == "hello" || request.method == "status")
    {
      value["control_session_lifecycle"] = true;
      value["temporal_cancellation_capability"] = {
          {"methods", {"step"}}, {"control_service_ms", 50}, {"stop_host_ms", 5000}};
      if (value.contains("methods")) value["methods"].push_back("cancel_operation");
    }
    return Temporal::Success(request.id, std::move(value));
  }
};

void ServeOwnedSession(Core::System& system, SOCKET sock)
{
#ifdef _WIN32
  u_long nonblocking = 1;
  if (ioctlsocket(sock, FIONBIO, &nonblocking) != 0) return;
#else
  const int flags = fcntl(sock, F_GETFL, 0);
  if (flags < 0 || fcntl(sock, F_SETFL, flags | O_NONBLOCK) < 0) return;
#endif
  static uint64_t direct_session = 0;
  OwnedSession session(EnvOr("EMUCAP_LAUNCH_ID", ""));
  Temporal::Attachment direct{"direct-" + session.runtime, 1, ++direct_session};
  if (session.owner.Attach(direct) != Temporal::OwnerResult::Ok) return;
  session.current = direct;
  std::string pending;
  bool connected = true;
  while (connected && !s_stop.load() && !s_control_retired.load())
  {
    OwnedJson value;
    const auto read = ReadOwned(sock, pending, value);
    if (read == OwnedRead::Idle) continue;
    if (read == OwnedRead::Closed) break;
    if (value.contains("_control_session"))
    {
      Temporal::SessionEvent event;
      if (!Temporal::ReadEvent(value, event) || !session.Event(event)) break;
      continue;
    }
    Temporal::Request request;
    if (!session.current || !Temporal::ReadRequest(value, request) ||
        !Temporal::Authenticate(request, *session.current, session.broker.has_value())) break;
    if (request.method == "cancel_operation")
    {
      Temporal::OperationKey key;
      if (!Temporal::ReadKey(request.params, key)) break;
      connected = OwnedWrite(sock, Temporal::Success(request.id, {{"status", "not_active"}}));
      continue;
    }
    const auto active = request.child ? request.child : request.parent ? request.parent : session.owner.ActiveKey();
    if (active && active->runtime != session.runtime) break;
    ResetRequestCancellation();
    // State I/O joins native completion before dispatch publishes its result.
    const auto state_call = request.method == "save_state" || request.method == "load_state" ?
                                std::make_shared<StateCall>() : nullptr;
    auto worker = std::async(std::launch::async, [&] {
      return session.Dispatch(system, request, state_call);
    });
    auto last_progress = OwnedClock::now();
    std::optional<Temporal::SessionEvent> detached;
    while (worker.wait_for(std::chrono::milliseconds(0)) != std::future_status::ready)
    {
      if (!connected || detached || s_stop.load())
      {
        CancelRequest();
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
        continue;
      }
      OwnedJson control;
      const auto incoming = ReadOwned(sock, pending, control);
      if (incoming == OwnedRead::Closed) { connected = false; CancelRequest(); continue; }
      if (incoming == OwnedRead::Ready)
      {
        if (control.contains("_control_session"))
        {
          Temporal::SessionEvent event;
          if (!Temporal::ReadEvent(control, event) || event.runtime != session.runtime || event.attach)
          { connected = false; CancelRequest(); continue; }
          if (session.current == event.attachment) { detached = event; CancelRequest(); }
        }
        else
        {
          Temporal::Request cancel;
          if (!Temporal::ReadRequest(control, cancel) || !session.current ||
              !Temporal::Authenticate(cancel, *session.current, session.broker.has_value()))
          { connected = false; CancelRequest(); continue; }
          OwnedJson reply;
          if (cancel.method == "cancel_operation")
          {
            Temporal::OperationKey target;
            if (!Temporal::ReadKey(cancel.params, target))
              reply = Temporal::Error(cancel.id, "bad_params", "invalid cancellation identity");
            else
            {
              const bool matches = active && *active == target;
              if (matches) CancelRequest();
              reply = Temporal::Success(cancel.id, {{"status", matches ? "requested" : "not_active"}});
            }
          }
          else reply = Temporal::Error(cancel.id, "busy", "native operation is active");
          if (!OwnedWrite(sock, reply)) { connected = false; CancelRequest(); }
        }
      }
      if (connected && !detached && OwnedClock::now() - last_progress >= std::chrono::seconds(1))
      {
        if (!OwnedWrite(sock, Temporal::Success(request.id, {{"status", "working"}})))
        { connected = false; CancelRequest(); }
        last_progress = OwnedClock::now();
      }
    }
    auto reply = worker.get();
    if (connected && !OwnedWrite(sock, reply)) { connected = false; CancelRequest(); }
    if (detached && !session.Event(*detached)) connected = false;
  }
  if (!session.Close()) s_control_retired.store(true);
}
