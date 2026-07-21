// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Native Dolphin adapter for GameCube and Wii. A dedicated thread connects to the Control MCP
// listener and translates NDJSON requests into Dolphin APIs.

#include "Core/EmuCap.h"

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <cctype>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <deque>
#include <fstream>
#include <iterator>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <arpa/inet.h>
#include <sys/socket.h>
#include <unistd.h>
using SOCKET = int;
#define INVALID_SOCKET (-1)
#define closesocket close
#endif

#include <picojson.h>

#include "Common/Config/Config.h"
#include "Common/Event.h"
#include "Common/FileUtil.h"
#include "Common/SocketContext.h"
#include "Core/Config/MainSettings.h"
#include "Core/Core.h"
#include "Core/Debugger/Debugger_SymbolMap.h"
#include "Core/Debugger/PPCDebugInterface.h"
#include "Core/HW/CPU.h"
#include "Core/HW/Memmap.h"
#include "Core/PowerPC/BreakPoints.h"
#include "Core/PowerPC/Gekko.h"
#include "Core/PowerPC/JitInterface.h"
#include "Core/PowerPC/MMU.h"
#include "Core/PowerPC/PowerPC.h"
#include "Core/State.h"
#include "Core/System.h"
#include "InputCommon/GCPadStatus.h"
#include "VideoCommon/FrameDumper.h"

namespace EmuCap
{
namespace
{
std::thread s_thread;
std::atomic<bool> s_stop{false};
std::atomic<bool> s_started{false};

std::mutex s_socket_mutex;
SOCKET s_active_socket = INVALID_SOCKET;

std::mutex s_ev_mutex;
std::deque<picojson::value> s_events;

std::mutex s_bp_mutex;
std::map<int, u32> s_breakpoints;  // id -> address
int s_next_bp = 1;
std::atomic<u64> s_file_sequence{0};
std::string s_handler_error_kind;
std::string s_handler_error;

constexpr uint64_t MAX_SYNC_ADVANCE_COUNT = 15;
constexpr uint64_t MAX_MEMORY_READ_BYTES = 16 * 1024 * 1024;
constexpr uint64_t MAX_MEMORY_WRITE_BYTES = 16 * 1024;

std::mutex s_frame_step_mutex;
std::condition_variable s_frame_step_cv;
u64 s_frame_step_completions = 0;
u64 s_breakpoint_interruptions = 0;

// Per-controller set_input override. An engaged override replaces GCPad::GetStatus output.
std::mutex s_input_mutex;
struct InputOverride
{
  bool engaged = false;
  GCPadStatus status;  // The default constructor initializes a neutral controller state.
};
InputOverride s_input[4];

std::string Base64(const uint8_t* data, size_t n)
{
  static const char* T = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  std::string out;
  out.reserve((n + 2) / 3 * 4);
  size_t i = 0;
  for (; i + 3 <= n; i += 3)
  {
    uint32_t v = (data[i] << 16) | (data[i + 1] << 8) | data[i + 2];
    out.push_back(T[(v >> 18) & 63]);
    out.push_back(T[(v >> 12) & 63]);
    out.push_back(T[(v >> 6) & 63]);
    out.push_back(T[v & 63]);
  }
  if (n - i == 1)
  {
    uint32_t v = data[i] << 16;
    out.push_back(T[(v >> 18) & 63]);
    out.push_back(T[(v >> 12) & 63]);
    out.push_back('=');
    out.push_back('=');
  }
  else if (n - i == 2)
  {
    uint32_t v = (data[i] << 16) | (data[i + 1] << 8);
    out.push_back(T[(v >> 18) & 63]);
    out.push_back(T[(v >> 12) & 63]);
    out.push_back(T[(v >> 6) & 63]);
    out.push_back('=');
  }
  return out;
}

bool ButtonBit(const std::string& name, u16& bit)
{
  std::string normalized;
  normalized.reserve(name.size());
  for (const unsigned char character : name)
    normalized.push_back(static_cast<char>(std::tolower(character)));

  if (normalized == "a") bit = PAD_BUTTON_A;
  else if (normalized == "b") bit = PAD_BUTTON_B;
  else if (normalized == "x") bit = PAD_BUTTON_X;
  else if (normalized == "y") bit = PAD_BUTTON_Y;
  else if (normalized == "start") bit = PAD_BUTTON_START;
  else if (normalized == "z") bit = PAD_TRIGGER_Z;
  else if (normalized == "l") bit = PAD_TRIGGER_L;
  else if (normalized == "r") bit = PAD_TRIGGER_R;
  else if (normalized == "up") bit = PAD_BUTTON_UP;
  else if (normalized == "down") bit = PAD_BUTTON_DOWN;
  else if (normalized == "left") bit = PAD_BUTTON_LEFT;
  else if (normalized == "right") bit = PAD_BUTTON_RIGHT;
  else return false;
  return true;
}

std::string EnvOr(const char* key, const char* fallback)
{
  const char* v = std::getenv(key);
  return v ? std::string(v) : std::string(fallback);
}

std::string ToHex(const uint8_t* data, size_t n)
{
  static const char* k = "0123456789abcdef";
  std::string out;
  out.reserve(n * 2);
  for (size_t i = 0; i < n; ++i)
  {
    out.push_back(k[data[i] >> 4]);
    out.push_back(k[data[i] & 0xF]);
  }
  return out;
}

bool FromHex(const std::string& s, std::vector<uint8_t>& out)
{
  if (s.size() % 2)
    return false;
  auto nib = [](char c) -> int {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
  };
  out.clear();
  for (size_t i = 0; i < s.size(); i += 2)
  {
    int hi = nib(s[i]), lo = nib(s[i + 1]);
    if (hi < 0 || lo < 0)
      return false;
    out.push_back(static_cast<uint8_t>((hi << 4) | lo));
  }
  return true;
}

// Read an integer parameter from either a JSON number or a prefixed string.
bool GetU64(const picojson::object& p, const char* key, uint64_t& out)
{
  auto it = p.find(key);
  if (it == p.end())
    return false;
  if (it->second.is<double>())
  {
    const double value = it->second.get<double>();
    if (!std::isfinite(value) || value < 0 || std::floor(value) != value ||
        value >= 18446744073709551616.0)
    {
      return false;
    }
    out = static_cast<uint64_t>(value);
    return true;
  }
  if (it->second.is<std::string>())
  {
    const std::string& s = it->second.get<std::string>();
    if (s.empty() || s.front() == '+' || s.front() == '-')
      return false;
    size_t offset = 0;
    int base = 10;
    if (s.front() == '$')
    {
      offset = 1;
      base = 16;
    }
    else if (s.size() >= 2 && s.front() == '0' && (s[1] == 'x' || s[1] == 'X'))
    {
      offset = 2;
      base = 16;
    }
    if (offset == s.size())
      return false;
    errno = 0;
    char* end = nullptr;
    const char* begin = s.c_str() + offset;
    const unsigned long long value = std::strtoull(begin, &end, base);
    if (errno == ERANGE || end == begin || *end != '\0')
      return false;
    out = static_cast<uint64_t>(value);
    return true;
  }
  return false;
}

void PushEvent(picojson::value ev)
{
  std::lock_guard<std::mutex> lk(s_ev_mutex);
  s_events.push_back(std::move(ev));
}

// Request handlers return a result object on success and set the request-scoped error on failure.

picojson::value MakeError(const std::string& kind, const std::string& msg)
{
  picojson::object e;
  e["kind"] = picojson::value(kind);
  e["message"] = picojson::value(msg);
  return picojson::value(e);
}

picojson::object Fail(const std::string& message)
{
  s_handler_error_kind = "emulator_error";
  s_handler_error = message;
  return {};
}

picojson::object Fail(const std::string& kind, const std::string& message)
{
  s_handler_error_kind = kind;
  s_handler_error = message;
  return {};
}

// Guard PowerPC and memory access against concurrent CPU-thread execution.
struct SafeAccess
{
  Core::System& system;
  Core::CPUThreadGuard guard;
  explicit SafeAccess(Core::System& sys) : system(sys), guard(sys) {}
};

picojson::object CpuState(const auto& ppc)
{
  picojson::object state;
  state["cpu.pc"] = picojson::value(static_cast<double>(ppc.pc));
  for (int i = 0; i < 32; ++i)
    state["cpu.r" + std::to_string(i)] = picojson::value(static_cast<double>(ppc.gpr[i]));
  state["cpu.lr"] = picojson::value(static_cast<double>(ppc.spr[SPR_LR]));
  state["cpu.ctr"] = picojson::value(static_cast<double>(ppc.spr[SPR_CTR]));
  state["cpu.xer"] = picojson::value(static_cast<double>(ppc.spr[SPR_XER]));
  state["cpu.msr"] = picojson::value(static_cast<double>(ppc.msr.Hex));
  state["cpu.cr"] = picojson::value(static_cast<double>(ppc.cr.Get()));
  return state;
}

void AddExecutionLimits(picojson::object& result)
{
  picojson::object limits;
  limits["max_sync_advance_count"] =
      picojson::value(static_cast<double>(MAX_SYNC_ADVANCE_COUNT));
  result["execution_limits"] = picojson::value(limits);
}

picojson::object Hello(Core::System&, const picojson::object&)
{
  const std::string system = EnvOr("EMUCAP_SYSTEM", "gamecube");
  const bool gamecube = system == "gamecube" || system == "gc" || system == "ngc";
  picojson::object r;
  r["protocol_version"] = picojson::value(1.0);
  r["name"] = picojson::value(EnvOr("EMUCAP_NAME", "dolphin"));
  r["system"] = picojson::value(system);
  r["adapter"] = picojson::value(std::string("dolphin-native"));
  picojson::array methods;
  for (const char* m :
       {"read_memory", "write_memory", "get_state", "status", "pause", "resume",
        "step", "step_instructions", "set_breakpoint", "clear_breakpoint", "list_breakpoints",
        "clear_all_breakpoints", "poll_events", "disassemble", "call_stack", "save_state",
        "load_state", "screenshot"})
  {
    methods.push_back(picojson::value(std::string(m)));
  }
  if (gamecube)
    methods.push_back(picojson::value(std::string("set_input")));
  r["methods"] = picojson::value(methods);
  picojson::object breakpoint_kind;
  breakpoint_kind["kind"] = picojson::value(std::string("exec"));
  breakpoint_kind["range_unit"] = picojson::value(std::string("address"));
  breakpoint_kind["range_mode"] = picojson::value(std::string("exact"));
  breakpoint_kind["memory_type_used"] = picojson::value(false);
  breakpoint_kind["snapshot"] = picojson::value(false);
  picojson::array breakpoint_kinds;
  breakpoint_kinds.push_back(picojson::value(breakpoint_kind));
  r["breakpoint_kinds"] = picojson::value(breakpoint_kinds);
  picojson::array active_exceptions;
  for (const char* id :
       {"dolphin.breakpoint.exact-exec-only", "dolphin.state-save.frozen-only",
        "dolphin.state-load.frozen-only", "dolphin.screenshot.running-only",
        "dolphin.call-stack.best-effort"})
  {
    active_exceptions.push_back(picojson::value(std::string(id)));
  }
  if (gamecube)
  {
    active_exceptions.push_back(
        picojson::value(std::string("dolphin.input-hold.port-zero-only")));
  }
  picojson::object contracts;
  contracts["catalog"] =
      picojson::value(std::string("emucap-feature-contracts/v3"));
  contracts["active_exceptions"] = picojson::value(active_exceptions);
  r["contracts"] = picojson::value(contracts);
  AddExecutionLimits(r);
  picojson::array mt;
  mt.push_back(picojson::value(std::string("main")));
  r["memory_types"] = picojson::value(mt);
  const std::string tok = EnvOr("EMUCAP_SESSION_TOKEN", "");
  if (!tok.empty())
    r["session_token"] = picojson::value(tok);
  const std::string content = EnvOr("EMUCAP_CONTENT", "");
  if (!content.empty())
    r["content"] = picojson::value(content);
  const std::string launch_id = EnvOr("EMUCAP_LAUNCH_ID", "");
  if (!launch_id.empty())
    r["launch_id"] = picojson::value(launch_id);
  return r;
}

picojson::object Status(Core::System& system, const picojson::object&)
{
  picojson::object r;
  const Core::State st = Core::GetState(system);
  r["connected"] = picojson::value(true);
  r["state"] = picojson::value(std::string(st == Core::State::Paused ? "frozen" : "running"));
  r["adapter"] = picojson::value(std::string("dolphin-native"));
  // Lightweight breakpoint diagnostics. dbg_effective is Config::IsDebuggingEnabled()
  // (MAIN_ENABLE_DEBUGGING and not achievements-hardcore) and must be true for core checks.
  // cpu_core: 0=Interpreter, 1=JIT64, 4=JITARM64, 5=CachedInterpreter.
  r["dbg_config"] = picojson::value(Config::Get(Config::MAIN_ENABLE_DEBUGGING));
  r["dbg_effective"] = picojson::value(Config::IsDebuggingEnabled());
  r["cpu_core"] = picojson::value(static_cast<double>(static_cast<int>(Config::Get(Config::MAIN_CPU_CORE))));
  {
    std::lock_guard<std::mutex> lk(s_bp_mutex);
    r["breaking_enabled"] = picojson::value(true);
    r["dolphin_bp_count"] =
        picojson::value(static_cast<double>(s_breakpoints.size()));
  }
  {
    std::lock_guard<std::mutex> lk(s_input_mutex);
    picojson::object input_override;
    input_override["observable"] = picojson::value(true);
    input_override["authority"] = picojson::value(std::string("adapter_local"));
    input_override["engaged"] = picojson::value(s_input[0].engaged);
    input_override["mode"] =
        picojson::value(std::string(s_input[0].engaged ? "persistent" : "native"));
    r["input_override"] = picojson::value(input_override);
  }
  AddExecutionLimits(r);
  return r;
}

picojson::object ReadMemory(Core::System& system, const picojson::object& p)
{
  uint64_t addr = 0, len = 0;
  if (!GetU64(p, "address", addr) || !GetU64(p, "length", len))
    return Fail("bad_params", "read_memory requires integer address and length");
  if (addr > UINT32_MAX || len > MAX_MEMORY_READ_BYTES ||
      len > 0x100000000ULL - addr)
  {
    return Fail("bad_params", "read_memory range is outside the bounded 32-bit memory space");
  }
  std::vector<uint8_t> buf(static_cast<size_t>(len));
  {
    SafeAccess sa(system);
    if (len != 0 &&
        system.GetMemory().GetPointerForRange(static_cast<u32>(addr), buf.size()) == nullptr)
    {
      return Fail("bad_params", "read_memory range is not mapped Dolphin memory");
    }
    system.GetMemory().CopyFromEmu(buf.data(), static_cast<u32>(addr), buf.size());
  }
  picojson::object r;
  r["hex"] = picojson::value(ToHex(buf.data(), buf.size()));
  return r;
}

picojson::object WriteMemory(Core::System& system, const picojson::object& p)
{
  uint64_t addr = 0;
  auto it = p.find("hex");
  if (!GetU64(p, "address", addr) || it == p.end() || !it->second.is<std::string>())
    return Fail("bad_params", "write_memory requires an integer address and hex bytes");
  std::vector<uint8_t> data;
  if (!FromHex(it->second.get<std::string>(), data))
    return Fail("bad_params", "write_memory hex must contain complete hexadecimal bytes");
  if (addr > UINT32_MAX || data.size() > MAX_MEMORY_WRITE_BYTES ||
      data.size() > 0x100000000ULL - addr)
  {
    return Fail("bad_params", "write_memory range is outside the bounded 32-bit memory space");
  }
  {
    SafeAccess sa(system);
    if (!data.empty() &&
        system.GetMemory().GetPointerForRange(static_cast<u32>(addr), data.size()) == nullptr)
    {
      return Fail("bad_params", "write_memory range is not mapped Dolphin memory");
    }
    system.GetMemory().CopyToEmu(static_cast<u32>(addr), data.data(), data.size());
  }
  picojson::object r;
  r["written"] = picojson::value(static_cast<double>(data.size()));
  return r;
}

picojson::object GetState(Core::System& system, const picojson::object&)
{
  picojson::object state;
  {
    SafeAccess sa(system);
    state = CpuState(system.GetPPCState());
  }
  picojson::object r;
  r["state"] = picojson::value(state);
  return r;
}

picojson::object Disassemble(Core::System& system, const picojson::object& p)
{
  uint64_t address = 0;
  uint64_t count = 8;
  if (!GetU64(p, "address", address))
    return Fail("bad_params", "disassemble requires an address");
  if (p.count("count") && !GetU64(p, "count", count))
    return Fail("bad_params", "disassemble count must be an integer");
  if (address > UINT32_MAX || (address & 3) != 0)
    return Fail("bad_params", "PowerPC disassembly address must be an aligned 32-bit address");
  if (count == 0 || count > 256)
    return Fail("bad_params", "disassemble count must be in 1..256");
  if (address + (count - 1) * 4 > UINT32_MAX)
    return Fail("bad_params", "disassemble range exceeds the 32-bit address space");

  picojson::array instructions;
  {
    SafeAccess sa(system);
    const auto& debug = system.GetPowerPC().GetDebugInterface();
    for (uint64_t index = 0; index < count; ++index)
    {
      const u32 current = static_cast<u32>(address + index * 4);
      if (!PowerPC::MMU::HostIsRAMAddress(sa.guard, current))
        return Fail("bad_params", "disassemble address is not mapped PowerPC RAM");
      const u32 opcode = debug.ReadInstruction(sa.guard, current);
      const uint8_t bytes[] = {
          static_cast<uint8_t>(opcode >> 24),
          static_cast<uint8_t>(opcode >> 16),
          static_cast<uint8_t>(opcode >> 8),
          static_cast<uint8_t>(opcode),
      };
      picojson::object instruction;
      instruction["addr"] = picojson::value(static_cast<double>(current));
      instruction["bytes"] = picojson::value(ToHex(bytes, sizeof(bytes)));
      instruction["text"] = picojson::value(debug.Disassemble(&sa.guard, current));
      instructions.push_back(picojson::value(instruction));
    }
  }

  picojson::object r;
  r["instructions"] = picojson::value(instructions);
  return r;
}

picojson::object CallStack(Core::System& system, const picojson::object&)
{
  std::vector<Dolphin_Debugger::CallstackEntry> entries;
  bool valid = false;
  {
    SafeAccess sa(system);
    valid = Dolphin_Debugger::GetCallstack(sa.guard, entries);
  }

  picojson::array frames;
  for (auto entry = entries.rbegin(); entry != entries.rend(); ++entry)
  {
    picojson::object frame;
    frame["pc"] = picojson::value(static_cast<double>(entry->vAddress));
    std::string text = entry->Name;
    while (!text.empty() && (text.back() == '\n' || text.back() == '\r'))
      text.pop_back();
    frame["text"] = picojson::value(text);
    frames.push_back(picojson::value(frame));
  }

  picojson::object r;
  r["call_stack"] = picojson::value(frames);
  r["depth"] = picojson::value(static_cast<double>(entries.size()));
  r["valid"] = picojson::value(valid);
  return r;
}

picojson::object Pause(Core::System& system, const picojson::object&)
{
  Core::SetState(system, Core::State::Paused);
  picojson::object r;
  r["state"] = picojson::value(std::string("frozen"));
  return r;
}

picojson::object Resume(Core::System& system, const picojson::object&)
{
  Core::SetState(system, Core::State::Running);
  picojson::object r;
  r["state"] = picojson::value(std::string("running"));
  return r;
}

picojson::object StepInstructions(Core::System& system, const picojson::object& p)
{
  uint64_t count = 1;
  if (p.count("count") && !GetU64(p, "count", count))
    return Fail("bad_params", "count must be an integer");
  if (count == 0 || count > MAX_SYNC_ADVANCE_COUNT)
    return Fail("bad_params", "instruction count must be in 1..15");

  auto& cpu = system.GetCPU();
  cpu.SetStepping(true);
  auto& power_pc = system.GetPowerPC();
  const PowerPC::CoreMode old_mode = power_pc.GetMode();
  power_pc.SetMode(PowerPC::CoreMode::Interpreter);
  bool completed_all = true;
  bool can_restore_mode = true;
  for (uint64_t i = 0; i < count; ++i)
  {
    Common::Event completed;
    cpu.StepOpcode(&completed);
    if (!completed.WaitFor(std::chrono::seconds(1)))
    {
      can_restore_mode = cpu.CancelStepOpcode(&completed);
      completed_all = false;
      break;
    }
  }
  if (can_restore_mode)
    power_pc.SetMode(old_mode);
  if (!completed_all)
    return Fail("instruction step did not complete within 1 second");
  picojson::object r;
  r["status"] = picojson::value(std::string("completed"));
  r["count"] = picojson::value(static_cast<double>(count));
  r["state"] = picojson::value(std::string("frozen"));
  return r;
}

picojson::object StepFrames(Core::System& system, const picojson::object& p)
{
  uint64_t count = 1;
  if (p.count("frames"))
  {
    if (!GetU64(p, "frames", count))
      return Fail("bad_params", "frames must be an integer");
  }
  else if (p.count("count") && !GetU64(p, "count", count))
  {
    return Fail("bad_params", "count must be an integer");
  }
  if (count == 0 || count > MAX_SYNC_ADVANCE_COUNT)
    return Fail("bad_params", "frame count must be in 1..15");
  if (Core::GetState(system) != Core::State::Paused)
    return Fail("bad_state", "frame step requires a frozen core");

  struct FrameStart
  {
    Common::Event dispatched;
    std::atomic<bool> cancelled{false};
    std::atomic<bool> accepted{false};
  };
  const auto cancel_frame_step = [] {
    auto cancelled = std::make_shared<Common::Event>();
    Core::QueueHostJob([cancelled](Core::System& host_system) {
      Core::CancelFrameStep(host_system);
      cancelled->Set();
    });
    return cancelled->WaitFor(std::chrono::seconds(1));
  };

  const auto operation_deadline = std::chrono::steady_clock::now() + std::chrono::seconds(4);
  uint64_t completed = 0;
  for (; completed < count; ++completed)
  {
    u64 completion_before = 0;
    u64 interruption_before = 0;
    {
      std::lock_guard<std::mutex> lock(s_frame_step_mutex);
      completion_before = s_frame_step_completions;
      interruption_before = s_breakpoint_interruptions;
    }

    auto start = std::make_shared<FrameStart>();
    Core::QueueHostJob([start](Core::System& host_system) {
      if (!start->cancelled.load())
        start->accepted.store(Core::DoFrameStep(host_system));
      start->dispatched.Set();
    });

    const auto dispatch_remaining = operation_deadline - std::chrono::steady_clock::now();
    if (dispatch_remaining <= std::chrono::steady_clock::duration::zero() ||
        !start->dispatched.WaitFor(
            std::min(std::chrono::duration_cast<std::chrono::milliseconds>(dispatch_remaining),
                     std::chrono::milliseconds(1000))))
    {
      start->cancelled.store(true);
      if (!cancel_frame_step())
      {
        return Fail("timeout",
                    "frame step dispatch timed out and cleanup did not complete on the host thread");
      }
      return Fail("timeout", "frame step was not dispatched before the operation deadline");
    }
    if (!start->accepted.load())
      return Fail("bad_state", "Dolphin did not accept frame step from the frozen state");

    std::unique_lock<std::mutex> lock(s_frame_step_mutex);
    const auto completion_deadline =
        std::min(operation_deadline, std::chrono::steady_clock::now() + std::chrono::seconds(2));
    const bool signaled = s_frame_step_cv.wait_until(lock, completion_deadline, [&] {
      return s_frame_step_completions != completion_before ||
             s_breakpoint_interruptions != interruption_before || s_stop.load();
    });
    const bool frame_completed = s_frame_step_completions != completion_before;
    const bool interrupted = s_breakpoint_interruptions != interruption_before;
    lock.unlock();

    if (frame_completed)
      continue;

    if (!cancel_frame_step())
      return Fail("timeout", "frame step stopped but cleanup did not complete on the host thread");
    if (interrupted)
    {
      picojson::object r;
      r["status"] = picojson::value(std::string("interrupted"));
      r["count"] = picojson::value(static_cast<double>(completed));
      r["requested"] = picojson::value(static_cast<double>(count));
      r["state"] = picojson::value(std::string("frozen"));
      return r;
    }
    if (!signaled || std::chrono::steady_clock::now() >= operation_deadline)
      return Fail("timeout", "frame step did not reach a new presented frame");
    return Fail("not_connected", "Dolphin stopped while frame step was in progress");
  }

  picojson::object r;
  r["status"] = picojson::value(std::string("completed"));
  r["count"] = picojson::value(static_cast<double>(completed));
  r["state"] = picojson::value(std::string("frozen"));
  return r;
}

picojson::object SetBreakpoint(Core::System& system, const picojson::object& p)
{
  const auto kind_it = p.find("kind");
  if (kind_it != p.end() &&
      (!kind_it->second.is<std::string>() || kind_it->second.get<std::string>() != "exec"))
  {
    return Fail("bad_params", "only exec breakpoints are supported");
  }
  uint64_t addr = 0;
  if (!GetU64(p, "start", addr))
    return Fail("bad_params", "start is required");
  uint64_t end = addr;
  if (p.count("end") && !GetU64(p, "end", end))
    return Fail("bad_params", "end must be an integer");
  if (end != addr)
    return Fail("bad_params", "only exact-address breakpoints are supported");
  if (addr > UINT32_MAX || (addr & 3) != 0)
    return Fail("bad_params", "PowerPC exec breakpoint address must be aligned and 32-bit");
  const auto pause_it = p.find("pause_on_hit");
  if (pause_it != p.end() &&
      (!pause_it->second.is<bool>() || !pause_it->second.get<bool>()))
  {
    return Fail("bad_params", "Dolphin breakpoints must pause on hit");
  }
  const auto auto_state_it = p.find("auto_savestate");
  if (auto_state_it != p.end() &&
      (!auto_state_it->second.is<bool>() || auto_state_it->second.get<bool>()))
  {
    return Fail("bad_params", "auto_savestate is not supported");
  }
  for (const char* field :
       {"value", "value_mask", "value_len", "pc_min", "pc_max", "snapshot"})
  {
    if (p.count(field))
      return Fail("bad_params", std::string(field) + " is not supported for Dolphin breakpoints");
  }
  const int id = s_next_bp++;
  {
    std::lock_guard<std::mutex> lk(s_bp_mutex);
    s_breakpoints[id] = static_cast<u32>(addr);
  }
  {
    SafeAccess sa(system);
    auto& breakpoints = system.GetPowerPC().GetBreakPoints();
    breakpoints.EnableBreaking(true);
    breakpoints.Add(static_cast<u32>(addr));
    // Existing JIT and CachedInterpreter blocks may lack the new breakpoint check. Clear the
    // complete cache so recompilation inserts it; invalidating one instruction is insufficient.
    system.GetJitInterface().ClearCache(sa.guard);
  }
  picojson::object r;
  r["id"] = picojson::value(static_cast<double>(id));
  return r;
}

picojson::object ClearBreakpoint(Core::System& system, const picojson::object& p)
{
  uint64_t id = 0;
  if (!GetU64(p, "id", id))
    return Fail("id required");
  u32 address = 0;
  {
    std::lock_guard<std::mutex> lk(s_bp_mutex);
    const auto it = s_breakpoints.find(static_cast<int>(id));
    if (it == s_breakpoints.end())
      return Fail("breakpoint not found");
    address = it->second;
  }
  {
    SafeAccess sa(system);
    bool address_still_used = false;
    {
      std::lock_guard<std::mutex> lk(s_bp_mutex);
      s_breakpoints.erase(static_cast<int>(id));
      for (const auto& entry : s_breakpoints)
      {
        if (entry.second == address)
        {
          address_still_used = true;
          break;
        }
      }
    }
    if (!address_still_used)
    {
      system.GetPowerPC().GetBreakPoints().Remove(address);
      system.GetJitInterface().ClearCache(sa.guard);
    }
  }
  picojson::object r;
  r["cleared"] = picojson::value(static_cast<double>(id));
  return r;
}

picojson::object ListBreakpoints(Core::System&, const picojson::object&)
{
  picojson::array bps;
  std::lock_guard<std::mutex> lk(s_bp_mutex);
  for (const auto& [id, addr] : s_breakpoints)
  {
    picojson::object b;
    b["id"] = picojson::value(static_cast<double>(id));
    b["kind"] = picojson::value(std::string("exec"));
    b["start"] = picojson::value(static_cast<double>(addr));
    b["end"] = picojson::value(static_cast<double>(addr));
    bps.push_back(picojson::value(b));
  }
  picojson::object r;
  r["breakpoints"] = picojson::value(bps);
  return r;
}

picojson::object ClearAllBreakpoints(Core::System& system, const picojson::object&)
{
  std::vector<u32> addresses;
  size_t cleared = 0;
  {
    std::lock_guard<std::mutex> lk(s_bp_mutex);
    cleared = s_breakpoints.size();
    addresses.reserve(s_breakpoints.size());
    for (const auto& [id, address] : s_breakpoints)
    {
      (void)id;
      if (std::find(addresses.begin(), addresses.end(), address) == addresses.end())
        addresses.push_back(address);
    }
    s_breakpoints.clear();
  }
  {
    SafeAccess sa(system);
    auto& breakpoints = system.GetPowerPC().GetBreakPoints();
    for (const u32 address : addresses)
      breakpoints.Remove(address);
    if (!addresses.empty())
      system.GetJitInterface().ClearCache(sa.guard);
  }
  picojson::object r;
  r["cleared"] = picojson::value(static_cast<double>(cleared));
  return r;
}

picojson::object PollEvents(Core::System&, const picojson::object&)
{
  picojson::array out;
  {
    std::lock_guard<std::mutex> lk(s_ev_mutex);
    while (!s_events.empty())
    {
      out.push_back(std::move(s_events.front()));
      s_events.pop_front();
    }
  }
  picojson::object r;
  r["events"] = picojson::value(out);
  r["dropped"] = picojson::value(0.0);
  return r;
}

picojson::object SaveState(Core::System& system, const picojson::object& p)
{
  const auto path_value = p.find("path");
  if (path_value == p.end() || !path_value->second.is<std::string>() ||
      path_value->second.get<std::string>().empty())
  {
    return Fail("bad_params", "save_state requires a non-empty path");
  }
  if (Core::GetState(system) != Core::State::Paused)
    return Fail("bad_state", "save_state requires a frozen core");

  const std::string path = path_value->second.get<std::string>();
  if (!File::CreateFullPath(path))
    return Fail("io_error", "failed to create the savestate directory");
  const std::string staging =
      path + ".emucap-" + std::to_string(s_file_sequence.fetch_add(1)) + ".stage";
  std::remove(staging.c_str());
  if (!State::SaveAsSynchronous(system, staging))
  {
    std::remove(staging.c_str());
    return Fail("emulator_error", "Dolphin failed to create a complete savestate");
  }
  if (!File::MoveWithOverwrite(staging, path) || !File::IsFile(path))
  {
    std::remove(staging.c_str());
    return Fail("io_error", "failed to publish the completed savestate");
  }

  picojson::object r;
  r["status"] = picojson::value(std::string("saved"));
  r["path"] = picojson::value(path);
  r["bytes"] = picojson::value(static_cast<double>(File::GetSize(path)));
  r["state"] = picojson::value(std::string("frozen"));
  const std::string launch_id = EnvOr("EMUCAP_LAUNCH_ID", "");
  if (!launch_id.empty())
    r["generation"] = picojson::value(launch_id);
  return r;
}

picojson::object LoadState(Core::System& system, const picojson::object& p)
{
  const auto path_value = p.find("path");
  if (path_value == p.end() || !path_value->second.is<std::string>() ||
      path_value->second.get<std::string>().empty())
  {
    return Fail("bad_params", "load_state requires a non-empty path");
  }
  if (Core::GetState(system) != Core::State::Paused)
    return Fail("bad_state", "load_state requires a frozen core");

  const std::string path = path_value->second.get<std::string>();
  if (!File::IsFile(path))
    return Fail("bad_params", "savestate path is not a file");
  if (!State::LoadAsSynchronous(system, path))
    return Fail("emulator_error", "Dolphin failed to load a coherent savestate");

  picojson::object r;
  r["status"] = picojson::value(std::string("loaded"));
  r["path"] = picojson::value(path);
  r["state"] = picojson::value(std::string("frozen"));
  const std::string launch_id = EnvOr("EMUCAP_LAUNCH_ID", "");
  if (!launch_id.empty())
    r["generation"] = picojson::value(launch_id);
  return r;
}

picojson::object SetInput(Core::System&, const picojson::object& p)
{
  int port = 0;
  uint64_t v = 0;
  const char* port_field = p.count("port") ? "port" : (p.count("pad") ? "pad" : nullptr);
  if (port_field)
  {
    if (!GetU64(p, port_field, v))
      return Fail("bad_params", "controller port must be an integer");
    if (v > 3)
      return Fail("bad_params", "controller port must be in 0..3");
    port = static_cast<int>(v);
  }
  if (port != 0)
    return Fail("bad_params", "only controller port 0 is supported");

  std::lock_guard<std::mutex> lk(s_input_mutex);
  auto engaged = p.find("engaged");
  const auto buttons = p.find("buttons");
  if (buttons != p.end() && !buttons->second.is<picojson::array>())
    return Fail("bad_params", "buttons must be an array");
  const bool empty_buttons =
      buttons != p.end() && buttons->second.is<picojson::array>() &&
      buttons->second.get<picojson::array>().empty();
  if ((engaged != p.end() && engaged->second.is<bool>() && !engaged->second.get<bool>()) ||
      empty_buttons)
  {
    s_input[port].engaged = false;
    picojson::object r;
    r["engaged"] = picojson::value(false);
    r["port"] = picojson::value(static_cast<double>(port));
    return r;
  }

  GCPadStatus st;  // Neutral defaults.
  if (buttons != p.end() && buttons->second.is<picojson::array>())
  {
    u16 bits = 0;
    for (const auto& button : buttons->second.get<picojson::array>())
    {
      if (!button.is<std::string>())
        return Fail("bad_params", "buttons must contain strings");
      u16 bit = 0;
      if (!ButtonBit(button.get<std::string>(), bit))
        return Fail("bad_params", "unsupported GameCube button: " + button.get<std::string>());
      bits |= bit;
    }
    st.button = bits;
  }
  const auto set_axis = [&](const char* name, u8& destination) {
    if (!p.count(name))
      return true;
    if (!GetU64(p, name, v) || v > UINT8_MAX)
      return false;
    destination = static_cast<u8>(v);
    return true;
  };
  if (!set_axis("stickX", st.stickX) || !set_axis("stickY", st.stickY) ||
      !set_axis("substickX", st.substickX) || !set_axis("substickY", st.substickY) ||
      !set_axis("triggerL", st.triggerLeft) || !set_axis("triggerR", st.triggerRight))
  {
    return Fail("bad_params", "controller axes and triggers must be integers in 0..255");
  }
  s_input[port].status = st;
  s_input[port].engaged = true;

  picojson::object r;
  r["engaged"] = picojson::value(true);
  r["port"] = picojson::value(static_cast<double>(port));
  return r;
}

picojson::object Screenshot(Core::System& system, const picojson::object&)
{
  // Dolphin fulfills screenshot requests on the next present. Reject a frozen core before arming
  // a request: resuming behind the caller's back would make this observation mutate guest time,
  // while arming and timing out would leave work owned by a completed request.
  if (Core::GetState(system) == Core::State::Paused)
    return Fail("bad_state", "screenshot requires a running core");
  if (!g_frame_dumper)
    return Fail("bad_state", "frame dumper is not initialized");

  const u64 sequence = s_file_sequence.fetch_add(1);
  const std::string path =
      File::GetUserPath(D_CACHE_IDX) + "emucap-screenshot-" + std::to_string(sequence) + ".png";
  if (!File::CreateFullPath(path))
    return Fail("io_error", "failed to create the screenshot directory");
  std::remove(path.c_str());

  const ScreenshotWaitResult wait_result =
      g_frame_dumper->SaveScreenshotAndWait(path, std::chrono::seconds(2));
  if (wait_result == ScreenshotWaitResult::Busy)
    return Fail("bad_state", "another screenshot request is active");
  if (wait_result == ScreenshotWaitResult::TimedOut)
  {
    std::remove(path.c_str());
    return Fail("emulator_error", "screenshot did not complete within 2 seconds");
  }

  std::vector<uint8_t> bytes;
  {
    std::ifstream f(path, std::ios::binary | std::ios::ate);
    if (f)
    {
      const std::streamsize size = f.tellg();
      if (size > 0)
      {
        f.seekg(0);
        bytes.resize(static_cast<size_t>(size));
        f.read(reinterpret_cast<char*>(bytes.data()), size);
        if (!f)
          bytes.clear();
      }
    }
  }
  std::remove(path.c_str());
  static constexpr uint8_t png_signature[] = {0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a};
  if (bytes.size() < 24 ||
      !std::equal(std::begin(png_signature), std::end(png_signature), bytes.begin()))
  {
    return Fail("io_error", "screenshot output was missing or was not a PNG");
  }
  const auto read_be32 = [&bytes](size_t offset) {
    return (static_cast<u32>(bytes[offset]) << 24) |
           (static_cast<u32>(bytes[offset + 1]) << 16) |
           (static_cast<u32>(bytes[offset + 2]) << 8) |
           static_cast<u32>(bytes[offset + 3]);
  };

  picojson::object r;
  r["png_base64"] = picojson::value(Base64(bytes.data(), bytes.size()));
  r["bytes"] = picojson::value(static_cast<double>(bytes.size()));
  r["format"] = picojson::value(std::string("png"));
  r["width"] = picojson::value(static_cast<double>(read_be32(16)));
  r["height"] = picojson::value(static_cast<double>(read_be32(20)));
  r["freshness"] = picojson::value(std::string("current"));
  r["state"] = picojson::value(std::string("running"));
  const std::string launch_id = EnvOr("EMUCAP_LAUNCH_ID", "");
  if (!launch_id.empty())
    r["generation"] = picojson::value(launch_id);
  return r;
}

using Handler = picojson::object (*)(Core::System&, const picojson::object&);

Handler Lookup(const std::string& m)
{
  if (m == "hello") return Hello;
  if (m == "status") return Status;
  if (m == "read_memory") return ReadMemory;
  if (m == "write_memory") return WriteMemory;
  if (m == "get_state") return GetState;
  if (m == "disassemble") return Disassemble;
  if (m == "call_stack") return CallStack;
  if (m == "pause") return Pause;
  if (m == "resume") return Resume;
  if (m == "step") return StepFrames;
  if (m == "step_instructions") return StepInstructions;
  if (m == "set_breakpoint") return SetBreakpoint;
  if (m == "clear_breakpoint") return ClearBreakpoint;
  if (m == "list_breakpoints") return ListBreakpoints;
  if (m == "clear_all_breakpoints") return ClearAllBreakpoints;
  if (m == "poll_events") return PollEvents;
  if (m == "save_state") return SaveState;
  if (m == "load_state") return LoadState;
  if (m == "screenshot") return Screenshot;
  if (m == "set_input") return SetInput;
  return nullptr;
}

bool SendLine(SOCKET sock, const std::string& line)
{
  const std::string out = line + "\n";
  size_t offset = 0;
  while (offset < out.size())
  {
#ifdef MSG_NOSIGNAL
    constexpr int flags = MSG_NOSIGNAL;
#else
    constexpr int flags = 0;
#endif
    const int sent =
        send(sock, out.data() + offset, static_cast<int>(out.size() - offset), flags);
    if (sent <= 0)
      return false;
    offset += static_cast<size_t>(sent);
  }
  return true;
}

void ServeSession(Core::System& system, SOCKET sock)
{
  std::string buf;
  char chunk[4096];
  while (!s_stop.load())
  {
    size_t nl;
    while ((nl = buf.find('\n')) != std::string::npos)
    {
      std::string line = buf.substr(0, nl);
      buf.erase(0, nl + 1);
      if (line.empty())
        continue;

      picojson::value req;
      const std::string perr = picojson::parse(req, line);
      if (!perr.empty() || !req.is<picojson::object>())
        continue;
      const picojson::object& env = req.get<picojson::object>();
      double id = 0;
      if (auto it = env.find("id"); it != env.end() && it->second.is<double>())
        id = it->second.get<double>();
      const std::string method =
          env.count("method") ? env.at("method").to_str() : std::string();
      picojson::object params;
      if (auto it = env.find("params"); it != env.end() && it->second.is<picojson::object>())
        params = it->second.get<picojson::object>();

      picojson::object resp;
      resp["id"] = picojson::value(id);
      Handler h = Lookup(method);
      if (!h)
      {
        resp["ok"] = picojson::value(false);
        resp["error"] = MakeError("unknown_method", method);
      }
      else
      {
        s_handler_error_kind.clear();
        s_handler_error.clear();
        picojson::object result = h(system, params);
        if (s_handler_error.empty())
        {
          resp["ok"] = picojson::value(true);
          resp["result"] = picojson::value(result);
        }
        else
        {
          resp["ok"] = picojson::value(false);
          resp["error"] =
              MakeError(s_handler_error_kind.empty() ? "emulator_error" : s_handler_error_kind,
                        s_handler_error);
        }
      }
      if (!SendLine(sock, picojson::value(resp).serialize()))
        return;
    }

    const int got = recv(sock, chunk, sizeof(chunk), 0);
    if (got <= 0)
      return;
    buf.append(chunk, static_cast<size_t>(got));
  }
}

void ThreadMain(Core::System& system, unsigned short port)
{
  Common::SocketContext socket_context;  // Windows WSAStartup RAII
  while (!s_stop.load())
  {
    SOCKET sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (sock == INVALID_SOCKET)
    {
      std::this_thread::sleep_for(std::chrono::milliseconds(200));
      continue;
    }
#ifdef __APPLE__
    int no_sigpipe = 1;
    setsockopt(sock, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));
#endif
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
    if (connect(sock, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) == 0)
    {
      {
        std::lock_guard<std::mutex> lk(s_socket_mutex);
        s_active_socket = sock;
      }
      ServeSession(system, sock);
      {
        std::lock_guard<std::mutex> lk(s_socket_mutex);
        if (s_active_socket == sock)
          s_active_socket = INVALID_SOCKET;
      }
    }
    closesocket(sock);
    if (!s_stop.load())
      std::this_thread::sleep_for(std::chrono::milliseconds(200));
  }
}
}  // namespace

void ApplyInputOverride(int pad_num, GCPadStatus* status)
{
  if (pad_num < 0 || pad_num > 3 || status == nullptr)
    return;
  std::lock_guard<std::mutex> lk(s_input_mutex);
  if (s_input[pad_num].engaged)
    *status = s_input[pad_num].status;
}

void NotifyBreakpointHit(Core::System& system, u32 address)
{
  std::vector<int> ids;
  {
    std::lock_guard<std::mutex> lk(s_bp_mutex);
    for (const auto& [id, breakpoint_address] : s_breakpoints)
    {
      if (breakpoint_address == address)
        ids.push_back(id);
    }
  }

  for (const int id : ids)
  {
    picojson::object event;
    event["type"] = picojson::value(std::string("breakpoint_hit"));
    event["breakpoint_id"] = picojson::value(static_cast<double>(id));
    event["kind"] = picojson::value(std::string("exec"));
    event["address"] = picojson::value(static_cast<double>(address));
    event["pc"] = picojson::value(static_cast<double>(address));
    event["registers"] = picojson::value(CpuState(system.GetPPCState()));
    PushEvent(picojson::value(event));
  }
  if (!ids.empty())
  {
    {
      std::lock_guard<std::mutex> lock(s_frame_step_mutex);
      ++s_breakpoint_interruptions;
    }
    s_frame_step_cv.notify_all();
  }
}

void NotifyFrameStepComplete()
{
  {
    std::lock_guard<std::mutex> lock(s_frame_step_mutex);
    ++s_frame_step_completions;
  }
  s_frame_step_cv.notify_all();
}

void Start(Core::System& system)
{
  if (s_started.exchange(true))
    return;
  const char* port_env = std::getenv("EMUCAP_PORT");
  if (!port_env || !*port_env)
    return;
  const unsigned short port = static_cast<unsigned short>(std::atoi(port_env));
  if (port == 0)
    return;
  // Dolphin checks exec breakpoints only while debugging is enabled. Turn it on whenever this
  // adapter is active so breakpoint behavior does not depend on a user-profile setting.
  Config::SetBaseOrCurrent(Config::MAIN_ENABLE_DEBUGGING, true);
  s_stop.store(false);
  s_thread = std::thread([&system, port] { ThreadMain(system, port); });
}

void Stop()
{
  s_stop.store(true);
  s_frame_step_cv.notify_all();
  {
    std::lock_guard<std::mutex> lk(s_socket_mutex);
    if (s_active_socket != INVALID_SOCKET)
    {
#ifdef _WIN32
      shutdown(s_active_socket, SD_BOTH);
#else
      shutdown(s_active_socket, SHUT_RDWR);
#endif
    }
  }
  if (s_thread.joinable())
    s_thread.join();
  s_started.store(false);
}
}  // namespace EmuCap
