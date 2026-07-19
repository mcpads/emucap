// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Dolphin(GameCube/Wii) 네이티브 emucap 어댑터. 별도 스레드에서 emucap-mcp 리스너로
// 접속해 NDJSON 요청을 Dolphin 내부 API로 번역한다. GDB-스텁 브리지와 달리 savestate/
// screenshot 까지 제공한다(입력/frame 은 후속 훅 필요 — 아래 TODO).

#include "Core/EmuCap.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cctype>
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <deque>
#include <fstream>
#include <iterator>
#include <map>
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
#include "Core/HW/CPU.h"
#include "Core/HW/Memmap.h"
#include "Core/PowerPC/BreakPoints.h"
#include "Core/PowerPC/Gekko.h"
#include "Core/PowerPC/JitInterface.h"
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
std::atomic<u64> s_screenshot_sequence{0};
std::string s_handler_error_kind;
std::string s_handler_error;

// set_input 오버라이드(패드별). engaged면 GCPad::GetStatus 결과를 이 값으로 덮는다.
std::mutex s_input_mutex;
struct InputOverride
{
  bool engaged = false;
  GCPadStatus status;  // 기본 생성자가 중립(스틱 중앙) 값으로 초기화
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

// params 에서 정수 얻기(숫자 또는 "0x.." 문자열 허용).
bool GetU64(const picojson::object& p, const char* key, uint64_t& out)
{
  auto it = p.find(key);
  if (it == p.end())
    return false;
  if (it->second.is<double>())
  {
    out = static_cast<uint64_t>(it->second.get<double>());
    return true;
  }
  if (it->second.is<std::string>())
  {
    const std::string& s = it->second.get<std::string>();
    out = std::strtoull(s.c_str(), nullptr, s.rfind("0x", 0) == 0 ? 16 : 0);
    return true;
  }
  return false;
}

void PushEvent(picojson::value ev)
{
  std::lock_guard<std::mutex> lk(s_ev_mutex);
  s_events.push_back(std::move(ev));
}

// ── 메서드 핸들러 ── (성공 시 result object 반환, 실패 시 GdbError 대신 throw std::string)

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

// CPU 스레드와 경합 없이 PowerPC/메모리에 접근하기 위한 가드. 코어가 안 떠 있으면 실패.
struct SafeAccess
{
  Core::System& system;
  Core::CPUThreadGuard guard;
  explicit SafeAccess(Core::System& sys) : system(sys), guard(sys) {}
};

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
        "step_instructions", "set_breakpoint", "clear_breakpoint", "list_breakpoints",
        "poll_events", "screenshot"})
  {
    methods.push_back(picojson::value(std::string(m)));
  }
  if (gamecube)
    methods.push_back(picojson::value(std::string("set_input")));
  r["methods"] = picojson::value(methods);
  picojson::object limits;
  limits["max_sync_advance_count"] = picojson::value(10000.0);
  r["execution_limits"] = picojson::value(limits);
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
  // exec BP 진단 필드(경량 — CPUThreadGuard 없이 읽는다): 브레이크포인트가 왜 히트/미히트
  // 하는지 런타임 근거로 확인한다. dbg_effective 는 Config::IsDebuggingEnabled()(=
  // MAIN_ENABLE_DEBUGGING && !achievements-hardcore) 로, 코어가 BP 를 체크하려면 true 여야 한다.
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
  return r;
}

picojson::object ReadMemory(Core::System& system, const picojson::object& p)
{
  uint64_t addr = 0, len = 0;
  if (!GetU64(p, "address", addr) || !GetU64(p, "length", len))
    return Fail("address/length required");
  std::vector<uint8_t> buf(static_cast<size_t>(len));
  {
    SafeAccess sa(system);
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
    return Fail("address/hex required");
  std::vector<uint8_t> data;
  if (!FromHex(it->second.get<std::string>(), data))
    return Fail("invalid hex");
  {
    SafeAccess sa(system);
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
    const auto& ppc = system.GetPPCState();
    state["cpu.pc"] = picojson::value(static_cast<double>(ppc.pc));
    for (int i = 0; i < 32; ++i)
      state["cpu.r" + std::to_string(i)] = picojson::value(static_cast<double>(ppc.gpr[i]));
    state["cpu.lr"] = picojson::value(static_cast<double>(ppc.spr[SPR_LR]));
    state["cpu.ctr"] = picojson::value(static_cast<double>(ppc.spr[SPR_CTR]));
    state["cpu.xer"] = picojson::value(static_cast<double>(ppc.spr[SPR_XER]));
    state["cpu.msr"] = picojson::value(static_cast<double>(ppc.msr.Hex));
    state["cpu.cr"] = picojson::value(static_cast<double>(ppc.cr.Get()));
  }
  picojson::object r;
  r["state"] = picojson::value(state);
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
    return Fail("count must be an integer");
  if (count == 0 || count > 10000)
    return Fail("count must be in 1..10000");

  auto& cpu = system.GetCPU();
  cpu.SetStepping(true);
  auto& power_pc = system.GetPowerPC();
  const PowerPC::CoreMode old_mode = power_pc.GetMode();
  power_pc.SetMode(PowerPC::CoreMode::Interpreter);
  bool completed_all = true;
  for (uint64_t i = 0; i < count; ++i)
  {
    Common::Event completed;
    cpu.StepOpcode(&completed);
    if (!completed.WaitFor(std::chrono::seconds(1)))
    {
      completed_all = false;
      break;
    }
  }
  power_pc.SetMode(old_mode);
  if (!completed_all)
    return Fail("instruction step did not complete within 1 second");
  picojson::object r;
  r["status"] = picojson::value(std::string("completed"));
  r["count"] = picojson::value(static_cast<double>(count));
  r["state"] = picojson::value(std::string("frozen"));
  return r;
}

picojson::object SetBreakpoint(Core::System& system, const picojson::object& p)
{
  auto kind_it = p.find("kind");
  if (kind_it != p.end() && kind_it->second.is<std::string>() &&
      kind_it->second.get<std::string>() != "exec")
  {
    return Fail("only exec breakpoints are supported");
  }
  uint64_t addr = 0;
  if (!GetU64(p, "start", addr))
    return Fail("start required");
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
    // 캐시된 블록(JIT·CachedInterpreter)에는 BP 체크가 컴파일돼 있지 않으므로, 전체 캐시를
    // 비워 재컴파일 시 체크가 삽입되게 한다(4바이트 InvalidateICache 만으로는 불충분).
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
  uint64_t slot = 0;
  auto fn = p.find("filename");
  if (fn != p.end() && fn->second.is<std::string>())
    State::SaveAs(system, fn->second.get<std::string>());
  else if (GetU64(p, "slot", slot))
    State::Save(system, static_cast<int>(slot));
  else
    State::Save(system, 1);
  picojson::object r;
  r["status"] = picojson::value(std::string("saved"));
  return r;
}

picojson::object LoadState(Core::System& system, const picojson::object& p)
{
  uint64_t slot = 0;
  auto fn = p.find("filename");
  if (fn != p.end() && fn->second.is<std::string>())
    State::LoadAs(system, fn->second.get<std::string>());
  else if (GetU64(p, "slot", slot))
    State::Load(system, static_cast<int>(slot));
  else
    State::Load(system, 1);
  picojson::object r;
  r["status"] = picojson::value(std::string("loaded"));
  return r;
}

picojson::object SetInput(Core::System&, const picojson::object& p)
{
  int port = 0;
  uint64_t v = 0;
  if (GetU64(p, "port", v) || GetU64(p, "pad", v))
    port = static_cast<int>(v);
  if (port != 0)
    return Fail("only controller port 0 is supported");

  std::lock_guard<std::mutex> lk(s_input_mutex);
  auto engaged = p.find("engaged");
  const auto buttons = p.find("buttons");
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

  GCPadStatus st;  // 중립 기본값
  if (buttons != p.end() && buttons->second.is<picojson::array>())
  {
    u16 bits = 0;
    for (const auto& button : buttons->second.get<picojson::array>())
    {
      if (!button.is<std::string>())
        return Fail("buttons must contain strings");
      u16 bit = 0;
      if (!ButtonBit(button.get<std::string>(), bit))
        return Fail("unsupported GameCube button: " + button.get<std::string>());
      bits |= bit;
    }
    st.button = bits;
  }
  if (GetU64(p, "stickX", v)) st.stickX = static_cast<u8>(v);
  if (GetU64(p, "stickY", v)) st.stickY = static_cast<u8>(v);
  if (GetU64(p, "substickX", v)) st.substickX = static_cast<u8>(v);
  if (GetU64(p, "substickY", v)) st.substickY = static_cast<u8>(v);
  if (GetU64(p, "triggerL", v)) st.triggerLeft = static_cast<u8>(v);
  if (GetU64(p, "triggerR", v)) st.triggerRight = static_cast<u8>(v);
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

  const u64 sequence = s_screenshot_sequence.fetch_add(1);
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
  if (m == "pause") return Pause;
  if (m == "resume") return Resume;
  if (m == "step_instructions") return StepInstructions;
  if (m == "set_breakpoint") return SetBreakpoint;
  if (m == "clear_breakpoint") return ClearBreakpoint;
  if (m == "list_breakpoints") return ListBreakpoints;
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

void NotifyBreakpointHit(u32 address)
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
    PushEvent(picojson::value(event));
  }
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
  // exec 브레이크포인트는 IsDebuggingEnabled() 일 때만 코어가 체크한다(config 의존 제거 —
  // emucap 어댑터가 붙으면 항상 디버깅을 켠다). CachedInterpreter/JIT 모두 이 플래그를 본다.
  Config::SetBaseOrCurrent(Config::MAIN_ENABLE_DEBUGGING, true);
  s_stop.store(false);
  s_thread = std::thread([&system, port] { ThreadMain(system, port); });
}

void Stop()
{
  s_stop.store(true);
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
