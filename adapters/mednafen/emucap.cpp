// emucap live-control socket client for the maintained Mednafen fork. It connects to
// emucap-mcp and serves the same NDJSON protocol as Mesen's emucap-core.lua. The supported
// Mednafen modules are Saturn (ss), PSX (psx), PC Engine (pce), PC-FX (pcfx),
// Mega Drive (md), WonderSwan (wswan), and Neo Geo Pocket/Color (ngp).
//
// 빌드 통합: src/drivers/로 복사 + Makefile.am에 추가 + main.cpp 프레임 루프에서 호출.
#include "main.h"            // CurGame, MDFNGI, Mednafen 타입, MDFNI_Reset
#include <mednafen/debug.h>  // DebuggerInfoStruct, AddressSpaceType
#include <mednafen/movie.h>  // MDFNMOV_IsPlaying(reset-origin capability gate)
#include <mednafen/netplay.h>  // MDFNnetplay(reset is remote-commanded, not immediate)
#include <mednafen/state.h>  // MDFNSS_SaveSM / MDFNSS_LoadSM
#include <mednafen/MemoryStream.h>
#include <mednafen/FileStream.h>
#include <mednafen/video/png.h>  // PNGWrite(screenshot)
#include <mednafen/hash/sha1.h>  // sha1(EMUCAP_CONTENT 보조 해시 — get_rom_info)

#include "emucap.h"
#include "emucap_input.h"
#include "emucap_json_num.h"
#include "emucap_json_strings.h"
#include "emucap_ngp.h"
#include "emucap_pcfx.h"
#include "emucap_recording.h"
#include "emucap_native_failure.h"

extern "C" int emucap_ngp_disasm_safe(unsigned address, unsigned length);

// 빌드 hash(build.sh가 생성; 없으면 unknown 폴백 — LSP·build.sh 밖 직접 컴파일 대비).
#if defined(__has_include)
#if __has_include("emucap_build.h")
#include "emucap_build.h"
#endif
#endif
#ifndef EMUCAP_BUILD_HASH
#define EMUCAP_BUILD_HASH "unknown"
#endif
#ifndef EMUCAP_MEDNAFEN_UPSTREAM
#define EMUCAP_MEDNAFEN_UPSTREAM ""
#endif
#ifndef EMUCAP_MEDNAFEN_UPSTREAM_REVISION
#define EMUCAP_MEDNAFEN_UPSTREAM_REVISION ""
#endif
#ifndef EMUCAP_MEDNAFEN_PATCHSET_SHA256
#define EMUCAP_MEDNAFEN_PATCHSET_SHA256 ""
#endif
#ifndef EMUCAP_MEDNAFEN_MD_REPEATABLE_CONDITIONS_SHA256
#define EMUCAP_MEDNAFEN_MD_REPEATABLE_CONDITIONS_SHA256 ""
#endif

#include <exception>
#include <atomic>
#include <chrono>
#include <stdexcept>
#include <memory>

#ifdef _WIN32
#include <winsock2.h>   // Windows 소켓(MinGW) — POSIX sys/socket.h 대체
#include <ws2tcpip.h>   // inet_pton
#include <windows.h>    // Sleep, GetTempPathA
#include <direct.h>     // _mkdir
#include <process.h>    // getpid/_getpid
#include <io.h>         // unlink/_unlink
#else
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#endif
#include <sys/stat.h>   // mkdir(dump_memory 디렉터리 생성)
#include <unistd.h>     // getpid/unlink/close(파일) — MinGW도 제공
#include <fcntl.h>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <string>

// ── 소켓/OS 이식성 shim (POSIX ↔ Windows/MinGW) ────────────────────────────
// Windows는 소켓이 winsock이라 close→closesocket, fcntl(nonblock)→ioctlsocket,
// errno→WSAGetLastError로 다르고 socket 사용 전 WSAStartup이 필요하다. 소켓 fd는 int로 다뤄도
// MinGW/x64에서 핸들이 작아 유효하다(INVALID_SOCKET→-1로 truncate돼 fd<0 검사도 동작).
#ifdef _WIN32
static inline void emucap_net_init() { static bool d = false; if (!d) { WSADATA w; WSAStartup(MAKEWORD(2, 2), &w); d = true; } }
static inline int  emucap_closesock(int s) { return ::closesocket((SOCKET)s); }
static inline int  emucap_set_nonblock(int s) { u_long m = 1; return ::ioctlsocket((SOCKET)s, FIONBIO, &m); }
static inline bool emucap_sock_wouldblock() { int e = ::WSAGetLastError(); return e == WSAEWOULDBLOCK || e == WSAEINTR; }
static inline int  emucap_mkdir(const char* p) { return ::_mkdir(p); }
static inline std::string emucap_temp_file(const std::string& name) {
  char dir[MAX_PATH]; DWORD n = ::GetTempPathA(MAX_PATH, dir);
  std::string d = (n > 0 && n < MAX_PATH) ? std::string(dir, n) : std::string(".\\");
  return d + name;   // GetTempPathA는 끝에 경로 구분자를 포함한다
}
#else
static inline void emucap_net_init() {}
static inline int  emucap_closesock(int s) { return ::close(s); }
static inline int  emucap_set_nonblock(int s) { return ::fcntl(s, F_SETFL, O_NONBLOCK); }
static inline bool emucap_sock_wouldblock() { return errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR; }
static inline int  emucap_mkdir(const char* p) { return ::mkdir(p, 0755); }
static inline std::string emucap_temp_file(const std::string& name) {
  const char* t = getenv("TMPDIR"); std::string d = (t && *t) ? std::string(t) : std::string("/tmp");
  if (d.empty() || d.back() != '/') d += '/';
  return d + name;
}
#endif
#include <cctype>
#include <strings.h>  // strcasecmp(set_layer_enable 이름 대소문자 무시 매칭)
#include <cstdlib>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

using namespace Mednafen;

// 상류 accessor(ss/vdp2.cpp): 파일-스코프 static RawRegs 섀도를 읽는 전역 accessor.
// RawRegs[a>>1]는 모든 16-bit VDP2 레지스터 쓰기의 무마스크 섀도라 렌더러(vdp2_render.cpp)
// 입력과 비트동일 — get_video_state가 렌더러 비트추출 공식을 복제하면 drift 0이다.
// extern 전방선언으로 ss 코어와 최소 결합(헤더 미포함). 익명 namespace 밖에 둬야 외부 심볼
// (MDFN_IEN_SS::VDP2::PeekRawReg)과 링크된다(익명 ns 안이면 내부 링키지로 떨어져 미해결).
namespace MDFN_IEN_SS { namespace VDP2 { uint16 PeekRawReg(uint32 a); } }

namespace {

const int PROTOCOL_VERSION = 1;

// 브레이크포인트: 콜백(emucap_cpu_cb)을 SetCPUCallback으로 걸면 코어가 DebugMode 경로로 전환
// (DBG_NeedCPUHooks가 CPUHook||BreakPoints로 결정). 히트 시 콜백 안에서 소켓을 스핀 서비스해
// 명령 단위로 정확히 freeze한다. emucap_cpu_cb는 serve_socket_once 뒤에 정의(여기선 전방선언).
void emucap_cpu_cb(uint32 PC, bool bpoint);
void serve_socket_once();
void cancel_recording_for_disconnect();
const char* system_shortname();
bool is_ss();
bool is_md();
bool lookup_button_bit(const std::string& name, uint16_t& bit);
struct BP { long id; int type; uint32 a1, a2; uint32 public_a1, public_a2;
            bool logical = true; bool pause_on_hit = true;
            uint32 value = 0, value_mask = 0xFFFFFFFF; int val_len = 1; bool has_value = false;
            bool adapter_bp = false; std::string memory_type;
            std::vector<EmucapSnapshotSpec> snapshots;
            bool has_pc_filter = false; uint32 pc_min = 0, pc_max = 0xFFFFFFFF; };
struct BPHit {
  uint32 pc = 0;
  bool has_breakpoint_id = false;
  long breakpoint_id = 0;
  bool has_access = false;
  uint32 addr = 0;
  unsigned len = 0;
  bool is_write = false;
  bool has_value = false;
  uint32 value = 0;
  std::string memory_type;
  std::string source;
  std::string snapshot_json;
  std::string snapshot_error;
  bool has_source_addr = false;
  uint32 source_addr = 0;
  std::string registers;  // 히트 순간 CPU 레지스터 {name:value}(exec BP 한정 — pc만으론 D0 등 못 봄)
};
std::vector<BP> g_bps;
long g_bp_next_id = 1;
std::vector<BPHit> g_bp_hits;  // 누적 히트(poll_events가 드레인)
const size_t EVENT_CAP = 4096;
const size_t BREAKPOINT_CAP = 128;
const size_t SNAPSHOT_CAP = 16;
const uint64 SNAPSHOT_BYTE_CAP = 16 * 1024;
uint64_t g_bp_dropped = 0;
// 값-조건 BP: debug.inc의 read/write BP 매칭 시 emucap_bp_record가 접근 주소/길이/유형을 기록하고,
// emucap_cpu_cb가 freeze 전 그 주소의 값을 읽어 BP value/value_mask와 비교한다(불일치면 freeze 스킵).
// debug.inc(코어 GameThread)와 emucap_cpu_cb(같은 GameThread)는 동일 스레드라 atomic 불필요.
uint32 g_bp_hit_addr = 0;
unsigned g_bp_hit_len = 0;
int g_bp_hit_type = BPOINT_PC;
bool g_bp_hit_valid = false;
bool g_bp_hit_has_value = false;
uint32 g_bp_hit_value = 0;

int g_fd = -1;
std::string g_rx;
std::string g_tx;
size_t g_tx_pos = 0;
static const size_t TX_CAP = 8 * 1024 * 1024;
static const long MAX_SYNC_ADVANCE = 5000;
static const uint64_t PROGRESS_INTERVAL_MS = 1000;
uint64_t g_frame = 0;
std::unique_ptr<EmucapRecording> g_recording;
bool g_recording_last_valid = false;
std::string g_recording_last_capture_id;
EmucapRecordingResult g_recording_last;
long g_test_adapter_exception_id = -1;
bool g_internal_failure_active = false;
char g_internal_failure_operation[128]{};
char g_internal_failure_reason[512]{};

// 지연 명령(run_frames): N프레임 진행 후 응답. 진행 중엔 새 명령을 받지 않고 keepalive를 보낸다.
long g_def_id = -1;
long g_def_remaining = 0;
uint64_t g_def_progress_ms = 0;
bool g_def_is_press = false;        // 현재 g_def가 press_buttons면 완료 시 입력 해제

// 입력 주입: non-empty set_input은 다음 set_input까지 유지하고 press_buttons는 완료·중단 때
// 네이티브 입력권을 반환한다. mask와 소유 flag를 한 atomic snapshot으로 읽어 release/apply 경합을 막는다.
EmucapInputOverride g_input_override;
// 게임 스레드 gamepad UpdateInput이 실제로 읽은 입력 버퍼 비트(emucap_game_data_store가 기록).
// status/set_input 응답으로 노출해 "보낸 버튼 → 게임이 실제 받은 비트"를 한눈에 확인한다(매핑 디버깅).
std::atomic<uint16_t> g_last_game_data{0};
// Saturn SMPC 진단: gamepad latch 다음 단계인 OREG/direct-port read를 기록한다.
// 모든 호출은 GameThread에서 이뤄지므로 배열 자체는 락 없이 갱신하고, status도 같은 스레드에서 읽는다.
uint8 g_last_smpc_oreg[0x20] = {0};
std::atomic<uint32_t> g_last_smpc_read_addr{0xFFFFFFFFu};
std::atomic<uint32_t> g_last_smpc_read_value{0};
std::atomic<uint32_t> g_smpc_read_count{0};
std::atomic<uint64_t> g_smpc_read_mask{0};

// 스크린샷: 매 프레임 emucap_capture가 최신 프레임버퍼(espec.surface/DisplayRect/LineWidths)를 기록.
// screenshot 메서드가 이걸 PNG로 인코딩한다(MDFNI_Emulate 직후 훅에서 캡처).
const MDFN_Surface* g_last_surface = nullptr;
MDFN_Rect g_last_rect = MDFN_Rect();
const int32* g_last_lw = nullptr;

// 레이어 enable 마스크 섀도: 코어에 getter가 없어(MDFNI_SetLayerEnableMask는 set 전용) 마지막 적용
// 마스크를 보관해 set_layer_enable 조회에 쓴다. 코어 기본은 ~0(전체 enable). load_state/reset은 이
// 마스크를 *건드리지 않는다*(UserLayerEnableMask는 세이브스테이트 미포함·MDFNI_Reset이 SetLayerEnableMask
// 미호출임을 상류 코드에서 확인). ~0 리셋은 게임 (재)로드 전용(mednafen.cpp:995)이고, 그건 포크 재시작=이 섀도도
// 재초기화다. 따라서 한 세션 안에서 섀도는 코어 실제 마스크와 정확히 일치한다.
uint64_t g_layer_enable_mask = ~0ULL;

// A repeatable MD launch captures the exact post-load, pre-first-instruction guest state once.
// Every eligible recording restores this bounded producer state before reset and movie arming, so
// prior debugging or guest execution in the same process cannot become an undeclared input.
static const uint64_t REPEATABLE_INITIAL_STATE_MAX_BYTES = 16ULL * 1024 * 1024;
std::unique_ptr<MemoryStream> g_repeatable_initial_state;

// freeze 상태머신: frozen이면 emucap_service가 스핀하며 프레임 진행을 막는다(MDFNI_Emulate 차단).
// step(N)은 g_step_remaining만큼 프레임을 진행시킨 뒤 재정지하며 g_step_id로 완료 응답.
bool g_frozen = false;
bool g_launch_start_controlled = false;
long g_step_id = -1;
long g_step_remaining = 0;
uint64_t g_step_progress_ms = 0;

// PSX can replace CPU and device state while the game thread is parked in a debugger callback.
// The patched CPU loop discards that callback's stale local execution state and invokes a fresh
// callback at the restored PC. Keep the request open until that fresh instruction boundary exists.
enum CoreRefreshReply { CORE_REFRESH_NONE, CORE_REFRESH_LOAD_STATE, CORE_REFRESH_RESET };
long g_core_refresh_id = -1;
CoreRefreshReply g_core_refresh_reply = CORE_REFRESH_NONE;

// 명령 단위 step(step_instructions): continuous CPU 콜백을 무장해 g_insn_remaining개 CPU 명령을
// 진행한 뒤 emucap_cpu_cb 안에서 재정지한다(기존 BP freeze 경로 freeze_spin_until_resume 재사용 —
// 새 동기화 없음). g_insn_step_id로 완료 응답. g_insn_armed: 마지막 rearm이 continuous를 무장했는지
// (resume에서만 해제 비용을 치르게 — continuous DebugMode는 매 명령 cb라 도구 비활성 시 즉시 해제).
long g_insn_step_id = -1;
long g_insn_remaining = 0;
uint64_t g_insn_progress_ms = 0;
bool g_insn_armed = false;
// 실행추적(set_trace/get_trace): continuous cb가 매 명령 PC를 원형버퍼에 기록한다 — 크래시 직전 실행
// 경로("어떻게 여기 왔나") 역추적용. step_instructions와 같은 continuous 콜백을 공유한다(arch 독립 — PC만).
bool g_trace_enabled = false;
static const size_t TRACE_CAP = 4096;
std::vector<uint32> g_trace_ring;   // 최근 실행 PC(원형; 크기 TRACE_CAP)
size_t g_trace_head = 0;            // 다음 기록 위치
size_t g_trace_count = 0;           // 채워진 개수(≤ TRACE_CAP)
// 레지스터 워치(watch_register): continuous cb가 매 명령 register 값을 읽어 [min,max]를 벗어나면 그
// 명령에서 freeze한다(SP 폭주 등 derail을 발생 지점에서 포착). 히트 시 1회성 해제(resume 재발화 방지).
bool g_watch_enabled = false;
std::string g_watch_reg;
uint32 g_watch_min = 0, g_watch_max = 0;
bool g_watch_pause = true;
// 콜스택(call_stack): set_trace가 켜진 동안 continuous cb가 call 명령에서 {call-site PC, 그 시점 SP}를
// push하고, *매 명령 SP가 어느 프레임의 call 시점 SP 이상으로 올라가면 그 프레임을 pop*한다 — RTS뿐 아니라
// JMP-return·JSR (An) 간접·RTE·수동 스택조작 등 *모든 반환*을 SP로 감지해 루프 중복누적 폴루션을 없앤다.
// "어떻게 여기 왔나"를 스택 메모리 손상과 독립적으로 답한다. set_trace(true) 선행 필요. ISA는 classify_instr.
struct CSFrame {
  uint32 pc;
  uint32 sp;
  // 콜리가 프레임을 "확립"했나(콜 시점 sp 아래로 내려간 적 — prologue 스택 예약). register-linkage
  // (MIPS JAL·SH BSR/JSR는 반환주소를 레지스터 PR/$ra에 둬 콜이 sp를 안 바꿈)에선 push 직후 sp==call-시점
  // sp라, 확립 전엔 pop 안 해 즉시-pop 버그를 막는다. stack-linkage(68000/HuC6280은 콜이 반환주소를 push해
  // sp 감소)는 다음 명령서 곧장 확립되어 기존 동작과 동일(회귀 없음).
  // NB: default member initializer 안 씀(C++11선 aggregate를 깨 {pc,sp,..} 초기화 불가) — push서 명시 초기화.
  bool established;
};
std::vector<CSFrame> g_callstack;
static const size_t CALLSTACK_CAP = 256;
std::string g_sp_reg_name;  // 캐시된 SP 레지스터 이름(set_trace 켤 때 해소; 매 명령 스캔 회피)
// break_on_reset(카트리지 전용 — MD/PCE/WS): 게임이 리셋 벡터를 재실행하면(워치독 리셋·크래시→리셋) freeze한다.
// 디스크(SS/PSX)는 "리셋"이 BIOS 부팅이라 개념이 안 맞아 미advertise(exec BP를 BIOS 엔트리에 거는 대체 사용).
bool g_break_on_reset = false;
uint32 g_reset_entry = 0;  // 리셋 진입 PC(enable 시 벡터에서 읽음)
// cb는 명령 *실행 직전*(pre-execution) 발화한다. BP 히트/명령단위 완료로 *콜백 안에서* frozen이면
// 진입명령의 cb가 이미 일어난 상태라 resume이 그 명령을 '공짜로' 실행시킨다 → step(N)=정확히 N.
// 그러나 pause/프레임-step로 *cold* frozen이면 진입명령 cb가 아직 안 일어났다 → 첫 continuous cb를
// 1회 흡수(g_insn_skip_first)해야 cold도 공짜 진입명령을 갖고 정확히 N이 된다(안 그러면 N-1).
// g_frozen_via_cb: "현재 진입명령의 cb가 이미 발화했나(=resume이 그 명령을 실행하나)". 일반적으로
// park 위치가 정하지만 PSX load/reset은 예외다. 그 코어는 복원 전 callback의 로컬 timestamp와 pipeline을
// 폐기한 뒤 복원된 PC에서 새 callback을 발화하므로, load/reset 응답도 그 새 boundary에서 완료한다.
bool g_frozen_via_cb = false;
bool g_insn_skip_first = false;

// 원자적 probe: 세이브스테이트 복귀 → N프레임 진행 → 타깃 읽기를 한 단위로(그 사이 새 명령 차단).
// 별도 load+run_frames 호출 사이의 자유 실행 누수를 없애 bisect를 결정론적으로 만든다.
long g_probe_id = -1;
long g_probe_requested = 0;
long g_probe_remaining = 0;
uint64_t g_probe_progress_ms = 0;
std::string g_probe_mt;
uint32 g_probe_addr = 0;
long g_probe_len = 0;

int emucap_port() {
  const char* p = getenv("EMUCAP_PORT");
  int port = p ? atoi(p) : 47800;
  return (port > 0 && port < 65536) ? port : 47800;
}

bool start_frozen_requested() {
  const char* value = getenv("EMUCAP_START_FROZEN");
  return value && !strcmp(value, "1");
}

uint64_t monotonic_millis() {
  return (uint64_t)std::chrono::duration_cast<std::chrono::milliseconds>(
      std::chrono::steady_clock::now().time_since_epoch()).count();
}

void rearm_breakpoints();

void cancel_session_requests() {
  const bool had_request = g_def_id >= 0 || g_probe_id >= 0 || g_step_id >= 0
      || g_insn_step_id >= 0 || g_core_refresh_id >= 0
      || g_test_adapter_exception_id >= 0 || g_recording.get();
  cancel_recording_for_disconnect();
  if (g_def_is_press) g_input_override.release();
  g_def_id = -1;
  g_def_remaining = 0;
  g_def_progress_ms = 0;
  g_def_is_press = false;
  g_probe_id = -1;
  g_probe_requested = 0;
  g_probe_remaining = 0;
  g_probe_progress_ms = 0;
  g_probe_mt.clear();
  g_probe_addr = 0;
  g_probe_len = 0;
  g_step_id = -1;
  g_step_remaining = 0;
  g_step_progress_ms = 0;
  g_insn_step_id = -1;
  g_insn_remaining = 0;
  g_insn_progress_ms = 0;
  g_insn_skip_first = false;
  g_core_refresh_id = -1;
  g_core_refresh_reply = CORE_REFRESH_NONE;
  g_test_adapter_exception_id = -1;
  if (g_insn_armed) rearm_breakpoints();
  if (had_request)
    fprintf(stderr, "emucap: connection ended; cancelled session-scoped request\n");
}

void emucap_disconnect() {
  // Never put an old response id ahead of the replacement session's hello. Persistent emulator,
  // breakpoint, explicit set_input, and freeze state survive; only the dead session's work is lost.
  cancel_session_requests();
  if (g_fd >= 0) emucap_closesock(g_fd);
  g_fd = -1;
  g_rx.clear();
  g_tx.clear();
  g_tx_pos = 0;
}

enum TxFlush { TX_IDLE, TX_COMPLETE, TX_PENDING, TX_ERROR };
TxFlush flush_tx_once() {
  if (g_fd < 0 || g_tx.empty()) return TX_IDLE;
#ifdef MSG_NOSIGNAL
  const int send_flags = MSG_NOSIGNAL;
#else
  const int send_flags = 0;
#endif
  ssize_t n = ::send(g_fd, g_tx.data() + g_tx_pos, g_tx.size() - g_tx_pos, send_flags);
  if (n > 0) {
    g_tx_pos += (size_t)n;
    if (g_tx_pos >= g_tx.size()) {
      g_tx.clear();
      g_tx_pos = 0;
      return TX_COMPLETE;
    }
    return TX_PENDING;
  }
  if (n < 0 && emucap_sock_wouldblock()) return TX_PENDING;
  emucap_disconnect();
  return TX_ERROR;
}

// emucap-mcp(서버)에 접속. localhost 블로킹 connect는 즉시 성공/거부된다.
void emucap_connect() {
  emucap_net_init();
  int fd = socket(AF_INET, SOCK_STREAM, 0);
  if (fd < 0) return;
#ifdef SO_NOSIGPIPE
  {
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
  }
#endif
  struct sockaddr_in addr;
  memset(&addr, 0, sizeof(addr));
  addr.sin_family = AF_INET;
  addr.sin_port = htons(emucap_port());
  inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
  if (connect(fd, (struct sockaddr*)&addr, sizeof(addr)) != 0) {
    emucap_closesock(fd);
    return;
  }
  emucap_set_nonblock(fd);  // recv는 논블로킹
  g_fd = fd;
  g_rx.clear();
  g_tx.clear();
  g_tx_pos = 0;
}

void send_line(const std::string& s) {
  if (g_fd < 0) return;
  const size_t line_size = s.size() + 1;
  const size_t remaining = g_tx.empty() ? 0 : g_tx.size() - g_tx_pos;
  if (s.size() >= TX_CAP || remaining > TX_CAP - line_size) {
    fprintf(stderr, "emucap: TX too large; dropping connection\n");
    emucap_disconnect();
    return;
  }
  if (!g_tx.empty() && g_tx_pos > 0) {
    g_tx.erase(0, g_tx_pos);
    g_tx_pos = 0;
  }
  g_tx.append(s);
  g_tx.push_back('\n');
  flush_tx_once();
}

// ── 최소 JSON 추출(첫 컷). 정식 파서는 후속(Lua의 json_decode에 대응). ──
std::string json_str(const std::string& s, const char* key) {
  std::string pat = std::string("\"") + key + "\"";
  size_t k = s.find(pat);
  if (k == std::string::npos) return "";
  size_t c = s.find(':', k + pat.size());
  if (c == std::string::npos) return "";
  size_t q1 = s.find('"', c + 1);
  if (q1 == std::string::npos) return "";
  size_t q2 = s.find('"', q1 + 1);
  if (q2 == std::string::npos) return "";
  return s.substr(q1 + 1, q2 - q1 - 1);
}

bool json_num(const std::string& s, const char* key, long& out) {
  std::string pat = std::string("\"") + key + "\"";
  size_t k = s.find(pat);
  if (k == std::string::npos) return false;
  size_t c = s.find(':', k + pat.size());
  if (c == std::string::npos) return false;
  out = strtol(s.c_str() + c + 1, nullptr, 10);
  return true;
}

bool json_bool(const std::string& s, const char* key, bool& out) {
  std::string pat = std::string("\"") + key + "\"";
  size_t k = s.find(pat);
  if (k == std::string::npos) return false;
  size_t c = s.find(':', k + pat.size());
  if (c == std::string::npos) return false;
  const char* p = s.c_str() + c + 1;
  while (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n') p++;
  if (!strncmp(p, "true", 4) || *p == '1') { out = true; return true; }
  if (!strncmp(p, "false", 5) || *p == '0') { out = false; return true; }
  return false;
}

// JSON 문자열 값 이스케이프(따옴표·역슬래시·제어문자). EMUCAP_NAME 등 외부 입력을 JSON에
// 그대로 박으면 깨진 JSON이 될 수 있어 안전하게 변환한다.
std::string json_escape(const std::string& s) {
  std::string o;
  o.reserve(s.size() + 8);
  for (unsigned char c : s) {
    switch (c) {
      case '"': o += "\\\""; break;
      case '\\': o += "\\\\"; break;
      case '\b': o += "\\b"; break;
      case '\f': o += "\\f"; break;
      case '\n': o += "\\n"; break;
      case '\r': o += "\\r"; break;
      case '\t': o += "\\t"; break;
      default:
        if (c < 0x20) { char b[8]; snprintf(b, sizeof(b), "\\u%04x", c); o += b; }
        else o += (char)c;
    }
  }
  return o;
}

void reply_ok(long id, const std::string& result_json) {
  char head[48];
  snprintf(head, sizeof(head), "{\"id\":%ld,\"ok\":true,\"result\":", id);
  send_line(std::string(head) + result_json + "}");
}

// JSON 문자열 값 이스케이프. 예외 메시지(파일 경로 따옴표 등)가 응답 JSON을 깨지 않게 한다.
std::string json_escape(const char* s) {
  std::string out;
  for (const char* p = s; *p; p++) {
    unsigned char c = (unsigned char)*p;
    if (c == '"' || c == '\\') { out += '\\'; out += (char)c; }
    else if (c == '\n') out += "\\n";
    else if (c == '\r') out += "\\r";
    else if (c == '\t') out += "\\t";
    else if (c < 0x20) { char b[8]; snprintf(b, sizeof(b), "\\u%04x", c); out += b; }
    else out += (char)c;
  }
  return out;
}

void reply_err(long id, const char* kind, const char* msg) {
  char head[96];
  snprintf(head, sizeof(head), "{\"id\":%ld,\"ok\":false,\"error\":{\"kind\":\"%s\",\"message\":\"", id, kind);
  send_line(std::string(head) + json_escape(msg) + "\"}}");  // msg는 예외 등 비신뢰 → 이스케이프
}

bool recording_hex_digest(const std::string& value) {
  if (value.size() != 64) return false;
  for (const unsigned char c : value) if (!isxdigit(c)) return false;
  return true;
}

bool recording_safe_id(const std::string& value) {
  if (value.empty() || value.size() > 96) return false;
  for (const unsigned char c : value) {
    if (!(isalnum(c) || c == '-' || c == '_')) return false;
  }
  return true;
}

bool recording_identity_available() {
  const char* binary = getenv("EMUCAP_MEDNAFEN_BINARY_SHA256");
  const char* launch_id = getenv("EMUCAP_LAUNCH_ID");
  return binary && recording_hex_digest(binary)
      && launch_id && *launch_id
      && EMUCAP_MEDNAFEN_UPSTREAM[0]
      && EMUCAP_MEDNAFEN_UPSTREAM_REVISION[0]
      && recording_hex_digest(EMUCAP_MEDNAFEN_PATCHSET_SHA256);
}

bool recording_reset_release_available() {
  // MDFNI_Reset is immediate for the maintained modules except regular Saturn, whose core ignores
  // MDFN_MSC_RESET in favor of the SMPC reset-button path. Movie playback can also suppress the
  // command, so neither case may advertise an exact post-reset origin.
  return !is_ss() && !MDFNnetplay && !MDFNMOV_IsPlaying();
}

bool repeatable_profile_requested() {
  const char* execution = getenv("EMUCAP_EXECUTION_PROFILE");
  const char* profile = getenv("EMUCAP_REPEATABLE_PROFILE_ID");
  const char* conditions = getenv("EMUCAP_REPEATABLE_CONDITIONS_SHA256");
  return start_frozen_requested() && is_md()
      && execution && !strcmp(execution, "repeatable")
      && profile && !strcmp(profile, EMUCAP_RECORDING_MD_REPEATABLE_PROFILE)
      && conditions
      && !strcmp(conditions, EMUCAP_MEDNAFEN_MD_REPEATABLE_CONDITIONS_SHA256);
}

const char* recording_repeatable_conditions() {
  if (!g_launch_start_controlled || !recording_reset_release_available()
      || !repeatable_profile_requested() || !g_repeatable_initial_state)
    return nullptr;
  return EMUCAP_MEDNAFEN_MD_REPEATABLE_CONDITIONS_SHA256;
}

bool capture_repeatable_initial_state(std::string& error) {
  if (!repeatable_profile_requested()) return false;
  try {
    std::unique_ptr<MemoryStream> state(new MemoryStream(1024 * 1024));
    MDFNSS_SaveSM(state.get(), false);
    if (state->size() == 0 || state->size() > REPEATABLE_INITIAL_STATE_MAX_BYTES) {
      error = "initial guest state exceeds the repeatable profile bound";
      return false;
    }
    state->rewind();
    g_repeatable_initial_state = std::move(state);
    return true;
  } catch (const std::exception& exception) {
    error = exception.what();
    return false;
  }
}

bool restore_repeatable_initial_state(std::string& error) {
  if (!g_repeatable_initial_state) {
    error = "repeatable initial guest state is unavailable";
    return false;
  }
  try {
    g_repeatable_initial_state->rewind();
    MDFNSS_LoadSM(g_repeatable_initial_state.get(), false);
    g_repeatable_initial_state->rewind();
    MDFNI_SetLayerEnableMask(~0ULL);
    g_layer_enable_mask = ~0ULL;
    return true;
  } catch (const std::exception& exception) {
    error = exception.what();
    g_repeatable_initial_state.reset();
    return false;
  }
}

enum RecordingJsonObjectState {
  RECORDING_JSON_OBJECT_ABSENT,
  RECORDING_JSON_OBJECT_VALID,
  RECORDING_JSON_OBJECT_INVALID,
};

RecordingJsonObjectState recording_json_object(
    const std::string& line,
    const char* key,
    std::string& object) {
  const std::string marker = std::string("\"") + key + "\"";
  const std::size_t key_pos = line.find(marker);
  if (key_pos == std::string::npos) return RECORDING_JSON_OBJECT_ABSENT;
  if (line.find(marker, key_pos + marker.size()) != std::string::npos)
    return RECORDING_JSON_OBJECT_INVALID;
  const std::size_t colon = line.find(':', key_pos + marker.size());
  if (colon == std::string::npos) return RECORDING_JSON_OBJECT_INVALID;
  std::size_t begin = colon + 1;
  while (begin < line.size() && std::isspace(static_cast<unsigned char>(line[begin]))) begin++;
  if (begin >= line.size() || line[begin] != '{') return RECORDING_JSON_OBJECT_INVALID;

  bool quoted = false;
  bool escaped = false;
  unsigned depth = 0;
  for (std::size_t pos = begin; pos < line.size(); pos++) {
    const char value = line[pos];
    if (quoted) {
      if (escaped) escaped = false;
      else if (value == '\\') escaped = true;
      else if (value == '"') quoted = false;
      continue;
    }
    if (value == '"') quoted = true;
    else if (value == '{') depth++;
    else if (value == '}') {
      if (depth == 0) return RECORDING_JSON_OBJECT_INVALID;
      if (--depth == 0) {
        object = line.substr(begin, pos - begin + 1);
        return RECORDING_JSON_OBJECT_VALID;
      }
    }
  }
  return RECORDING_JSON_OBJECT_INVALID;
}

bool recording_json_string(
    const std::string& object,
    const char* key,
    std::string& value) {
  const std::string marker = std::string("\"") + key + "\"";
  const std::size_t key_pos = object.find(marker);
  if (key_pos == std::string::npos ||
      object.find(marker, key_pos + marker.size()) != std::string::npos) return false;
  const std::size_t colon = object.find(':', key_pos + marker.size());
  if (colon == std::string::npos) return false;
  std::size_t pos = colon + 1;
  while (pos < object.size() && std::isspace(static_cast<unsigned char>(object[pos]))) pos++;
  if (pos >= object.size() || object[pos++] != '"') return false;
  value.clear();
  while (pos < object.size()) {
    const unsigned char current = static_cast<unsigned char>(object[pos++]);
    if (current == '"') return true;
    if (current < 0x20) return false;
    if (current != '\\') {
      value.push_back(static_cast<char>(current));
      continue;
    }
    if (pos >= object.size()) return false;
    const char escaped = object[pos++];
    switch (escaped) {
      case '"': value.push_back('"'); break;
      case '\\': value.push_back('\\'); break;
      case '/': value.push_back('/'); break;
      case 'b': value.push_back('\b'); break;
      case 'f': value.push_back('\f'); break;
      case 'n': value.push_back('\n'); break;
      case 'r': value.push_back('\r'); break;
      case 't': value.push_back('\t'); break;
      default: return false;
    }
  }
  return false;
}

bool recording_input_movie(
    const std::string& line,
    std::uint64_t frames,
    std::vector<std::uint16_t>& masks,
    bool& present,
    std::string& error) {
  present = false;
  std::string object;
  const RecordingJsonObjectState state =
      recording_json_object(line, "input_movie", object);
  if (state == RECORDING_JSON_OBJECT_ABSENT) return true;
  if (state != RECORDING_JSON_OBJECT_VALID) {
    error = "input_movie must be an object";
    return false;
  }

  std::uint64_t port = 0;
  std::uint64_t movie_frames = 0;
  std::uint64_t expected_bytes = 0;
  const std::string format = json_str(object, "format");
  std::string path;
  const std::string sha256 = json_str(object, "sha256");
  if (!recording_json_string(object, "path", path)
      || format != EMUCAP_RECORDING_INPUT_MOVIE_FORMAT || path.empty()
      || !recording_hex_digest(sha256)
      || emucap_json_u64(object, "port", port) != EmucapJsonNumberStatus::valid
      || emucap_json_u64(object, "frames", movie_frames) != EmucapJsonNumberStatus::valid
      || emucap_json_u64(object, "bytes", expected_bytes) != EmucapJsonNumberStatus::valid
      || port != 0 || movie_frames != frames || expected_bytes < 1
      || expected_bytes > 1024 * 1024) {
    error = "input_movie identity is invalid";
    return false;
  }

  std::ifstream input(path.c_str(), std::ios::in | std::ios::binary);
  if (!input) {
    error = "input_movie open failed";
    return false;
  }
  const std::string text(
      (std::istreambuf_iterator<char>(input)), std::istreambuf_iterator<char>());
  if (input.bad() || text.size() != expected_bytes || text.empty() || text.back() != '\n') {
    error = "input_movie size or terminal newline mismatch";
    return false;
  }

  if (!emucap_parse_recording_input_movie(
          text, frames, lookup_button_bit, masks, error)) {
    if (error.find("unsupported button") != std::string::npos) {
      error = std::string("unsupported ") + system_shortname() + " " + error;
    }
    return false;
  }
  present = true;
  return true;
}

bool recording_stop_condition(
    const std::string& line,
    bool include_frame_completed,
    std::uint64_t frames,
    EmucapRecordingRequest& request,
    std::string& error) {
  std::string object;
  const RecordingJsonObjectState state = recording_json_object(line, "stop_on", object);
  if (state == RECORDING_JSON_OBJECT_ABSENT) return true;
  if (state != RECORDING_JSON_OBJECT_VALID
      || json_str(object, "event_class") != "frame_completed"
      || !include_frame_completed
      || emucap_json_u64(object, "occurrence", request.stop_occurrence)
          != EmucapJsonNumberStatus::valid
      || request.stop_occurrence < 1 || request.stop_occurrence > frames) {
    error = "stop_on must select an advertised frame_completed occurrence";
    return false;
  }
  request.stop_on_frame_completed = true;
  return true;
}

bool recording_input_handler(bool engaged, std::uint16_t mask, std::string& error) {
  (void)error;
  if (engaged) g_input_override.engage(mask);
  else g_input_override.release();
  return true;
}

std::string recording_stop_event_json(const EmucapRecordingResult& result) {
  std::string stop_event = "null";
  if (result.has_stop_event) {
    stop_event = "{\"sequence\":" + std::to_string(result.stop_sequence) +
        ",\"event_class\":\"frame_completed\",\"clock_domain\":\"frame\"," +
        "\"clock_tick\":" + std::to_string(result.stop_clock_tick) +
        ",\"frame\":" + std::to_string(result.stop_frame) +
        ",\"occurrence\":" + std::to_string(result.stop_occurrence) + "}";
  }
  return stop_event;
}

std::string recording_result_json(
    const std::string& capture_id,
    const EmucapRecordingResult& result) {
  std::string reason = result.reason.empty()
      ? "null"
      : "\"" + json_escape(result.reason) + "\"";
  char numbers[640];
  snprintf(
      numbers, sizeof(numbers),
      ",\"f_start\":%llu,\"f_end\":%llu,\"final_frame\":%llu,"
      "\"frames\":%llu,\"events\":%llu,\"bytes\":%llu,"
      "\"physical_bytes\":%llu,\"dropped\":%llu,\"truncated\":%s,"
      "\"first_sequence_gap\":null,\"wall_ms\":%llu,",
      (unsigned long long)result.f_start,
      (unsigned long long)result.f_end,
      (unsigned long long)result.final_frame,
      (unsigned long long)result.frames,
      (unsigned long long)result.events,
      (unsigned long long)result.bytes,
      (unsigned long long)result.physical_bytes,
      (unsigned long long)result.dropped,
      result.truncated ? "true" : "false",
      (unsigned long long)result.wall_ms);
  return "{\"status\":\"" + result.status + "\",\"capture_id\":\"" +
      json_escape(capture_id) + "\",\"operation_outcome\":\"" +
      result.operation_outcome + "\",\"execution_outcome\":\"" +
      result.execution_outcome + "\",\"integrity\":\"" + result.integrity +
      "\",\"reason\":" + reason + numbers + "\"stop_event\":" +
      recording_stop_event_json(result) +
      ",\"final_execution_state\":\"" +
      result.final_execution_state + "\",\"cleanup\":{\"hooks\":\"not_acquired\","
      "\"transient_input\":\"" +
      (result.input_acquired
           ? (result.input_released ? "released" : "unverifiable")
           : "not_acquired") +
      "\",\"sink\":\"" +
      (result.sink_released ? "released" : "unverifiable") + "\"}}";
}

std::string recording_last_json() {
  if (!g_recording_last_valid) return "null";
  const EmucapRecordingResult& result = g_recording_last;
  std::string reason = result.reason.empty()
      ? "null"
      : "\"" + json_escape(result.reason) + "\"";
  char numbers[384];
  snprintf(
      numbers, sizeof(numbers),
      "\",\"final_frame\":%llu,\"counters\":{\"frames\":%llu,"
      "\"events\":%llu,\"bytes\":%llu,\"dropped\":%llu},",
      (unsigned long long)result.final_frame,
      (unsigned long long)result.frames,
      (unsigned long long)result.events,
      (unsigned long long)result.bytes,
      (unsigned long long)result.dropped);
  return "{\"capture_id\":\"" + json_escape(g_recording_last_capture_id) +
      "\",\"operation_outcome\":\"" + result.operation_outcome +
      "\",\"execution_outcome\":\"" + result.execution_outcome +
      "\",\"integrity\":\"" + result.integrity +
      "\",\"final_execution_state\":\"" +
      result.final_execution_state + numbers +
      "\"cleanup\":{\"hooks\":\"not_acquired\",\"transient_input\":\"" +
      (result.input_acquired
           ? (result.input_released ? "released" : "unverifiable")
           : "not_acquired") +
      "\",\"sink\":\"" +
      (result.sink_released ? "released" : "unverifiable") +
      "\"},\"stop_event\":" + recording_stop_event_json(result) +
      ",\"reason\":" + reason + "}";
}

void remember_recording_result(
    const std::string& capture_id,
    const EmucapRecordingResult& result) {
  g_recording_last_capture_id = capture_id;
  g_recording_last = result;
  g_recording_last_valid = true;
}

void finish_recording(bool reply) {
  if (!g_recording) return;
  const long request_id = g_recording->request_id();
  const std::string capture_id = g_recording->capture_id();
  const EmucapRecordingResult result = g_recording->result(monotonic_millis());
  remember_recording_result(capture_id, result);
  if (result.final_execution_state == "frozen") g_frozen = true;
  g_recording.reset();
  if (reply) reply_ok(request_id, recording_result_json(capture_id, result));
}

void cancel_recording_for_disconnect() {
  if (!g_recording) return;
  g_recording->cancel(
      g_frame, monotonic_millis(), "connection_closed", "frozen");
  finish_recording(false);
}

bool recording_conflict() {
  return g_recording.get() || g_def_id >= 0 || g_probe_id >= 0 || g_step_id >= 0
      || g_insn_step_id >= 0 || g_core_refresh_id >= 0
      || g_input_override.engaged() || !g_bps.empty()
      || g_trace_enabled || g_watch_enabled || g_break_on_reset;
}

bool recording_u64(
    long id,
    const std::string& line,
    const char* key,
    std::uint64_t& value) {
  if (emucap_json_u64(line, key, value) == EmucapJsonNumberStatus::valid) return true;
  const std::string message = std::string(key) + " must be an unsigned integer";
  reply_err(id, "bad_params", message.c_str());
  return false;
}

void handle_record_window(long id, const std::string& line) {
  if (!recording_identity_available()) {
    reply_err(id, "unsupported", "record_window requires an identity-complete maintained build");
    return;
  }
  if (recording_conflict()) {
    reply_err(id, "busy", "a temporal operation, input override, or persistent debug hook is active");
    return;
  }
  if (g_frozen && g_frozen_via_cb) {
    reply_err(
        id, "unsafe_halt",
        "record_window requires a proven frame boundary; resume from the instruction-bound halt first");
    return;
  }
  EmucapRecordingRequest request;
  request.request_id = id;
  request.capture_id = json_str(line, "capture_id");
  request.launch_id = json_str(line, "launch_id");
  request.request_digest_sha256 = json_str(line, "request_digest_sha256");
  const std::string capability_revision = json_str(line, "capability_revision");
  const std::string origin = json_str(line, "origin");
  const bool reset_available = recording_reset_release_available();
  const char* repeatability_conditions = recording_repeatable_conditions();
  const bool repeatable = repeatability_conditions != nullptr;
  request.reset_release = origin == "reset_release";
  const char* expected_launch_id = getenv("EMUCAP_LAUNCH_ID");
  bool include_frame_completed = false;
  if (!recording_safe_id(request.capture_id)
      || !expected_launch_id || request.launch_id != expected_launch_id
      || !recording_hex_digest(request.request_digest_sha256)
      || capability_revision != emucap_recording_capability_revision(
          reset_available, repeatable)
      || (origin != "next_frame_boundary" && !request.reset_release)
      || (request.reset_release && !reset_available)
      || !emucap_recording_exact_event_classes(line, include_frame_completed)) {
    reply_err(id, "bad_params", "recording identity, origin, or event contract is invalid");
    return;
  }
  request.include_frame_completed = include_frame_completed;
  if (!recording_u64(id, line, "frames", request.frames)
      || !recording_u64(id, line, "max_frames", request.limits.max_frames)
      || !recording_u64(id, line, "max_events", request.limits.max_events)
      || !recording_u64(id, line, "max_bytes", request.limits.max_bytes)
      || !recording_u64(id, line, "max_line_bytes", request.limits.max_line_bytes)
      || !recording_u64(id, line, "max_host_ms", request.limits.max_host_ms)
      || !recording_u64(
          id, line, "progress_interval_ms", request.limits.progress_interval_ms)) return;
  if (request.frames < 1 || request.frames > 300
      || request.limits.max_frames != request.frames
      || request.limits.max_events < request.frames * (include_frame_completed ? 2 : 1)
      || request.limits.max_events > 100000
      || request.limits.max_bytes < request.limits.max_line_bytes
      || request.limits.max_bytes > 64ULL * 1024 * 1024
      || request.limits.max_line_bytes < 1
      || request.limits.max_line_bytes > 64ULL * 1024
      || request.limits.max_host_ms < 1 || request.limits.max_host_ms > 30000
      || request.limits.progress_interval_ms < 10
      || request.limits.progress_interval_ms >= request.limits.max_host_ms
      || g_frame > std::numeric_limits<std::uint64_t>::max() - request.frames) {
    reply_err(id, "bad_params", "recording frames or limits are outside the advertised bounds");
    return;
  }
  std::string request_error;
  bool input_movie_present = false;
  if (!recording_stop_condition(
          line, include_frame_completed, request.frames, request, request_error)
      || !recording_input_movie(
          line, request.frames, request.input_masks, input_movie_present, request_error)) {
    reply_err(id, "bad_params", request_error.c_str());
    return;
  }
  if (repeatable && (!request.reset_release || !input_movie_present)) {
    reply_err(
        id, "bad_params",
        "the repeatable Mega Drive profile requires reset_release and an explicit input movie");
    return;
  }
  const std::string endpoint = json_str(line, "endpoint");
  const std::string token = json_str(line, "token");
  std::string sink_error;
  std::unique_ptr<EmucapRecordingSink> sink = emucap_open_recording_sink(
      endpoint, token, request.capture_id, sink_error);
  if (!sink) {
    reply_err(id, "sink_unavailable", sink_error.c_str());
    return;
  }
  if (repeatable && !restore_repeatable_initial_state(request_error)) {
    g_frozen = true;
    reply_err(id, "initial_state_unavailable", request_error.c_str());
    return;
  }
  const std::uint64_t started_ms = monotonic_millis();
  if (request.reset_release) MDFNI_Reset();
  g_recording.reset(new EmucapRecording(
      request, g_frame, started_ms, std::move(sink), recording_input_handler));
  const EmucapRecordingEffect initial = g_recording->tick(g_frame, started_ms);
  if (initial == EmucapRecordingEffect::terminal) {
    finish_recording(true);
    return;
  }
  g_frozen = false;
  reply_ok(id, g_recording->progress_json());
}

void handle_abort_recording(long id, const std::string& line) {
  const std::string capture_id = json_str(line, "capture_id");
  const std::string launch_id = json_str(line, "launch_id");
  if (!g_recording || capture_id != g_recording->capture_id()
      || launch_id != g_recording->launch_id()) {
    reply_err(id, "bad_state", "abort identity does not match the active recording");
    return;
  }
  g_recording->cancel(g_frame, monotonic_millis(), "request_cancelled", "frozen");
  reply_ok(
      id, "{\"status\":\"completed\",\"capture_id\":\"" +
          json_escape(capture_id) + "\",\"aborted\":true}");
  finish_recording(true);
}

bool service_recording() {
  if (!g_recording) return false;
  if (!g_tx.empty() && flush_tx_once() == TX_ERROR) return false;
  if (!g_recording) return false;
  const EmucapRecordingEffect effect = g_recording->tick(g_frame, monotonic_millis());
  if (effect == EmucapRecordingEffect::working && g_tx.empty())
    reply_ok(g_recording->request_id(), g_recording->progress_json());
  if (effect == EmucapRecordingEffect::terminal) {
    finish_recording(true);
    return false;
  }
  serve_socket_once();
  return g_recording.get() != nullptr;
}

bool json_u32_arg(
    long id,
    const std::string& line,
    const char* key,
    uint32& out,
    bool required,
    bool* present = nullptr) {
  std::uint32_t parsed = 0;
  const EmucapJsonNumberStatus status = emucap_json_u32(line, key, parsed);
  if (present) *present = status != EmucapJsonNumberStatus::absent;
  if (status == EmucapJsonNumberStatus::absent && !required) return true;
  if (status != EmucapJsonNumberStatus::valid) {
    const std::string message =
        std::string(key) + (status == EmucapJsonNumberStatus::absent
            ? " is required"
            : " must be an unsigned 32-bit integer");
    reply_err(id, "bad_params", message.c_str());
    return false;
  }
  out = static_cast<uint32>(parsed);
  return true;
}

bool publish_native_failure(
    const char* operation,
    const char* reason,
    bool active,
    const char* execution_state) noexcept {
  try {
    std::string error;
    const bool written = emucap_publish_native_failure(
        "mednafen-native",
        EMUCAP_BUILD_HASH,
        g_frame,
        operation,
        reason,
        active,
        execution_state,
        &error);
    if (!written)
      fprintf(stderr, "emucap: cannot persist native adapter failure: %s\n", error.c_str());
    return written;
  } catch (const std::exception& error) {
    fprintf(stderr, "emucap: native adapter failure serialization failed: %s\n", error.what());
  } catch (...) {
    fprintf(stderr, "emucap: native adapter failure serialization failed\n");
  }
  return false;
}

void remember_active_native_failure(const char* operation, const char* reason) noexcept {
  g_internal_failure_active = true;
  snprintf(
      g_internal_failure_operation,
      sizeof(g_internal_failure_operation),
      "%s",
      operation != nullptr ? operation : "unknown");
  snprintf(
      g_internal_failure_reason,
      sizeof(g_internal_failure_reason),
      "%s",
      reason != nullptr ? reason : "unknown native adapter exception");
  (void)publish_native_failure(
      g_internal_failure_operation, g_internal_failure_reason, true, "unknown");
}

void recover_native_failure_after_status() noexcept {
  if (!g_internal_failure_active) return;
  const char* state = g_frozen ? "frozen" : "running";
  if (publish_native_failure(
          g_internal_failure_operation, g_internal_failure_reason, false, state)) {
    g_internal_failure_active = false;
  }
}

void flush_pending_internal_error(long id, const char* reason) noexcept {
  if (id < 0 || g_fd < 0) return;
  try {
    reply_err(id, "internal_error", reason);
    for (int guard = 0; guard < 16 && g_fd >= 0 && !g_tx.empty(); guard++) {
      const TxFlush status = flush_tx_once();
      if (status == TX_COMPLETE || status == TX_IDLE || status == TX_ERROR) break;
      usleep(1000);
    }
  } catch (...) {
  }
}

void contain_service_exception(const char* operation, const char* reason) noexcept {
  const long pending_ids[] = {
      g_def_id, g_probe_id, g_step_id, g_insn_step_id, g_core_refresh_id,
      g_test_adapter_exception_id};
  g_frozen = true;
  g_frozen_via_cb = false;
  for (size_t i = 0; i < sizeof(pending_ids) / sizeof(pending_ids[0]); i++) {
    bool duplicate = false;
    for (size_t previous = 0; previous < i; previous++)
      duplicate = duplicate || pending_ids[previous] == pending_ids[i];
    if (!duplicate) flush_pending_internal_error(pending_ids[i], reason);
  }
  remember_active_native_failure(operation, reason);
  emucap_disconnect();
}

bool adapter_exception_test_enabled() {
  const char* value = getenv("EMUCAP_ENABLE_TEST_ADAPTER_EXCEPTION");
  return value != nullptr
      && (!strcmp(value, "1") || !strcasecmp(value, "true"));
}

bool normalize_sync_advance(long id, long& count, bool allow_zero) {
  const long minimum = allow_zero ? 0 : 1;
  if (count < minimum) count = minimum;
  if (count <= MAX_SYNC_ADVANCE) return true;
  char msg[192];
  snprintf(msg, sizeof(msg),
           "frame/instruction count %ld exceeds synchronous limit %ld; split the request and verify each terminal response",
           count, MAX_SYNC_ADVANCE);
  reply_err(id, "bad_params", msg);
  return false;
}

AddressSpaceType* find_aspace(const std::string& name) {
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->AddressSpaces) return nullptr;
  for (auto& as : *CurGame->Debugger->AddressSpaces)
    if (as.name == name) return &as;
  return nullptr;
}

// 시스템 식별: 한 바이너리가 ss/psx/pce/pcfx/md/wswan/ngp를 모두 처리하므로(모두 컴파일·링크됨), 시스템 특화
// 코드(주소공간 매핑·버튼 테이블·엔디안)를 런타임에 분기한다. shortname은 MDFNGI 멤버이고
// psx.cpp는 "psx", ss.cpp는 "ss", pce.cpp는 "pce", pcfx.cpp는 "pcfx",
// md/system.cpp는 "md", wswan/main.cpp는 "wswan", ngp/neopop.cpp는 "ngp"로 설정한다.
const char* system_shortname() {
  return (CurGame && CurGame->shortname) ? CurGame->shortname : "";
}

bool is_psx() {
  return !strcmp(system_shortname(), "psx");
}

uint32 fold_psx_exec_address(uint32 address) {
  static const uint32 masks[8] = {
      0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu,
      0x7FFFFFFFu, 0x1FFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu};
  return address & masks[address >> 29];
}

bool is_pce() {
  const char* s = system_shortname();
  return !strcmp(s, "pce") || !strcmp(s, "pce_fast");
}

bool is_pcfx() {
  return !strcmp(system_shortname(), "pcfx");
}

bool is_ss() {
  return !strcmp(system_shortname(), "ss");
}

bool is_md() {
  return !strcmp(system_shortname(), "md");
}

bool is_ws() {
  return !strcmp(system_shortname(), "wswan");
}

bool is_ngp() {
  return !strcmp(system_shortname(), "ngp");
}

std::string cpu_targets_json() {
  const char* alias = "cpu";
  const char* mode = "auto";
  if (is_ss()) { alias = "sh2"; mode = "sh2"; }
  else if (is_psx()) { alias = "r3000a"; mode = "mips"; }
  else if (is_pce()) { alias = "huc6280"; mode = "huc6280"; }
  else if (is_pcfx()) { alias = "v810"; mode = "v810"; }
  else if (is_md()) { alias = "m68000"; mode = "m68k"; }
  else if (is_ws()) { alias = "v30mz"; mode = "x86"; }
  else if (is_ngp()) { alias = "tlcs900h"; mode = "tlcs900h"; }
  std::string modes = "[\"auto\"]";
  if (strcmp(mode, "auto")) modes = "[\"auto\",\"" + std::string(mode) + "\"]";
  return "[{\"id\":\"main\",\"aliases\":[\"" + std::string(alias) +
      "\"],\"default\":true,\"disassembly_modes\":" + modes + "}]";
}

std::string hex_bytes(const uint8* data, size_t len) {
  std::string out;
  out.reserve(len * 2);
  for (size_t i = 0; i < len; i++) {
    char h[3];
    snprintf(h, sizeof(h), "%02x", data[i]);
    out += h;
  }
  return out;
}

bool validate_snapshot_specs(
    long id,
    const std::string& line,
    std::vector<EmucapSnapshotSpec>& snapshots) {
  std::string error;
  const EmucapSnapshotParseStatus status =
      emucap_parse_snapshot_specs(line, snapshots, error);
  if (status == EmucapSnapshotParseStatus::invalid) {
    reply_err(id, "bad_params", error.c_str());
    return false;
  }
  if (snapshots.empty()) return true;
  if (!is_pcfx()) {
    reply_err(id, "unsupported", "breakpoint snapshots are currently supported only for PC-FX");
    return false;
  }
  if (snapshots.size() > SNAPSHOT_CAP) {
    reply_err(id, "bad_params", "breakpoint snapshot range limit exceeded");
    return false;
  }
  uint64 total = 0;
  for (const EmucapSnapshotSpec& snapshot : snapshots) {
    AddressSpaceType* space = find_aspace(snapshot.memory_type);
    const uint64 end = (uint64)snapshot.address + snapshot.length;
    if (!space || end > space->size) {
      reply_err(id, "bad_params", "breakpoint snapshot range is outside its memory type");
      return false;
    }
    total += snapshot.length;
    if (total > SNAPSHOT_BYTE_CAP) {
      reply_err(id, "bad_params", "breakpoint snapshot byte limit exceeded");
      return false;
    }
  }
  return true;
}

std::string capture_snapshot_json(const std::vector<EmucapSnapshotSpec>& snapshots) {
  std::string output = "[";
  for (size_t index = 0; index < snapshots.size(); index++) {
    const EmucapSnapshotSpec& snapshot = snapshots[index];
    AddressSpaceType* space = find_aspace(snapshot.memory_type);
    if (!space) throw std::runtime_error("snapshot memory type disappeared");
    std::vector<uint8> bytes(snapshot.length);
    space->GetAddressSpaceBytes(
        snapshot.memory_type.c_str(), snapshot.address, snapshot.length, bytes.data());
    if (index) output += ",";
    output += "{\"memory_type\":\"" + json_escape(snapshot.memory_type) + "\",\"address\":"
        + std::to_string(snapshot.address) + ",\"length\":" + std::to_string(snapshot.length)
        + ",\"hex\":\"" + hex_bytes(bytes.data(), bytes.size()) + "\"}";
  }
  output += "]";
  return output;
}

void reset_input_diagnostics() {
  g_last_game_data = 0;
  g_last_smpc_read_addr = 0xFFFFFFFFu;
  g_last_smpc_read_value = 0;
  g_smpc_read_count = 0;
  g_smpc_read_mask = 0;
  memset(g_last_smpc_oreg, 0, sizeof(g_last_smpc_oreg));
}

// Saturn(SH-2) RAM/메모리 region → SH-2 외부버스 base/size 표. set_breakpoint의 region→버스주소
// 변환과 emucap_read_value_for_bp의 버스주소→aspace 역변환이 같은 표를 공유한다(드리프트 방지).
// 베이스/크기 권위 출처(work/mednafen/src/ss): ss.cpp의 SH-2 외부버스 주소맵 주석 + 각 region
// 디코드 — workraml: BusRW_DB_CS0(0x00200000, A&0xFFFFF), workramh: BusRW_DB_CS3(0x06000000,
// A&0xFFFFF), scspram: scu.inc→SOUND_Write16(A&0x1FFFFF)→SS_SCSP::RW(A<0x80000, RAM[A]),
// vdp1vram: vdp1.cpp Write16_DB(A&0x1FFFFF<0x80000 → VRAM[A>>1]), vdp2vram: vdp2.cpp RW
// (A&0x1FFFFF<0x100000 → vri=(A&0x7FFFF)>>1), cram: vdp2.cpp RW(A&0x1FFFFF<0x180000 →
// cri=(A&0xFFF)>>1, 버스 base 0x05F00000). aspace 마스크(debug.inc GetAddressSpaceBytes)도 동일
// (workram &0xFFFFF, scspram/vdp*vram &0x7FFFF, cram &0xFFF)이라 offset↔버스offset이 1:1 선형.
// 제외(BP 미지원): backup(0x00180000, 8bit→16bit 매핑이라 offset×2+홀수 비선형), vdp1fb0/fb1
// (0x05C80000은 FBDrawWhich 한쪽만 보이고 8bpp/rotate 시 주소 swizzle), scspmprog/scsptemp/
// scspmems/dspprog(레지스터 포트 뒤 내부 메모리라 SH-2 버스 선형주소 없음).
struct SSBusRegion { const char* mt; uint32 base; uint32 size; };
const SSBusRegion kSSBusRegions[] = {
  {"workraml", 0x00200000u, 0x100000u},  // Low Work RAM 1MB (CS0)
  {"workramh", 0x06000000u, 0x100000u},  // High Work RAM/SDRAM 1MB (CS3)
  {"scspram",  0x05A00000u, 0x80000u},   // SCSP RAM 512KB (CS2)
  {"vdp1vram", 0x05C00000u, 0x80000u},   // VDP1 VRAM 512KB (CS2)
  {"vdp2vram", 0x05E00000u, 0x80000u},   // VDP2 VRAM 512KB (CS2)
  {"cram",     0x05F00000u, 0x1000u},    // VDP2 CRAM 4KB (CS2)
};

// 값-조건 BP용: addr의 len(1~4)바이트를 읽어 정수로. emucap_cpu_cb이 freeze 전 BP
// value/value_mask와 비교한다. 시스템별로 주소공간·엔디안이 다르다(아래 분기).
uint32 emucap_read_value_for_bp(const BP& bp, uint32 addr, unsigned len) {
  const char* asname;
  uint32 off;
  bool psx = is_psx();
  bool pce = is_pce();
  bool pcfx = is_pcfx();
  bool md = is_md();
  bool ws = is_ws();
  if (psx) {
    // PSX(MIPS): cpu aspace 단일 경로. cpu는 addr_mask로 KUSEG/KSEG0/KSEG1 미러를 접고
    // MainRAM·스크래치패드(0x1F800000~)·BIOS(0x1FC00000~)·HW(0x1F801000~)를 모두 디코드하므로
    // logical addr를 그대로 넘긴다(Saturn식 logical→aspace 분기 불필요).
    asname = "cpu"; off = addr;
  } else if (pce) {
    // PCE(HuC6280): cpu BP는 16비트 logical, physical BP는 21비트 물리 주소 기준이다.
    // cpu aspace는 현재 MPR 매핑을 반영한다. 값 조립은 65C02 계열 리틀엔디언.
    asname = bp.logical ? "cpu" : "physical";
    off = bp.logical ? (addr & 0xFFFF) : (addr & 0x1FFFFF);
  } else if (pcfx) {
    // PC-FX(V810): debugger read/write BP는 32비트 CPU physical 주소를 보고한다.
    // V810은 little-endian이고 cpu aspace가 RAM/backup/BIOS 버스를 디코드한다.
    asname = "cpu"; off = addr;
  } else if (md) {
    // Mega Drive/Genesis(68000): debugger read/write BP가 24비트 CPU physical 주소를 보고한다.
    // Work RAM은 0xFF0000~0xFFFFFF mirror이며, 다바이트 값은 68000 big-endian으로 조립한다.
    asname = "cpu"; off = addr & 0xFFFFFF;
  } else if (ws) {
    // WonderSwan(V30MZ, x86계): read/write BP가 20비트 물리 주소를 보고한다(v30mz PutMemB/GetMemB의
    // (seg<<4)+off). physical aspace가 WSwan_readmem20으로 구현돼 있어 그대로 읽고, 값은 x86 리틀엔디언.
    asname = "physical"; off = addr & 0xFFFFF;
  } else {
    // Saturn(SH-2): physical aspace는 미구현(0 반환)이라 RAM/메모리 region을 SH-2 외부버스 주소로
    // 식별해 해당 aspace에서 값을 읽는다(set_breakpoint의 region→버스 변환과 같은 kSSBusRegions를
    // 역으로 사용). 예: 0x06000000~=workramh, 0x00200000~=workraml, 0x05E00000~=vdp2vram 등.
    // 값-조건 read BP가 이 경로로 읽으므로(write BP는 주입값 사용) 변환 가능한 모든 region을 커버.
    asname = "physical"; off = addr;
    for (const auto& r : kSSBusRegions) {
      if (addr >= r.base && addr < r.base + r.size) { asname = r.mt; off = addr - r.base; break; }
    }
  }
  AddressSpaceType* sp = find_aspace(asname);
  if (!sp) return 0;
  uint8 buf[4] = {0};
  unsigned n = len > 4 ? 4 : (len < 1 ? 1 : len);
  sp->GetAddressSpaceBytes(asname, off, n, buf);
  uint32 v = 0;
  if (psx || pce || pcfx || ws)
    for (unsigned i = 0; i < n; i++) v |= (uint32)buf[i] << (i * 8);  // MIPS/HuC6280/V810/V30MZ little-endian
  else
    for (unsigned i = 0; i < n; i++) v = (v << 8) | buf[i];           // SH-2/68000 big-endian
  return v;
}

// PC의 명령을 call/return/other로 분류한다(shadow stack call_stack용). 시스템별 CPU 주소공간·엔디안·ISA:
// 68000(BSR/JSR·RTS/RTR), SH-2(BSR/BSRF/JSR·RTS), MIPS(JAL/JALR·JR $ra), HuC6280(JSR·RTS).
enum CallKind { CK_OTHER = 0, CK_CALL, CK_RETURN };
CallKind classify_instr(uint32 pc) {
  if (is_pcfx()) {
    AddressSpaceType* space = find_aspace("cpu");
    if (!space) return CK_OTHER;
    uint8 bytes[2] = {0, 0};
    space->GetAddressSpaceBytes("cpu", pc, sizeof(bytes), bytes);
    const uint16 first_halfword = (uint16)bytes[0] | ((uint16)bytes[1] << 8);
    const EmucapV810CallKind kind = emucap_v810_classify(first_halfword);
    if (kind == EmucapV810CallKind::call) return CK_CALL;
    if (kind == EmucapV810CallKind::return_from_call) return CK_RETURN;
    return CK_OTHER;
  }
  const char* asname = "cpu";
  uint32 off = pc;
  if (is_pce()) {
    off = pc & 0xFFFF;
  } else if (is_md()) {
    off = pc & 0xFFFFFF;
  } else if (is_ws()) {
    // WonderSwan(V30MZ): PC는 IP offset이다. cs aspace가 현재 CS:IP를 (CS<<4)+off로 물리 변환해 읽는다.
    asname = "cs"; off = pc & 0xFFFF;
  } else if (is_ss()) {
    // SH-2 PC는 외부버스 주소 — kSSBusRegions로 region을 찾아 읽는다("physical" 미구현).
    asname = "physical";
    for (const auto& r : kSSBusRegions) {
      if (pc >= r.base && pc < r.base + r.size) {
        asname = r.mt;
        off = pc - r.base;
        break;
      }
    }
  }
  // psx: asname="cpu", off=pc(기본값)
  AddressSpaceType* sp = find_aspace(asname);
  if (!sp) return CK_OTHER;
  uint8 b[6] = {0, 0, 0, 0, 0, 0};
  if (is_ws()) {  // V30MZ(NEC V30, 8086 계열): 가변길이 — 선행 프리픽스를 건너뛴 뒤 opcode로 call/ret 판별.
    sp->GetAddressSpaceBytes(asname, off, 6, b);
    unsigned i = 0;
    // V30 프리픽스: 세그먼트 오버라이드(ES/CS/SS/DS=0x26/2E/36/3E)·LOCK(0xF0)·REP/REPNE(0xF2/F3)만.
    // 0x64/65/66/67(FS/GS·operand/address-size 오버라이드)은 386+ 도입이라 V30MZ엔 없다.
    while (i < 4 && (b[i] == 0x26 || b[i] == 0x2E || b[i] == 0x36 || b[i] == 0x3E ||
                     b[i] == 0xF0 || b[i] == 0xF2 || b[i] == 0xF3)) i++;
    uint8 op = b[i];
    if (op == 0xE8 || op == 0x9A) return CK_CALL;  // CALL rel16 / CALL far
    if (op == 0xFF) { unsigned reg = (b[i + 1] >> 3) & 7; if (reg == 2 || reg == 3) return CK_CALL; }  // CALL r/m near/far
    if (op == 0xC3 || op == 0xC2 || op == 0xCB || op == 0xCA) return CK_RETURN;  // RET near/far
    return CK_OTHER;
  }
  if (is_pce()) {  // HuC6280: 8비트 opcode
    sp->GetAddressSpaceBytes(asname, off, 1, b);
    if (b[0] == 0x20) return CK_CALL;   // JSR
    if (b[0] == 0x60) return CK_RETURN; // RTS
    return CK_OTHER;
  }
  if (is_psx()) {  // MIPS R3000A: 32비트 little-endian
    sp->GetAddressSpaceBytes(asname, off, 4, b);
    uint32 op = (uint32)b[0] | ((uint32)b[1] << 8) | ((uint32)b[2] << 16) | ((uint32)b[3] << 24);
    uint32 opc = op >> 26;
    if (opc == 3) return CK_CALL;  // JAL
    if (opc == 0) {
      uint32 funct = op & 0x3F;
      if (funct == 9) return CK_CALL;                                 // JALR
      if (funct == 8 && ((op >> 21) & 0x1F) == 31) return CK_RETURN;  // JR $ra
    }
    return CK_OTHER;
  }
  // md(68000) / ss(SH-2): 16비트 big-endian opcode
  sp->GetAddressSpaceBytes(asname, off, 2, b);
  uint16 op = (uint16)((b[0] << 8) | b[1]);
  if (is_md()) {
    if ((op & 0xFF00) == 0x6100) return CK_CALL;         // BSR
    if ((op & 0xFFC0) == 0x4E80) return CK_CALL;         // JSR
    if (op == 0x4E75 || op == 0x4E77) return CK_RETURN;  // RTS/RTR
    return CK_OTHER;
  }
  // ss(SH-2)
  if ((op & 0xF000) == 0xB000) return CK_CALL;  // BSR
  if ((op & 0xF0FF) == 0x0003) return CK_CALL;  // BSRF Rn
  if ((op & 0xF0FF) == 0x400B) return CK_CALL;  // JSR @Rn
  if (op == 0x000B) return CK_RETURN;           // RTS
  return CK_OTHER;
}

bool bp_pc_allows(const BP& bp, uint32 pc) {
  return !bp.has_pc_filter || (pc >= bp.pc_min && pc <= bp.pc_max);
}

bool bp_value_allows(const BP& bp, bool has_value, uint32 value) {
  if (!bp.has_value) return true;
  if (!has_value) return false;
  return (value & bp.value_mask) == (bp.value & bp.value_mask);
}

bool ranges_overlap(uint32 a_start, uint32 a_len, uint32 b_start, uint32 b_end) {
  uint64 a_end = (uint64)a_start + (a_len ? (uint64)a_len - 1 : 0);
  return (uint64)a_start <= (uint64)b_end && a_end >= (uint64)b_start;
}

void freeze_spin_until_resume() {
  g_frozen = true;
  g_frozen_via_cb = true;     // cb 안 park(BP 히트/명령 카운트 0) — 진입명령 cb 발화함 → resume이 공짜
                              // 실행 → step_instructions skip 안 함. park 위치가 권위(핸들러 아님).
  // resume까지 스핀. 단 step(g_step_remaining>0)이나 step_instructions(g_insn_remaining>0)이 들어오면
  // 빠져나와 진행시킨다 — 안 그러면 BP 히트/명령단위 freeze 상태에서 step이 게임을 못 돌려 frame hook이
  // 안 돌고 timeout 난다. 명령단위 step은 이 같은 스핀을 탈출해 continuous cb가 다음 N명령을 진행한다.
  while (g_frozen && g_step_remaining == 0 && g_insn_remaining == 0) {
    // A transport disconnect is not a release event. Reconnect while the GameThread remains
    // parked in this exact instruction-bound callback; returning would execute the rest of the
    // frame and silently invalidate the frozen observation point.
    if (g_fd < 0) {
      emucap_connect();
      if (g_fd < 0) {
        usleep(2000);
        continue;
      }
    }
    serve_socket_once();
    usleep(2000);               // 2ms — busy-spin 방지
  }
}

// freeze 균일화(백스톱 B2 + pause_on_hit 일관성): pause_on_hit BP가 히트하면 *무엇이 진행 중이든* 그 명령을
// interrupted({status:"interrupted",reason:"breakpoint",pc})로 즉시 닫고 freeze한다 — "pause_on_hit = 히트하면
// 멈춘다"를 도구 불문 균일하게(run_frames/press_buttons=g_def·probe=g_probe·step(frames)=g_step·tap도 g_step·
// step_instructions=g_insn). 이제 g_step/g_insn도 닫으면 freeze_spin의
// `while(... g_step_remaining==0 && g_insn_remaining==0)`이 스핀해 freeze가 발효된다. 이중응답 방지: step 완료
// (emucap_service `if(g_step_remaining>0 && g_step_id>=0)`)·insn 완료(cb `if(g_insn_remaining>0)`)가 remaining>0·
// id>=0으로 가드하므로 remaining=0·id=-1로 비우면 후속 self-complete가 안 일어난다. Rust는 interrupted를 정상
// result로 반환한다(protocol::STATUS_INTERRUPTED). press면 입력을 뗀다.
void flush_deferred_on_freeze(uint32 pc) {
  char buf[160];
  snprintf(buf, sizeof(buf),
           "{\"status\":\"interrupted\",\"reason\":\"breakpoint\",\"pc\":%u,\"frame\":%llu}",
           (unsigned)pc, (unsigned long long)g_frame);
  if (g_def_id >= 0) {
    if (g_def_is_press) { g_input_override.release(); g_def_is_press = false; }
    reply_ok(g_def_id, buf);
    g_def_id = -1;
    g_def_remaining = 0;
    g_def_progress_ms = 0;
  }
  if (g_probe_id >= 0) {            // probe 중 BP 끼면 결정론 측정 무효 → interrupted로 닫는다(hang 대신)
    const long completed = g_probe_requested > g_probe_remaining
        ? g_probe_requested - g_probe_remaining : 0;
    char probe_buf[256];
    snprintf(probe_buf, sizeof(probe_buf),
             "{\"status\":\"interrupted\",\"reason\":\"breakpoint\",\"pc\":%u,"
             "\"frame\":%llu,\"requested_frames\":%ld,\"completed_frames\":%ld}",
             (unsigned)pc, (unsigned long long)g_frame, g_probe_requested, completed);
    reply_ok(g_probe_id, probe_buf);
    g_probe_id = -1;
    g_probe_requested = 0;
    g_probe_remaining = 0;
    g_probe_progress_ms = 0;
  }
  if (g_step_id >= 0) {             // step(frames)/tap 진행 중 BP → step 중단·interrupted → freeze 발효(균일)
    reply_ok(g_step_id, buf);
    g_step_id = -1;
    g_step_remaining = 0;
    g_step_progress_ms = 0;
  }
  if (g_insn_step_id >= 0) {        // step_instructions 진행 중 BP → 중단·interrupted → freeze 발효(균일)
    reply_ok(g_insn_step_id, buf);
    g_insn_step_id = -1;
    g_insn_remaining = 0;
    g_insn_progress_ms = 0;
  }
}

// 첫 RegGroup(대개 CPU)의 레지스터를 {name:value} JSON 오브젝트로 — BP 히트 순간 컨텍스트(D0 등) 캡처용.
std::string capture_registers_json() {
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->RegGroups) return "";
  std::string out;
  for (auto* rg : *CurGame->Debugger->RegGroups) {
    out = "{";
    bool first = true;
    for (int x = 0; rg->Regs[x].bsize; x++) {
      if (rg->Regs[x].bsize == 0xFFFF) continue;
      uint32 val = rg->GetRegister(rg->Regs[x].id, nullptr, 0);
      char kv[128];
      snprintf(kv, sizeof(kv), "%s\"%s\":%u", first ? "" : ",", rg->Regs[x].name, (unsigned)val);
      out += kv;
      first = false;
    }
    out += "}";
    break;  // 첫 그룹(CPU)만 — 히트 컨텍스트엔 CPU 레지스터로 충분(전체는 get_state)
  }
  return out;
}

void enqueue_bp_hit(const BPHit& hit_in, bool should_freeze) {
  // 버퍼가 가득 찼고 non-freezing이면 비싼 capture_registers_json() 앞에서 즉시 드롭한다 — 핫 exec BP가
  // 드롭될 이벤트에도 매 히트 full-register JSON을 빌드하면 게임 스레드가 굶어 소켓이 링크 타임아웃(5s) 안에
  // 서비스되지 못하고 연결이 끊긴다. freezing BP(should_freeze)는 첫 히트에서 멈추므로 영향 없다.
  if (g_bp_hits.size() >= EVENT_CAP && !should_freeze) { g_bp_dropped++; return; }
  if (g_bp_hits.size() >= EVENT_CAP) {
    g_bp_hits.erase(g_bp_hits.begin());
    g_bp_dropped++;
  }
  BPHit hit = hit_in;
  // exec BP는 pc만이라 D0 등을 못 본다 — 히트 순간 CPU 레지스터를 캡처한다. access BP는 addr/value가 이미
  // 있고 write-BP firehose 증폭을 피하려 제외. 히트는 이산 이벤트라 비용 낮음.
  if (!hit.has_access && hit.registers.empty()) hit.registers = capture_registers_json();
  if (g_bp_hits.size() < EVENT_CAP) g_bp_hits.push_back(hit);  // poll_events 드레인용 누적
  else g_bp_dropped++;
  if (should_freeze) {
    flush_deferred_on_freeze(hit.pc);  // freeze 진입 전 진행 중 지연 명령을 interrupted로 마무리(timeout 방지)
    freeze_spin_until_resume();
  }
}

// 현재 g_bps로 코어 BP를 재구성하고 콜백을 (재)설치/해제. set/clear에서 공통 사용.
void rearm_breakpoints() {
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->AddBreakPoint) return;
  CurGame->Debugger->FlushBreakPoints(BPOINT_PC);
  CurGame->Debugger->FlushBreakPoints(BPOINT_READ);
  CurGame->Debugger->FlushBreakPoints(BPOINT_WRITE);
  CurGame->Debugger->FlushBreakPoints(BPOINT_AUX_READ);
  CurGame->Debugger->FlushBreakPoints(BPOINT_AUX_WRITE);
  bool has_core_bp = false;
  for (auto& b : g_bps) {
    if (b.adapter_bp) continue;
    CurGame->Debugger->AddBreakPoint(b.type, b.a1, b.a2, b.logical);
    has_core_bp = true;
  }
  // 명령단위 도구(step_instructions) 활성 시 continuous로 무장 → cb가 매 명령 발화(카운트다운·재freeze).
  // 비활성(g_insn_remaining==0)이면 기존 동작: BP 있을 때만 cb(continuous=false), 없으면 해제하여
  // DBG_NeedCPUHooks→false로 빠른 경로 복귀(BP 없을 때 오버헤드 0). continuous DebugMode는 매 명령
  // 비용이라 도구 활성 구간만 무장한다(resume이 즉시 해제).
  // continuous(매 명령 cb) = 명령단위 step 또는 실행추적 또는 레지스터워치 중 하나라도 활성.
  // rearm은 flag에서 매번 재계산하므로 resume이 trace/watch를 끄지 않는다(set_trace(false)/clear까지 유지).
  bool continuous = g_insn_remaining > 0 || g_core_refresh_id >= 0
      || g_trace_enabled || g_watch_enabled || g_break_on_reset;
  g_insn_armed = continuous;
  CurGame->Debugger->SetCPUCallback((has_core_bp || continuous) ? emucap_cpu_cb : nullptr, continuous);
}

bool defer_psx_core_refresh(long id, CoreRefreshReply reply) {
  if (!is_psx() || !g_frozen_via_cb) return false;
  g_core_refresh_id = id;
  g_core_refresh_reply = reply;
  g_frozen = false;
  rearm_breakpoints();
  return true;
}

bool complete_psx_core_refresh() {
  if (g_core_refresh_id < 0) return false;
  const long id = g_core_refresh_id;
  const CoreRefreshReply reply = g_core_refresh_reply;
  g_core_refresh_id = -1;
  g_core_refresh_reply = CORE_REFRESH_NONE;
  g_frozen = true;
  rearm_breakpoints();
  if (reply == CORE_REFRESH_LOAD_STATE)
    reply_ok(id, "{\"status\":\"completed\"}");
  else
    reply_ok(id, "{\"reset\":true}");
  freeze_spin_until_resume();
  return true;
}

// read_memory·probe가 한 번에 읽을 수 있는 최대 바이트(거대 length로 인한 과대 할당 방지).
static const long MAX_READ_LEN = 16L * 1024 * 1024;  // 16MB
static const long MAX_FIND_LEN = 16L * 1024 * 1024;  // 16MB — 벌크 read라 한 호출로 region 전체(예: SS workram 1MB) 스캔. read_memory와 동일 상한(초과 시 truncated→start 옮겨 청크)

// dump_memory: 한 AddressSpace를 통째 .bin으로 쓸 때의 최대 바이트. 이를 넘는 합성 전체-버스 공간
// (PSX "cpu" 4GB·Saturn "physical" 128MB)은 건너뛴다 — 4GB 파일·타임아웃을 피한다. 전용 RAM/VRAM
// 공간(work/video RAM 등, ≤2MB; 예: Saturn vdp2vram 512KB)은 모두 이 아래라 손실 없이 export된다.
static const uint64 DUMP_MAX_REGION_BYTES = 64ULL * 1024 * 1024;  // 64MB

// 주소공간 [addr, addr+len)을 hex 문자열로. aspace 없거나 범위 위반이면 false. read_memory·probe 공용.
bool validate_aspace_range(const std::string& mt, uint32 addr, long len) {
  AddressSpaceType* sp = find_aspace(mt);
  if (!sp) return false;
  if (len <= 0 || len > MAX_READ_LEN) return false;
  uint64 end = (uint64)addr + (uint64)len;
  return end <= 0x100000000ULL && (!sp->size || end <= sp->size);
}

bool read_aspace_hex(const std::string& mt, uint32 addr, long len, std::string& hex_out) {
  AddressSpaceType* sp = find_aspace(mt);
  // 주소·길이 범위 검증 — 거대 length는 reserve에서 std::bad_alloc을 던져 핸들러 밖으로
  // 탈출시켜 에뮬레이터를 죽인다. 이 검증으로 uint32 캐스팅(off/chunk)도 잘림 없이 안전해진다.
  if (!sp || !validate_aspace_range(mt, addr, len)) return false;
  hex_out.clear();
  hex_out.reserve((size_t)len * 2);
  static uint8 buf[0x10000];
  uint64 off = addr;
  long remaining = len;
  while (remaining > 0) {
    long chunk = remaining > (long)sizeof(buf) ? (long)sizeof(buf) : remaining;
    sp->GetAddressSpaceBytes(mt.c_str(), (uint32)off, (uint32)chunk, buf);
    for (long i = 0; i < chunk; i++) {
      char h[3];
      snprintf(h, sizeof(h), "%02x", buf[i]);
      hex_out += h;
    }
    off += chunk;
    remaining -= chunk;
  }
  return true;
}

// Saturn "physical"(합성 128MB SH-2 버스)은 미구현이라 read가 조용히 0을 준다 — advertise되는데도
// silent-wrong이다. read_memory·probe·find_pattern이 공통으로 거부해 zero-fill 데이터 대신 명확한
// "unimplemented" 신호를 준다(구체 region memory_type을 쓰게). 거부하면 true(핸들러는 return).
bool reject_ss_physical_read(long id, const std::string& mt) {
  if (is_ss() && mt == "physical") {
    reply_err(id, "unsupported",
              "Mednafen Saturn physical address space is unimplemented (reads 0); use a specific region memory_type (workraml/workramh/scspram/vdp1vram/vdp2vram/cram)");
    return true;
  }
  return false;
}

int hex_nibble(char c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  return -1;
}

bool decode_hex_bytes(const std::string& hex, std::vector<uint8>& out) {
  if (hex.empty() || (hex.size() % 2) != 0) return false;
  out.clear();
  out.reserve(hex.size() / 2);
  for (size_t i = 0; i < hex.size(); i += 2) {
    int hi = hex_nibble(hex[i]);
    int lo = hex_nibble(hex[i + 1]);
    if (hi < 0 || lo < 0) return false;
    out.push_back((uint8)((hi << 4) | lo));
  }
  return true;
}

void handle_find_pattern(long id, const std::string& line) {
  std::string mt = json_str(line, "memory_type");
  std::string pat_hex = json_str(line, "hex");
  // Saturn "physical"은 미구현(read=0)이라 스캔이 조용히 all-zeros를 훑어 거짓 "패턴 없음"을 낸다 —
  // read_memory와 동일하게 거부한다(silent-wrong 검색 결과 방지).
  if (reject_ss_physical_read(id, mt)) return;
  AddressSpaceType* sp = find_aspace(mt);
  if (!sp) { reply_err(id, "bad_params", "unknown memory_type"); return; }

  std::vector<uint8> pat;
  if (!decode_hex_bytes(pat_hex, pat)) {
    reply_err(id, "bad_params", "hex must be a non-empty even-length hexadecimal string");
    return;
  }
  uint32 start = 0;
  if (!json_u32_arg(id, line, "start", start, false)) return;
  long length = -1, max_matches = 256, align = 1;
  bool has_length = json_num(line, "length", length);
  json_num(line, "max_matches", max_matches);
  json_num(line, "align", align);
  if (max_matches < 1) max_matches = 1;
  if (max_matches > 4096) max_matches = 4096;
  if (align < 1) align = 1;
  if ((uint64)start >= sp->size) {
    reply_err(id, "bad_params", "start is outside the memory region");
    return;
  }

  uint64 available = sp->size - (uint64)start;
  uint64 requested = has_length ? (uint64)(length < 0 ? -1 : length) : available;
  if (has_length && length < 0) {
    reply_err(id, "bad_params", "length exceeds the memory region");
    return;
  }
  if (requested > available) requested = available;
  bool truncated = requested > (uint64)MAX_FIND_LEN;
  uint64 scan_len64 = truncated ? (uint64)MAX_FIND_LEN : requested;
  if (scan_len64 > 0xFFFFFFFFULL) scan_len64 = 0xFFFFFFFFULL;
  uint32 scan_len = (uint32)scan_len64;

  std::vector<uint8> buf(scan_len);
  if (scan_len)
    sp->GetAddressSpaceBytes(mt.c_str(), (uint32)start, scan_len, buf.data());

  std::vector<uint32> matches;
  if (pat.size() <= buf.size()) {
    for (uint32 off = 0; off + pat.size() <= buf.size(); off++) {
      if (((uint64)start + off) % (uint64)align != 0) continue;
      if (memcmp(buf.data() + off, pat.data(), pat.size()) == 0) {
        matches.push_back((uint32)((uint64)start + off));
        if ((long)matches.size() >= max_matches) {
          truncated = true;
          break;
        }
      }
    }
  }

  std::string arr = "[";
  for (size_t i = 0; i < matches.size(); i++) {
    char b[24];
    snprintf(b, sizeof(b), "%s%u", i ? "," : "", (unsigned)matches[i]);
    arr += b;
  }
  arr += "]";
  char tail[160];
  snprintf(tail, sizeof(tail),
           ",\"count\":%zu,\"start\":%u,\"scanned\":%u,\"truncated\":%s}",
           matches.size(), (unsigned)start, (unsigned)scan_len, truncated ? "true" : "false");
  reply_ok(id, "{\"matches\":" + arr + tail);
}

void handle_read_memory(long id, const std::string& line) {
  std::string mt = json_str(line, "memory_type");
  uint32 addr = 0;
  if (!json_u32_arg(id, line, "address", addr, true)) return;
  long len = 0;
  json_num(line, "length", len);
  // Saturn "physical"(합성 128MB SH-2 버스)은 미구현이라 read가 조용히 0을 준다 — advertise되는데도
  // silent-wrong이므로 명확히 거부하고 구체 region memory_type을 쓰게 한다(가치-조건 BP가 kSSBusRegions로
  // 하듯 SH-2 버스주소를 workraml/workramh/vdp2vram/cram 등으로 지정). probe·find_pattern과 공통 가드.
  if (reject_ss_physical_read(id, mt)) return;
  std::string hex;
  if (!read_aspace_hex(mt, addr, len, hex)) {
    reply_err(id, "bad_params", "unknown memory_type or address/length exceeds the region");
    return;
  }
  reply_ok(id, "{\"hex\":\"" + hex + "\"}");
}

void handle_write_memory(long id, const std::string& line) {
  std::string mt = json_str(line, "memory_type");
  uint32 addr = 0;
  if (!json_u32_arg(id, line, "address", addr, true)) return;
  std::string hex = json_str(line, "hex");
  AddressSpaceType* sp = find_aspace(mt);
  if (!sp) { reply_err(id, "bad_params", "unknown memory_type"); return; }
  if (is_pcfx() && (mt == "cpu" || mt == "bios" || mt.rfind("track", 0) == 0)) {
    reply_err(id, "unsupported",
              "PC-FX write_memory rejects cpu because it can mutate BIOS, and rejects bios/track views as protected or read-only");
    return;
  }
  if (is_ngp() && !emucap_ngp_memory_writable(emucap_ngp_memory(mt))) {
    reply_err(id, "unsupported",
              "Neo Geo Pocket write_memory supports ram only; rom and bios are observational views");
    return;
  }
  if (is_md() && mt == "cpu") {
    reply_err(id, "unsupported", "Mednafen MD cpu address space write is a no-op; use memory_type=ram");
    return;
  }
  // Saturn "physical"(합성 128MB SH-2 버스)은 미구현이라 write가 조용히 no-op이 된다 — MD cpu와 같이
  // 명확히 거부하고 구체 region memory_type을 쓰게 한다(silent-wrong 제거).
  if (is_ss() && mt == "physical") {
    reply_err(id, "unsupported",
              "Mednafen Saturn physical address space write is a no-op; use a specific region memory_type (workraml/workramh/scspram/vdp1vram/vdp2vram/cram)");
    return;
  }
  // 홀수 길이 hex는 마지막 nibble을 조용히 버리는 오류를 낸다 — Mesen 어댑터와 동일하게 거부한다.
  if (hex.size() % 2 != 0) { reply_err(id, "bad_params", "hex must be an even-length hexadecimal string"); return; }
  std::vector<uint8> bytes;
  if (!decode_hex_bytes(hex, bytes)) {
    reply_err(id, "bad_params", "hex must be a non-empty even-length hexadecimal string");
    return;
  }
  uint64 end = (uint64)addr + (uint64)bytes.size();
  if (end > 0x100000000ULL || (sp->size && end > sp->size)) {
    reply_err(id, "bad_params", "address+hex length exceeds the memory region");
    return;
  }
  if (!bytes.empty())
    sp->PutAddressSpaceBytes(mt.c_str(), (uint32)addr, (uint32)bytes.size(), 1, true,
                             bytes.data());
  char buf[48];
  snprintf(buf, sizeof(buf), "{\"written\":%zu}", bytes.size());
  reply_ok(id, buf);
}

// 디렉터리를 재귀 생성("mkdir -p" 상당). 각 경로 컴포넌트를 mkdir(0755)로 만들고 이미 있으면(EEXIST)
// 무시한다. 셸을 거치지 않아 경로 인젝션이 없다(Mesen의 os.execute("mkdir -p")·PC-98 os.makedirs 대응).
bool mkdir_p(const std::string& path) {
  if (path.empty()) return false;
  std::string accum;
  size_t start = 0;
  if (path[0] == '/') { accum = "/"; start = 1; }
  for (size_t pos = start; pos <= path.size(); pos++) {
    if (pos == path.size() || path[pos] == '/') {
      if (pos > start) {
        if (!accum.empty() && accum.back() != '/') accum += '/';
        accum += path.substr(start, pos - start);
        if (emucap_mkdir(accum.c_str()) != 0 && errno != EEXIST) return false;
      }
      start = pos + 1;
    }
  }
  return true;
}

// 벌크 메모리 덤프(emucap diff·교차-ROM 키-값 디프 입력). 각 debugger AddressSpace를 <dir>/<name>.bin으로
// 64KB 청크로 직접 기록하고, regions.json([{name,memory_type,base_address,size}])을 같은 디렉터리에 쓴다 —
// Mesen(emucap-core.lua)·PC-98(Rust bridge) dump_memory 출력 형태와 일치해 Rust analysis::dump이
// 그대로 읽고 `emucap diff`가 동작한다. state.json은 MCP(tools::dump_memory)가 get_state로 따로 기록하므로
// (Mesen/PC-98 어댑터도 안 쓴다) 여기서 쓰지 않는다. 거대 hex 와이어 전송 없이 어댑터가 파일에 직접 써서
// 512KB+ VRAM 등 벌크를 한 번에 export한다(read_memory 인라인 hex 한계 우회). base_address는 명명 공간 내
// 오프셋 기준이라 0(read_memory(memory_type=name, address=offset)와 같은 주소계).
void handle_dump_memory(long id, const std::string& line) {
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->AddressSpaces) {
    reply_err(id, "unsupported", "dump_memory requires debugger address spaces");
    return;
  }
  std::string dir = json_str(line, "path");
  if (dir.empty()) { reply_err(id, "bad_params", "path is required"); return; }
  if (!mkdir_p(dir)) { reply_err(id, "io_error", "failed to create directory"); return; }

  static uint8 buf[0x10000];   // 64KB 청크(거대 length로 인한 과대 할당·스택 부담 회피)
  std::string metas;           // regions.json 항목(실제로 쓴 space만)
  std::string skipped;         // 캡 초과로 건너뛴 합성 전체-버스 공간(reply로 보고)
  size_t count = 0;
  try {
    for (auto& as : *CurGame->Debugger->AddressSpaces) {
      // PSX "cpu"(4GB)·Saturn "physical"(128MB) 같은 합성 공간은 캡을 넘어 건너뛴다. 거대 .bin을
      // 만들지도, 조용히 빠뜨리지도 않고 skipped로 알린다. 전용 RAM/VRAM은 전부 이 아래라 export됨.
      if (as.size > DUMP_MAX_REGION_BYTES) {
        char sb[160];
        snprintf(sb, sizeof(sb), "%s{\"name\":\"%s\",\"size\":%llu}",
                 skipped.empty() ? "" : ",", json_escape(as.name).c_str(),
                 (unsigned long long)as.size);
        skipped += sb;
        continue;
      }
      FileStream fs(dir + "/" + as.name + ".bin", FileStream::MODE_WRITE);
      uint64 off = 0;
      while (off < as.size) {
        uint64 chunk = as.size - off;
        if (chunk > sizeof(buf)) chunk = sizeof(buf);
        as.GetAddressSpaceBytes(as.name.c_str(), (uint32)off, (uint32)chunk, buf);
        fs.write(buf, chunk);
        off += chunk;
      }
      fs.close();
      char mb[256];
      snprintf(mb, sizeof(mb),
               "%s{\"name\":\"%s\",\"memory_type\":\"%s\",\"base_address\":0,\"size\":%llu}",
               metas.empty() ? "" : ",",
               json_escape(as.name).c_str(), json_escape(as.name).c_str(),
               (unsigned long long)as.size);
      metas += mb;
      count++;
    }
    std::string json = "[" + metas + "]";
    FileStream mf(dir + "/regions.json", FileStream::MODE_WRITE);
    mf.write(json.data(), json.size());
    mf.close();
  } catch (std::exception& e) { reply_err(id, "io_error", e.what()); return; }

  std::string resp = "{\"path\":\"" + json_escape(dir) + "\",\"regions\":" + std::to_string(count);
  if (!skipped.empty()) resp += ",\"skipped\":[" + skipped + "]";
  resp += "}";
  reply_ok(id, resp);
}

// ASCII 대소문자 무시 문자열 비교(strcasecmp 의존 회피).
static bool ieq_s(const std::string& a, const std::string& b) {
  if (a.size() != b.size()) return false;
  for (size_t i = 0; i < a.size(); i++) {
    char ca = a[i], cb = b[i];
    if (ca >= 'A' && ca <= 'Z') ca += 32;
    if (cb >= 'A' && cb <= 'Z') cb += 32;
    if (ca != cb) return false;
  }
  return true;
}
// register 이름으로 RegGroups에서 현재 값을 읽는다 — "name" 또는 "group.name"(get_state가 노출하는 형태)
// 을 대소문자 무시로 매칭한다. watch_register가 매 명령 호출한다. 못 찾으면 false.
bool read_register_by_name(const std::string& want, uint32& out) {
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->RegGroups) return false;
  for (auto* rg : *CurGame->Debugger->RegGroups) {
    std::string gname = (rg->name && rg->name[0]) ? rg->name : "";
    for (int x = 0; rg->Regs[x].bsize; x++) {
      if (rg->Regs[x].bsize == 0xFFFF) continue;
      std::string rn = rg->Regs[x].name ? rg->Regs[x].name : "";
      if (ieq_s(want, rn) || (!gname.empty() && ieq_s(want, gname + "." + rn))) {
        out = rg->GetRegister(rg->Regs[x].id, nullptr, 0);
        return true;
      }
    }
  }
  return false;
}

// SP(스택 포인터) 레지스터를 시스템별 후보 이름으로 찾아 캐시한다(콜스택 반환 감지용). 매 명령 스캔을
// 피하려 set_trace 켤 때 1회 해소한다. 못 찾으면 이름이 비어 SP-pruning은 비활성(폴백: pop 없음).
void resolve_sp_reg() {
  static const char* md_names[] = {"A7", "SP", "SSP", "USP", nullptr};
  static const char* ss_names[] = {"R15", "SP", nullptr};
  static const char* psx_names[] = {"SP", "sp", "R29", "r29", nullptr};
  static const char* pce_names[] = {"SP", "S", nullptr};
  static const char* ws_names[] = {"SP", nullptr};  // V30MZ 스택 포인터(debug.cpp V30MZ_Regs)
  static const char* pcfx_names[] = {"SP", "PR3", nullptr};  // V810 PR3 is the stack pointer.
  const char** names = is_md() ? md_names : is_ss() ? ss_names : is_psx() ? psx_names
                     : is_ws() ? ws_names : is_pcfx() ? pcfx_names : pce_names;
  uint32 v;
  for (int i = 0; names[i]; i++) {
    if (read_register_by_name(names[i], v)) {
      g_sp_reg_name = names[i];
      return;
    }
  }
  g_sp_reg_name.clear();
}
bool read_sp(uint32& out) {
  if (g_sp_reg_name.empty()) return false;
  return read_register_by_name(g_sp_reg_name, out);
}

std::vector<std::string> register_group_names() {
  std::vector<std::string> names;
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->RegGroups) return names;
  int index = 0;
  for (auto* group : *CurGame->Debugger->RegGroups) {
    names.push_back(group->name && group->name[0]
                        ? std::string(group->name)
                        : "g" + std::to_string(index));
    index++;
  }
  return names;
}

std::string register_group_names_json() {
  const std::vector<std::string> names = register_group_names();
  std::string output = "[";
  for (std::size_t index = 0; index < names.size(); index++) {
    if (index) output += ",";
    output += "\"" + json_escape(names[index]) + "\"";
  }
  output += "]";
  return output;
}

bool register_group_name_equal(const std::string& left, const std::string& right) {
  return left.size() == right.size() && !strcasecmp(left.c_str(), right.c_str());
}

bool register_group_requested(
    const std::vector<std::string>& requested,
    const std::string& available) {
  if (requested.empty()) return true;
  for (const std::string& value : requested)
    if (register_group_name_equal(value, available)) return true;
  return false;
}

// 레지스터 그룹(SH-2/SCU/VDP 등)을 평탄한 "그룹.레지스터": 값 맵으로. 구분자(bsize 0xFFFF) 제외.
void handle_get_state(long id, const std::string& line) {
  std::vector<std::string> requested;
  const EmucapJsonStringArrayStatus groups =
      emucap_json_string_array(line, "groups", requested);
  if (groups == EmucapJsonStringArrayStatus::invalid) {
    reply_err(id, "bad_params", "groups must be a list of non-empty strings");
    return;
  }
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->RegGroups) {
    reply_err(id, "no_debugger", "debugger is not initialized");
    return;
  }
  const std::vector<std::string> available = register_group_names();
  for (const std::string& wanted : requested) {
    bool found = false;
    for (const std::string& name : available)
      found = found || register_group_name_equal(wanted, name);
    if (!found) {
      const std::string message = "unknown state group '" + wanted
          + "'; available: " + register_group_names_json();
      reply_err(id, "bad_params", message.c_str());
      return;
    }
  }
  std::string out = "{\"state\":{";
  bool first = true;
  std::size_t group_index = 0;
  for (auto* rg : *CurGame->Debugger->RegGroups) {
    const std::string& group_name = available[group_index++];
    if (!register_group_requested(requested, group_name)) continue;
    for (int x = 0; rg->Regs[x].bsize; x++) {
      if (rg->Regs[x].bsize == 0xFFFF) continue;
      uint32 val = rg->GetRegister(rg->Regs[x].id, nullptr, 0);
      if (!first) out += ",";
      out += "\"" + json_escape(group_name) + "."
          + json_escape(rg->Regs[x].name) + "\":" + std::to_string((unsigned)val);
      first = false;
    }
  }
  out += "}";
  if (!requested.empty()) {
    out += ",\"groups_applied\":[";
    bool first_group = true;
    for (std::size_t wanted_index = 0; wanted_index < requested.size(); wanted_index++) {
      const std::string& wanted = requested[wanted_index];
      for (const std::string& name : available) {
        if (!register_group_name_equal(wanted, name)) continue;
        bool already_added = false;
        for (std::size_t index = 0; index < wanted_index; index++) {
          if (register_group_name_equal(requested[index], name)) already_added = true;
        }
        if (!already_added) {
          if (!first_group) out += ",";
          out += "\"" + json_escape(name) + "\"";
          first_group = false;
        }
        break;
      }
    }
    out += "]";
  }
  out += "}";
  reply_ok(id, out);
}

void handle_save_state(long id, const std::string& line) {
  std::string path = json_str(line, "path");
  if (path.empty()) { reply_err(id, "bad_params", "path is required"); return; }
  try {
    FileStream fs(path, FileStream::MODE_WRITE);
    MDFNSS_SaveSM(&fs);
    fs.close();
  } catch (std::exception& e) { reply_err(id, "io_error", e.what()); return; }
  reply_ok(id, "{\"status\":\"completed\"}");
}

void handle_load_state(long id, const std::string& line) {
  std::string path = json_str(line, "path");
  if (path.empty()) { reply_err(id, "bad_params", "path is required"); return; }
  try {
    FileStream fs(path, FileStream::MODE_READ);
    MDFNSS_LoadSM(&fs);
    fs.close();
  } catch (std::exception& e) { reply_err(id, "io_error", e.what()); return; }
  // PSX의 StateAction은 event scheduler를 timestamp 0으로 다시 맞춘다. BP callback의 복원 전
  // timestamp/pipeline으로 돌아가면 다음 advance에서 PSX_EventHandler assertion이 난다. 패치된 CPU가
  // 그 로컬 문맥을 버리고 복원 PC의 새 callback을 만든 뒤 이 요청을 완료한다.
  if (defer_psx_core_refresh(id, CORE_REFRESH_LOAD_STATE)) return;
  reply_ok(id, "{\"status\":\"completed\"}");
}

// 바이너리를 base64로 인코딩(screenshot PNG 응답용). 표준 알파벳, 패딩 포함.
std::string base64_encode(const uint8* data, size_t len) {
  static const char* T =
      "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  std::string out;
  out.reserve((len + 2) / 3 * 4);
  for (size_t i = 0; i < len; i += 3) {
    uint32_t n = (uint32_t)data[i] << 16;
    if (i + 1 < len) n |= (uint32_t)data[i + 1] << 8;
    if (i + 2 < len) n |= (uint32_t)data[i + 2];
    out += T[(n >> 18) & 63];
    out += T[(n >> 12) & 63];
    out += (i + 1 < len) ? T[(n >> 6) & 63] : '=';
    out += (i + 2 < len) ? T[n & 63] : '=';
  }
  return out;
}

// Mednafen IDIIS_Button*의 세 번째 인자는 BitOffset이 아니라 ConfigOrder다. 실제 BitOffset은
// IDIISG 생성자가 선언 순서대로 자동 배정한다(git.cpp). 아래 표는 각 코어의 IDII 선언 순서와
// padding을 기준으로 한 raw PortData 비트다.
//
// Saturn 표준 패드. PIDC[0].Data는 active-high(눌림=1)이고 코어가
// buttons = ~(data[0]|data[1]<<8)로 반전해 SMPC 버스에 내보낸다. l/r은 ls/rs 별칭.
struct BtnOff { const char* name; int off; };
const BtnOff g_satbtn[] = {
  {"z", 0}, {"y", 1}, {"x", 2},
  {"r", 3}, {"rs", 3}, {"r1", 3}, {"rb", 3},
  {"up", 4}, {"down", 5}, {"left", 6}, {"right", 7},
  {"b", 8}, {"c", 9}, {"a", 10}, {"start", 11}, {"enter", 11}, {"return", 11},
  {"l", 15}, {"ls", 15}, {"l1", 15}, {"lb", 15},
  {nullptr, 0}
};

// PSX 표준 디지털 패드/DualShock 버튼명→BitOffset. mednafen psx/input/gamepad.cpp의 IDII
// 선언 순서가 곧 BitOffset이고 PSX 하드웨어 표준 16비트 리포트와 일치한다(byte0={select,
// L3자리,R3자리,start,up,right,down,left}, byte1={l2,r2,l1,r1,triangle,circle,cross,square}).
// PortData[0..1]은 active-high(비트 set=눌림) — emucap_apply_input이 그대로 기록한다.
// l1/r1/l2/r2는 SNES식 l/r 별칭도 허용. DualShock 추가 비트: l3=1, r3=2(디지털 패드는 미사용).
const BtnOff g_psxbtn[] = {
  {"select", 0}, {"l3", 1}, {"r3", 2}, {"start", 3}, {"enter", 3}, {"return", 3},
  {"up", 4}, {"right", 5}, {"down", 6}, {"left", 7},
  {"l2", 8}, {"r2", 9}, {"l1", 10}, {"l", 10}, {"r1", 11}, {"r", 11},
  {"triangle", 12}, {"circle", 13}, {"o", 13}, {"cross", 14}, {"x", 14}, {"square", 15},
  {nullptr, 0}
};

// PC Engine 2/6-button pad. PCE는 I/II/RUN/SELECT 명칭을 쓰지만 에이전트 편의를 위해
// a/b/start 별칭도 허용한다. Bit 12는 mode_select 스위치라 버튼 입력으로 쓰지 않는다.
const BtnOff g_pcebtn[] = {
  {"i", 0}, {"a", 0}, {"ii", 1}, {"b", 1},
  {"select", 2}, {"run", 3}, {"start", 3}, {"enter", 3}, {"return", 3},
  {"up", 4}, {"right", 5}, {"down", 6}, {"left", 7},
  {"iii", 8}, {"iv", 9}, {"v", 10}, {"vi", 11},
  {nullptr, 0}
};

// PC-FX pad. gamepad.cpp's IDII declaration order is the raw little-endian input bit:
// I..VI, SELECT, RUN, directions, mode1, padding, mode2.
const BtnOff g_pcfxbtn[] = {
  {"i", 0}, {"a", 0}, {"ii", 1}, {"b", 1},
  {"iii", 2}, {"iv", 3}, {"v", 4}, {"vi", 5},
  {"select", 6}, {"run", 7}, {"start", 7}, {"enter", 7}, {"return", 7},
  {"up", 8}, {"right", 9}, {"down", 10}, {"left", 11},
  {"mode1", 12}, {"mode2", 14},
  {nullptr, 0}
};

// Mega Drive/Genesis pad. Mednafen MD IDII 선언 순서가 raw bit offset이다.
// 기본 launcher는 md.input.port1=gamepad6으로 고정해 x/y/z/mode까지 2바이트 버퍼로 받는다.
const BtnOff g_mdbtn[] = {
  {"up", 0}, {"down", 1}, {"left", 2}, {"right", 3},
  {"b", 4}, {"c", 5}, {"a", 6}, {"start", 7}, {"enter", 7}, {"return", 7},
  {"z", 8}, {"y", 9}, {"x", 10}, {"mode", 11},
  {nullptr, 0}
};

// WonderSwan/WSC 내장 패드. wswan/main.cpp IDII_GP 선언 순서가 곧 raw bit(WSButtonStatus =
// MDFN_de16lsb(PortData[0])): X커서 0-3(up/right/down/left), Y커서 4-7, start 8, a 9, b 10.
// 세로/가로 겸용이라 커서가 두 벌(X/Y)이다 — 공통 up/down/left/right는 X커서로, x1-x4/y1-y4는 명시.
// memory.cpp의 키매트릭스 읽기(H/X=&0xF, V/Y=>>4&0xF, buttons=>>8<<1)와 정확히 일치한다.
const BtnOff g_wsbtn[] = {
  {"up-x", 0}, {"x1", 0}, {"up", 0},
  {"right-x", 1}, {"x2", 1}, {"right", 1},
  {"down-x", 2}, {"x3", 2}, {"down", 2},
  {"left-x", 3}, {"x4", 3}, {"left", 3},
  {"up-y", 4}, {"y1", 4},
  {"right-y", 5}, {"y2", 5},
  {"down-y", 6}, {"y3", 6},
  {"left-y", 7}, {"y4", 7},
  {"start", 8}, {"enter", 8}, {"return", 8},
  {"a", 9}, {"b", 10},
  {nullptr, 0}
};

// Neo Geo Pocket/Color built-in pad. ngp/neopop.cpp declares the raw one-byte input in this
// order: directions, A, B, OPTION. Both monochrome and color cartridges use the same module.
const BtnOff g_ngpbtn[] = {
  {"up", 0}, {"down", 1}, {"left", 2}, {"right", 3},
  {"a", 4}, {"b", 5},
  {"option", 6}, {"start", 6}, {"enter", 6}, {"return", 6},
  {nullptr, 0}
};

// 활성 시스템의 버튼 테이블(런타임 분기). buttons_to_mask/mask_to_buttons가 사용.
const BtnOff* active_btntab() {
  if (is_psx()) return g_psxbtn;
  if (is_pce()) return g_pcebtn;
  if (is_pcfx()) return g_pcfxbtn;
  if (is_md()) return g_mdbtn;
  if (is_ws()) return g_wsbtn;
  if (is_ngp()) return g_ngpbtn;
  return g_satbtn;
}

bool lookup_button_bit(const std::string& name, uint16_t& bit) {
  for (const BtnOff* p = active_btntab(); p->name; p++) {
    if (name == p->name) {
      bit = (uint16_t)(1u << p->off);
      return true;
    }
  }
  return false;
}

bool buttons_to_mask(const std::string& line, uint16_t& mask, std::string& err) {
  mask = 0;
  size_t b = line.find("\"buttons\"");
  if (b == std::string::npos) return true;
  size_t lb = line.find('[', b);
  if (lb == std::string::npos) { err = "buttons must be a list"; return false; }
  size_t rb = line.find(']', lb);
  if (rb == std::string::npos || rb < lb) { err = "buttons must be a list"; return false; }
  std::vector<std::string> unknown;
  size_t i = lb + 1;
  while (i < rb) {
    size_t q1 = line.find('"', i);
    if (q1 == std::string::npos || q1 >= rb) break;
    size_t q2 = line.find('"', q1 + 1);
    if (q2 == std::string::npos || q2 > rb) { err = "malformed buttons array"; return false; }
    std::string tok = line.substr(q1 + 1, q2 - q1 - 1);
    for (char& c : tok) c = (char)tolower((unsigned char)c);
    uint16_t bit = 0;
    if (lookup_button_bit(tok, bit)) mask |= bit;
    else unknown.push_back(tok);
    i = q2 + 1;
  }
  if (!unknown.empty()) {
    err = std::string("unsupported ") + system_shortname() + " button";
    if (unknown.size() > 1) err += "s";
    err += ": ";
    for (size_t n = 0; n < unknown.size(); n++) {
      if (n) err += ",";
      err += unknown[n];
    }
    return false;
  }
  return true;
}

// 마스크 → 버튼명 JSON 배열(역디코드). 응답에 적용된 비트를 사람이 읽을 버튼명으로 echo한다.
// 별칭(ls/l, rs/r)은 같은 비트라 첫 이름만 출력한다.
std::string mask_to_buttons(uint16_t mask) {
  std::string out = "[";
  uint16_t seen = 0;
  bool first = true;
  for (const BtnOff* p = active_btntab(); p->name; p++) {
    uint16_t bit = (uint16_t)(1u << p->off);
    if ((mask & bit) && !(seen & bit)) {
      if (!first) out += ",";
      out += "\""; out += p->name; out += "\"";
      first = false; seen |= bit;
    }
  }
  out += "]";
  return out;
}

// NBG(0..3) 레지스터 디코드의 공유 소스 — get_video_state·resolve_tile가 같은 헬퍼를 쓴다(중복 구현
// 방지). 디코드 공식은 vdp2_render.cpp에서 복제. reg(a)=PeekRawReg.
// RawRegs는 렌더러 입력과 비트동일이라 공식만 정확히 복제하면 drift 0(게임 스레드 frozen → thread-safe).
struct NbgLayout {
  unsigned chctl_off, chctl_raw, CharSize, bmen, BMSize, colornum, bpp, isrgb;
  unsigned pncn_off, PNCNn, PNDSize, AuxMode, Supp, eff_charno_bits;
  unsigned cells_per_glyph, bytes_per_cell, glyph_bytes;
  unsigned PLSZ, PlaneSize, mpofn, mapoff;
  unsigned mapAB_off, mapCD_off, mapAB, mapCD;
  unsigned mnum[4], mnum_raw[4], mnum_off[4];
  int psshift;
  unsigned pbw[4], pbb[4];      // 플레인(네임테이블) 베이스: word / byte
  unsigned xscr_int, yscr_int;  // 정수 스크롤(NBG0/1=.8 소수의 정수부, NBG2/3=정수)
};

void decode_nbg_layout(int n, NbgLayout& L) {
  auto reg = [](uint32 a) -> unsigned { return (unsigned)MDFN_IEN_SS::VDP2::PeekRawReg(a); };
  L.bmen = 0; L.BMSize = 0;
  // 색/셀/비트맵: NBG0/1=CHCTLA(0x28), NBG2/3=CHCTLB(0x2A).
  if (n < 2) {
    unsigned sh = (unsigned)(n & 1) * 8;
    L.chctl_off = 0x28; L.chctl_raw = reg(0x28);
    L.CharSize = (L.chctl_raw >> (0 + sh)) & 1;
    L.bmen     = (L.chctl_raw >> (1 + sh)) & 1;
    L.BMSize   = (L.chctl_raw >> (2 + sh)) & 3;
    L.colornum = (L.chctl_raw >> (4 + sh)) & (n ? 3u : 7u);
  } else {
    unsigned sh = (unsigned)(n & 1) * 4;
    L.chctl_off = 0x2A; L.chctl_raw = reg(0x2A);
    L.CharSize = (L.chctl_raw >> (0 + sh)) & 1;
    L.colornum = (L.chctl_raw >> (1 + sh)) & 1;
  }
  if (L.colornum > 4) L.colornum = 4;
  static const unsigned bpp_tab[5]   = {4, 8, 16, 16, 32};
  static const unsigned isrgb_tab[5] = {0, 0, 0, 1, 1};
  L.bpp = bpp_tab[L.colornum];
  L.isrgb = isrgb_tab[L.colornum];

  // PND(PNCNn = 0x30 + n*2): bit15=PNDSize(0=2word,1=1word), bit14=AuxMode, bits9:0=supplement.
  L.pncn_off = 0x30 + n * 2; L.PNCNn = reg(L.pncn_off);
  L.PNDSize = (L.PNCNn >> 15) & 1;
  L.AuxMode = (L.PNCNn >> 14) & 1;
  L.Supp = L.PNCNn & 0x3FF;
  // 유효 charno 비트폭 = PNT 엔트리 타일인덱스 폭.
  L.eff_charno_bits = (!L.PNDSize) ? 15u : (L.AuxMode ? 12u : 10u);

  // cellbytes(도출, 0x20 하드코딩 금지).
  L.cells_per_glyph = L.CharSize ? 4u : 1u;
  L.bytes_per_cell = (L.bpp == 4) ? 0x20u : (L.bpp == 8) ? 0x40u : (L.bpp == 16) ? 0x80u : 0x100u;
  L.glyph_bytes = L.bytes_per_cell * L.cells_per_glyph;

  // 플레인(네임테이블) 베이스. PLSZ(0x3A), MPOFN(0x3C), MPABN/MPCDN(0x40+n*4 / 0x42+n*4).
  L.PLSZ = reg(0x3A); L.PlaneSize = (L.PLSZ >> (n * 2)) & 3;
  L.mpofn = reg(0x3C); L.mapoff = (L.mpofn >> (n * 4)) & 7;
  L.mapAB_off = 0x40 + n * 4; L.mapCD_off = 0x42 + n * 4;
  L.mapAB = reg(L.mapAB_off); L.mapCD = reg(L.mapCD_off);
  L.mnum[0] = L.mapAB & 0x3F; L.mnum[1] = (L.mapAB >> 8) & 0x3F;
  L.mnum[2] = L.mapCD & 0x3F; L.mnum[3] = (L.mapCD >> 8) & 0x3F;
  L.mnum_raw[0] = L.mapAB; L.mnum_raw[1] = L.mapAB; L.mnum_raw[2] = L.mapCD; L.mnum_raw[3] = L.mapCD;
  L.mnum_off[0] = L.mapAB_off; L.mnum_off[1] = L.mapAB_off; L.mnum_off[2] = L.mapCD_off; L.mnum_off[3] = L.mapCD_off;
  L.psshift = 13 - (int)L.PNDSize - ((int)L.CharSize << 1);
  for (int i = 0; i < 4; i++) {
    L.pbw[i] = ((L.mapoff << 6) + (L.mnum[i] & ~L.PlaneSize)) << L.psshift;  // word
    L.pbb[i] = L.pbw[i] << 1;                                                // ×2 = byte
  }

  // 정수 스크롤(resolve_tile용; 1:1 줌에서 ix=xscr_int+x). NBG0/1=SCXIN/SCYIN(.8 소수의 정수부),
  // NBG2/3=SCXN/SCYN(정수). get_video_state의 scroll 디코드와 같은 레지스터(0x70~/0x90~).
  if (n < 2) {
    L.xscr_int = reg(0x70 + n * 0x10) & 0x7FF;
    L.yscr_int = reg(0x74 + n * 0x10) & 0x7FF;
  } else {
    L.xscr_int = reg(0x90 + (n - 2) * 4) & 0x7FF;
    L.yscr_int = reg(0x92 + (n - 2) * 4) & 0x7FF;
  }
}

// VDP2 상태를 per-NBG(0..3)로 디코드해 노출(SS 전용 메서드). get_state엔 안 붙인다 — Mednafen은
// group 필터를 지원하지 않아 별도 메서드가 아니면 항상 계산돼 context 위생과 충돌한다. 디코드 공식은
// vdp2_render.cpp에서 복제한다.
// RawRegs는 렌더러 입력과 비트동일이라 공식만 정확히 복제하면 drift 0. 각 디코드 필드에
// {decoded, raw, reg_offset}를 동봉해 소비자가 raw로 자가검증한다. 게임별 보정상수
// (폰트 char base 등)는 넣지 않는다 — HW 디코드만(보정은 에이전트 RE 몫).
void handle_get_video_state(long id) {
  if (!is_ss()) { reply_err(id, "unsupported", "get_video_state is available only for Saturn"); return; }
  // reg(a) := VDP2::PeekRawReg(a). PeekRawReg가 (a>>1)&0xFF로 인덱싱하니 바이트 오프셋 a 전달.
  auto reg = [](uint32 a) -> unsigned { return (unsigned)MDFN_IEN_SS::VDP2::PeekRawReg(a); };
  // {decoded, raw, reg_offset} — decoded는 이미 완성된 JSON 값(숫자/true/false/"문자열").
  auto vf = [](const std::string& decoded, unsigned raw, unsigned off) -> std::string {
    char b[192];
    snprintf(b, sizeof(b), "{\"decoded\":%s,\"raw\":\"0x%04x\",\"reg_offset\":\"0x%02x\"}",
             decoded.c_str(), raw & 0xFFFFu, off & 0xFFu);
    return std::string(b);
  };
  auto jb = [](unsigned v) -> std::string { return v ? std::string("true") : std::string("false"); };
  auto jn = [](long long v) -> std::string { return std::to_string(v); };
  auto jq = [](const char* s) -> std::string { return std::string("\"") + s + "\""; };
  auto jhex = [](unsigned v) -> std::string { char b[16]; snprintf(b, sizeof(b), "\"0x%x\"", v); return std::string(b); };

  // BGON(0x20): bit n = NBGn enable, bit4/5 = RBG0/1 enable, bit n+8 = NBGn transparency-display.
  unsigned BGON = reg(0x20) & 0x1F3F;

  std::string nbg = "[";
  for (int n = 0; n < 4; n++) {
    unsigned enable = (BGON >> n) & 1;
    unsigned igntp  = (BGON >> (n + 8)) & 1;

    // NBG 레지스터 디코드는 공유 헬퍼로(resolve_tile와 동일 소스 — 중복 구현 방지). 아래 JSON 빌드는
    // L.* 별칭으로 받아 무변경 유지한다(출력 바이트 동일).
    NbgLayout L;
    decode_nbg_layout(n, L);
    unsigned chctl_off = L.chctl_off, chctl_raw = L.chctl_raw, CharSize = L.CharSize;
    unsigned bmen = L.bmen, BMSize = L.BMSize;
    unsigned bpp = L.bpp, isrgb = L.isrgb;
    unsigned pncn_off = L.pncn_off, PNCNn = L.PNCNn, PNDSize = L.PNDSize, AuxMode = L.AuxMode, Supp = L.Supp;
    unsigned eff_charno_bits = L.eff_charno_bits;
    unsigned cells_per_glyph = L.cells_per_glyph, bytes_per_cell = L.bytes_per_cell, glyph_bytes = L.glyph_bytes;
    unsigned PLSZ = L.PLSZ, PlaneSize = L.PlaneSize, mpofn = L.mpofn, mapoff = L.mapoff;
    unsigned (&mnum)[4] = L.mnum;
    unsigned (&mnum_raw)[4] = L.mnum_raw;
    unsigned (&mnum_off)[4] = L.mnum_off;
    int psshift = L.psshift;
    unsigned (&pbw)[4] = L.pbw;
    unsigned (&pbb)[4] = L.pbb;

    std::string o = "{";
    o += "\"n\":" + jn(n);
    o += ",\"enable\":" + vf(jb(enable), BGON, 0x20);
    o += ",\"ignore_transparent\":" + vf(jb(igntp), BGON, 0x20);
    if (n == 0)  // RBG1(BGON bit5) 켜지면 NBG0 슬롯 점유.
      o += ",\"rbg1_occupies_slot\":" + vf(jb((BGON >> 5) & 1), BGON, 0x20);
    o += ",\"char_size\":" + vf(jq(CharSize ? "16x16" : "8x8"), chctl_raw, chctl_off);
    o += ",\"color_mode\":" + vf(jq(bpp == 4 ? "4bpp" : bpp == 8 ? "8bpp"
                                    : bpp == 16 ? (isrgb ? "16bpp_rgb" : "16bpp_pal") : "32bpp_rgb"),
                                 chctl_raw, chctl_off);
    o += ",\"bpp\":" + jn(bpp);
    o += ",\"is_rgb\":" + jb(isrgb);
    o += ",\"pnd\":{";
    o +=   "\"mode\":" + vf(jq(PNDSize ? "1word" : "2word"), PNCNn, pncn_off);
    o +=   ",\"aux_mode\":" + vf(jn(AuxMode), PNCNn, pncn_off);
    o +=   ",\"supplement\":" + vf(jhex(Supp), PNCNn, pncn_off);
    o +=   ",\"eff_charno_bits\":" + jn(eff_charno_bits);
    o += "}";
    o += ",\"cell\":{\"bytes_per_cell\":" + jn(bytes_per_cell)
       + ",\"cells_per_glyph\":" + jn(cells_per_glyph)
       + ",\"glyph_bytes\":" + jn(glyph_bytes) + "}";
    o += ",\"plane\":{";
    o +=   "\"plane_size\":" + vf(jn(PlaneSize), PLSZ, 0x3A);
    o +=   ",\"map_offset\":" + vf(jn(mapoff), mpofn, 0x3C);
    o +=   ",\"map_num\":[";
    for (int i = 0; i < 4; i++) { if (i) o += ","; o += vf(jn(mnum[i]), mnum_raw[i], mnum_off[i]); }
    o +=   "]";
    o +=   ",\"psshift\":" + jn(psshift);
    o +=   ",\"plane_base_word\":[" + jn(pbw[0]) + "," + jn(pbw[1]) + "," + jn(pbw[2]) + "," + jn(pbw[3]) + "]";
    o +=   ",\"plane_base_byte\":[" + jn(pbb[0]) + "," + jn(pbb[1]) + "," + jn(pbb[2]) + "," + jn(pbb[3]) + "]";
    o += "}";
    o += ",\"scroll\":{";
    if (n < 2) {
      // .8 고정소수: SCXIN(정수 11bit) + SCXDN 상위바이트(소수 8bit).
      unsigned xi_off = 0x70 + n * 0x10, xf_off = 0x72 + n * 0x10;
      unsigned yi_off = 0x74 + n * 0x10, yf_off = 0x76 + n * 0x10;
      unsigned xi = reg(xi_off), xf = reg(xf_off), yi = reg(yi_off), yf = reg(yf_off);
      unsigned Xscr = ((xi & 0x7FF) << 8) + ((xf >> 8) & 0xFF);
      unsigned Yscr = ((yi & 0x7FF) << 8) + ((yf >> 8) & 0xFF);
      char sb[512];
      snprintf(sb, sizeof(sb),
        "\"format\":\".8fixed\","
        "\"x\":{\"decoded_fixed8\":%u,\"integer\":%u,\"fraction_256\":%u,"
        "\"raw_int\":\"0x%04x\",\"reg_offset_int\":\"0x%02x\",\"raw_frac\":\"0x%04x\",\"reg_offset_frac\":\"0x%02x\"},"
        "\"y\":{\"decoded_fixed8\":%u,\"integer\":%u,\"fraction_256\":%u,"
        "\"raw_int\":\"0x%04x\",\"reg_offset_int\":\"0x%02x\",\"raw_frac\":\"0x%04x\",\"reg_offset_frac\":\"0x%02x\"}",
        Xscr, (xi & 0x7FF), ((xf >> 8) & 0xFF), xi & 0xFFFF, xi_off & 0xFF, xf & 0xFFFF, xf_off & 0xFF,
        Yscr, (yi & 0x7FF), ((yf >> 8) & 0xFF), yi & 0xFFFF, yi_off & 0xFF, yf & 0xFFFF, yf_off & 0xFF);
      o += sb;
    } else {
      // 정수 스크롤: SCXN(0x90+(n-2)*4), SCYN(0x92+(n-2)*4).
      unsigned x_off = 0x90 + (n - 2) * 4, y_off = 0x92 + (n - 2) * 4;
      unsigned xr = reg(x_off), yr = reg(y_off);
      o += "\"format\":\"integer\",\"x\":" + vf(jn(xr & 0x7FF), xr, x_off);
      o += ",\"y\":" + vf(jn(yr & 0x7FF), yr, y_off);
    }
    o += "}";
    if (n < 2) {  // 비트맵(NBG0/1): BMOffset은 uint16 VRAM 워드인덱스(mapoff<<16) → byte=mapoff<<17(×0x20000).
      o += ",\"bitmap\":{\"enable\":" + vf(jb(bmen), chctl_raw, chctl_off);
      o +=   ",\"size\":" + vf(jn(BMSize), chctl_raw, chctl_off);
      o +=   ",\"offset_word\":" + jn((long long)(mapoff << 16));
      o +=   ",\"offset_byte\":" + jn((long long)(mapoff << 17));
      o += "}";
    }
    o += "}";
    if (n) nbg += ",";
    nbg += o;
  }
  nbg += "]";

  // common: RAMCTL VRAM/CRAM 모드, VCPRegs(fetch 허용 판정 — 주소 아님), CRAOFA/B.
  unsigned ramctl = reg(0x0E);
  unsigned cram_mode = (ramctl >> 12) & 3;
  unsigned vram_a = (ramctl >> 8) & 1, vram_b = (ramctl >> 9) & 1;
  const char* cram_label = (cram_mode == 0) ? "RGB555_1024" : (cram_mode == 1) ? "RGB555_2048"
                         : (cram_mode == 2) ? "RGB888_1024" : "illegal";
  unsigned craofa = reg(0xE4), craofb = reg(0xE6);
  static const struct { unsigned off; const char* name; } vcp[8] = {
    {0x10, "CYCA0L"}, {0x12, "CYCA0U"}, {0x14, "CYCA1L"}, {0x16, "CYCA1U"},
    {0x18, "CYCB0L"}, {0x1A, "CYCB0U"}, {0x1C, "CYCB1L"}, {0x1E, "CYCB1U"} };

  std::string common = "{";
  {
    char rb[320];
    snprintf(rb, sizeof(rb),
      "\"ramctl\":{\"raw\":\"0x%04x\",\"reg_offset\":\"0x0e\",\"vram_a_mode\":%u,\"vram_b_mode\":%u,"
      "\"cram_mode\":{\"decoded\":%u,\"label\":\"%s\",\"raw\":\"0x%04x\",\"reg_offset\":\"0x0e\"}},"
      "\"cram_mode\":{\"decoded\":%u,\"label\":\"%s\",\"raw\":\"0x%04x\",\"reg_offset\":\"0x0e\"}",
      ramctl & 0xFFFF, vram_a, vram_b, cram_mode, cram_label, ramctl & 0xFFFF,
      cram_mode, cram_label, ramctl & 0xFFFF);
    common += rb;
  }
  common += ",\"vcp_regs\":[";
  for (int i = 0; i < 8; i++) {
    char vb[96];
    snprintf(vb, sizeof(vb), "%s{\"name\":\"%s\",\"raw\":\"0x%04x\",\"reg_offset\":\"0x%02x\"}",
             i ? "," : "", vcp[i].name, reg(vcp[i].off) & 0xFFFF, vcp[i].off & 0xFF);
    common += vb;
  }
  common += "]";
  {
    char eb[320];
    snprintf(eb, sizeof(eb),
      ",\"craofa\":{\"raw\":\"0x%04x\",\"reg_offset\":\"0xe4\",\"note\":\"per-NBG CRAM addr offset, 3-bit fields N0..N3\"},"
      "\"craofb\":{\"raw\":\"0x%04x\",\"reg_offset\":\"0xe6\",\"note\":\"RBG0(bits2:0)+sprite(bits6:4) CRAM addr offset\"}",
      craofa & 0xFFFF, craofb & 0xFFFF);
    common += eb;
  }
  common += ",\"note\":\"vcp_regs are VRAM access-cycle patterns (fetch permission), not addresses\"";
  common += "}";

  // rbg: enable만(회전 파라미터 디코드는 범위 밖).
  std::string rbg = "[";
  rbg += "{\"n\":0,\"enable\":" + vf(jb((BGON >> 4) & 1), BGON, 0x20)
       + ",\"note\":\"RBG0; rotation params not decoded\"}";
  rbg += ",{\"n\":1,\"enable\":" + vf(jb((BGON >> 5) & 1), BGON, 0x20)
       + ",\"note\":\"RBG1 shares NBG0 cell resources\"}";
  rbg += "]";

  std::string resp = "{\"system\":\"ss\",\"nbg\":" + nbg + ",\"rbg\":" + rbg + ",\"common\":" + common
    + ",\"note\":\"RawRegs shadow = renderer input bit-for-bit; latest value at frozen frame boundary "
      "(per-line raster mid-frame history not captured); game-unwritten regs read 0\"}";
  reply_ok(id, resp);
}

// resolve_tile(nbg,x,y): 화면좌표 → 그 셀의 char 데이터 베이스 주소를 per-tile로 푼다(SS 전용).
// 스크롤 가산 → 맵셀 → PLSZ 랩 → 네임테이블(PNT) 엔트리 읽기 → supplement 반영 charno → char 데이터
// 주소. 좌표/PNT 디코드 공식은 렌더러(vdp2_render.cpp Fetch, 비-회전 NBG)에서 그대로 복제.
// 반환에 중간값(nt_addr·raw PND·charno·cellbytes·palno·flip)을 동봉해 소비자가 자가검증·합성한다.
// 게임 폰트 char-base 보정상수는 넣지 않는다(HW 디코드만 — 보정은 에이전트 RE 몫).
void handle_resolve_tile(long id, const std::string& line) {
  if (!is_ss()) { reply_err(id, "unsupported", "resolve_tile is available only for Saturn"); return; }
  long nbg = -1, sx = -1, sy = -1;
  json_num(line, "nbg", nbg);
  json_num(line, "x", sx);
  json_num(line, "y", sy);
  if (nbg < 0 || nbg > 3) { reply_err(id, "bad_params", "nbg must be 0..3 (NBG0..3; rotating RBG layers are excluded)"); return; }
  if (sx < 0 || sy < 0)   { reply_err(id, "bad_params", "x and y must be non-negative"); return; }
  int n = (int)nbg;

  NbgLayout L;
  decode_nbg_layout(n, L);
  unsigned PlaneSize = L.PlaneSize, PNDSize = L.PNDSize, CharSize = L.CharSize;
  unsigned AuxMode = L.AuxMode, Supp = L.Supp, bpp = L.bpp;

  // 스크롤 가산: 렌더러는 xc=CurXScrollIF; ix=xc>>8; xc+=xcinc 누적이다. 1:1 줌(xcinc=0x100)에서
  // ix = xscr_int + x로 동치(소수부는 정수타일을 안 바꾼다). 줌(ZMCTL)·모자이크·라인/세로셀 스크롤은
  // 적용 안 함 — 정수 스크롤 1:1 타일 해상도(note로 명시).
  uint32 ix = (uint32)((unsigned)L.xscr_int + (unsigned)sx);
  uint32 iy = (uint32)((unsigned)L.yscr_int + (unsigned)sy);

  // 좌표→네임테이블 워드주소(렌더러 Fetch 비-회전 NBG 공식 그대로; adj_map_regs=plane_base_word).
  uint32 mapidx = ((ix >> (9 + (bool)(PlaneSize & 0x1))) & 0x1)
                | ((iy >> (9 + (bool)(PlaneSize & 0x2) - 1)) & 0x2);
  uint32 planeidx = ((ix >> 9) & PlaneSize & 0x1) | ((iy >> (9 - 1)) & PlaneSize & 0x2);
  uint32 planeoffs = planeidx << (13 - PNDSize - (CharSize << 1));
  uint32 pageoffs = ((((ix >> 3) & 0x3F) >> CharSize)
                     + ((((iy >> 3) & 0x3F) >> CharSize) << (6 - CharSize))) << (1 - PNDSize);
  uint32 nt_addr = (L.pbw[mapidx] + planeoffs + pageoffs) & 0x3FFFF;  // word index

  // PNT 엔트리 읽기(vdp2vram, big-endian: PeekVRAM=ne16_rbo_be). nt_addr*2 = byte 주소.
  AddressSpaceType* sp = find_aspace("vdp2vram");
  if (!sp) { reply_err(id, "unsupported", "vdp2vram address space is unavailable"); return; }
  auto rdword = [&](uint32 word_idx) -> unsigned {
    uint8 b[2];
    uint32 byte_addr = (word_idx & 0x3FFFF) << 1;
    sp->GetAddressSpaceBytes("vdp2vram", byte_addr, 2, b);
    return (unsigned)((b[0] << 8) | b[1]);  // big-endian word (= VRAM[word_idx])
  };
  uint32 pnt_byte = nt_addr << 1;
  unsigned pnd0 = rdword(nt_addr);
  unsigned pnd1 = (!PNDSize) ? rdword(nt_addr + 1) : 0;  // 2word는 둘째 워드에 charno

  // charno·palno·flip 디코드(렌더러 Fetch 378-426 공식 그대로, supplement 반영).
  unsigned palno = 0, charno = 0, hflip = 0, vflip = 0, spr = 0, scc = 0;
  if (!PNDSize) {
    unsigned tmp = pnd0;
    palno = tmp & 0x7F;
    vflip = (tmp & 0x8000) ? 1u : 0u;
    hflip = (tmp & 0x4000) ? 1u : 0u;
    spr   = (tmp & 0x2000) ? 1u : 0u;
    scc   = (tmp & 0x1000) ? 1u : 0u;
    charno = pnd1 & 0x7FFF;
  } else {
    unsigned tmp = pnd0;
    if (bpp >= 8) palno = ((tmp >> 12) & 0x7) << 4;
    else          palno = ((tmp >> 12) & 0xF) | (((Supp >> 5) & 0x7) << 4);
    spr = (Supp & 0x200) ? 1u : 0u;
    scc = (Supp & 0x100) ? 1u : 0u;
    if (!AuxMode) {
      vflip = (tmp & 0x800) ? 1u : 0u;
      hflip = (tmp & 0x400) ? 1u : 0u;
      if (CharSize) charno = ((tmp & 0x3FF) << 2) + ((Supp & 0x1C) << 10) + (Supp & 0x3);
      else          charno = (tmp & 0x3FF) + ((Supp & 0x1F) << 10);
    } else {
      hflip = vflip = 0;
      if (CharSize) charno = ((tmp & 0xFFF) << 2) + ((Supp & 0x10) << 10) + (Supp & 0x3);
      else          charno = (tmp & 0xFFF) + ((Supp & 0x1C) << 10);
    }
  }
  unsigned charno_pnt = charno;  // PNT 디코드 베이스(16x16 셀 보정 전)

  // 16x16 char: 픽셀이 가리키는 셀로 charno 보정(렌더러 cidx; bpp>>2 = 셀당 워드 stride).
  if (CharSize) {
    uint32 cidx = (((ix >> 3) ^ hflip) & 0x1) + (((iy >> 2) ^ (vflip << 1)) & 0x2);
    charno = (charno + cidx * (bpp >> 2)) & 0x7FFF;
  }

  // char 데이터 베이스 주소(celly=0). 라인오프셋은 ((celly*bpp)>>1) word를 더함(공식은 note에 동봉).
  uint32 cg_addr_word = (charno << 4) & 0x3FFFF;
  uint32 char_data_byte = cg_addr_word << 1;

  char hi_str[12];
  if (!PNDSize) snprintf(hi_str, sizeof(hi_str), "\"0x%04x\"", pnd1 & 0xFFFF);
  else          snprintf(hi_str, sizeof(hi_str), "null");

  char buf[1700];
  snprintf(buf, sizeof(buf),
    "{\"system\":\"ss\",\"nbg\":%d,"
    "\"input\":{\"x\":%ld,\"y\":%ld},"
    "\"scroll_int\":{\"x\":%u,\"y\":%u},"
    "\"map_coord\":{\"ix\":%u,\"iy\":%u},"
    "\"char_size\":\"%s\",\"bpp\":%u,\"pnd_mode\":\"%s\",\"aux_mode\":%u,"
    "\"supplement\":\"0x%03x\",\"eff_charno_bits\":%u,"
    "\"plane_size\":%u,\"map_offset\":%u,"
    "\"mapidx\":%u,\"planeidx\":%u,\"planeoffs\":%u,\"pageoffs\":%u,\"plane_base_word\":%u,"
    "\"nt_addr\":%u,\"pnt_entry_vram_addr\":%u,"
    "\"raw_pnd_word\":\"0x%04x\",\"raw_pnd_word_hi\":%s,"
    "\"palno\":%u,\"flip\":{\"h\":%s,\"v\":%s},\"special\":{\"spr\":%u,\"scc\":%u},"
    "\"charno_pnt\":%u,\"charno\":%u,"
    "\"cellbytes\":%u,\"cells_per_glyph\":%u,\"glyph_bytes\":%u,"
    "\"char_data_word\":%u,\"char_data_addr\":%u,"
    "\"line_offset_formula\":\"celly=(iy&7)^(vflip?7:0); cg_word=(char_data_word + ((celly*bpp)>>1)) & 0x3FFFF; line_addr=cg_word<<1\","
    "\"note\":\"vdp2vram word big-endian; non-rot NBG; ix=xscr_int+x,iy=yscr_int+y (1:1 zoom + integer "
      "scroll; mosaic/line-scroll/vcell-scroll/zoom not applied); char_data_addr is cell base (celly=0); "
      "game font char-base correction not applied (agent RE)\"}",
    n, sx, sy, (unsigned)L.xscr_int, (unsigned)L.yscr_int, (unsigned)ix, (unsigned)iy,
    CharSize ? "16x16" : "8x8", bpp, PNDSize ? "1word" : "2word", AuxMode,
    Supp & 0x3FF, L.eff_charno_bits,
    PlaneSize, L.mapoff,
    (unsigned)mapidx, (unsigned)planeidx, (unsigned)planeoffs, (unsigned)pageoffs, (unsigned)L.pbw[mapidx],
    (unsigned)nt_addr, (unsigned)pnt_byte,
    pnd0 & 0xFFFFu, hi_str,
    palno, hflip ? "true" : "false", vflip ? "true" : "false", spr, scc,
    charno_pnt, charno,
    L.bytes_per_cell, L.cells_per_glyph, L.glyph_bytes,
    (unsigned)cg_addr_word, (unsigned)char_data_byte);
  reply_ok(id, std::string(buf));
}

// set_layer_enable(layers?|mask?): Mednafen 내장 레이어 enable 마스크를 노출한다(비파괴 VDP1/VDP2 라우팅
// 확정·클린플레이트용 — 파괴적 VRAM diff 우회 불필요). MDFNGameInfo->LayerNames(null-구분 이름; 순서=비트
// 0..N, SS=NBG0..3/RBG0/1/Sprite)를 파싱해 이름↔비트를 매핑한다. layers(이름 배열, 대소문자 무시 → 그
// 비트만 set·나머지 clear) 또는 mask(raw uint)로 마스크를 조립해 MDFNI_SetLayerEnableMask로 적용한다. 둘
// 다 생략 시 적용 없이 조회만(코어에 getter 부재 → 섀도 g_layer_enable_mask 반환). LayerNames 없는
// 시스템(PSX)은 unsupported, 알 수 없는 layer 이름은 조용히 무시하지 않고 bad_params. 반환
// {layer_names, mask, enabled:[이름]}. 마스크는 디버그 override라 바꿀 때까지 유지(지속성 안내는 tool에).
void handle_set_layer_enable(long id, const std::string& line) {
  if (!MDFNGameInfo || !MDFNGameInfo->LayerNames) {
    reply_err(id, "unsupported", "layer toggling is unavailable for this system");
    return;
  }
  // LayerNames는 null-구분 문자열 목록이며 빈 문자열(이중 null)로 끝난다(드라이버 gfxdebugger.cpp 동형).
  // 인덱스 = 비트 위치(렌더러 UserLayerEnableMask 비트순).
  std::vector<std::string> names;
  {
    const char* lnp = MDFNGameInfo->LayerNames;
    size_t clen;
    while ((clen = strlen(lnp)) != 0) {
      names.push_back(std::string(lnp, clen));
      lnp += clen + 1;
    }
  }

  // 적용 마스크 결정. 우선순위: layers > mask > (조회만). layers 키 유무로 조회/적용을 가른다.
  uint64_t mask = 0;
  bool do_apply = false;
  if (line.find("\"layers\"") != std::string::npos) {
    size_t b = line.find("\"layers\"");
    size_t lb = line.find('[', b);
    size_t rb = (lb == std::string::npos) ? std::string::npos : line.find(']', lb);
    if (lb == std::string::npos || rb == std::string::npos) {
      reply_err(id, "bad_params", "layers must be an array of strings");
      return;
    }
    std::string arr = line.substr(lb + 1, rb - lb - 1);
    // 배열 안의 각 따옴표 토큰을 이름↔비트로 매핑(대소문자 무시). 알 수 없는 이름은 bad_params.
    size_t pos = 0;
    int matched = 0;
    while (true) {
      size_t q1 = arr.find('"', pos);
      if (q1 == std::string::npos) break;
      size_t q2 = arr.find('"', q1 + 1);
      if (q2 == std::string::npos) break;
      std::string want = arr.substr(q1 + 1, q2 - q1 - 1);
      pos = q2 + 1;
      int bit = -1;
      for (size_t i = 0; i < names.size(); i++) {
        if (names[i].size() == want.size() && !strcasecmp(names[i].c_str(), want.c_str())) {
          bit = (int)i;
          break;
        }
      }
      if (bit < 0) {
        reply_err(id, "bad_params", ("unknown layer name: " + want).c_str());
        return;
      }
      mask |= (1ULL << bit);
      matched++;
    }
    // 빈 layers 배열은 *조회*로 본다(전부 disable은 명시 mask:0). 빈 배열로 화면이 통째 꺼지는 사고를
    // 막고, Rust 프록시가 빈 배열을 omit과 동일 처리하는 계약과도 일치시킨다.
    do_apply = (matched > 0);
  } else {
    std::uint64_t raw = 0;
    const EmucapJsonNumberStatus mask_status = emucap_json_u64(line, "mask", raw);
    if (mask_status == EmucapJsonNumberStatus::invalid) {
      reply_err(id, "bad_params", "mask must be an unsigned 64-bit integer");
      return;
    }
    if (mask_status == EmucapJsonNumberStatus::valid) {
      mask = (uint64_t)raw;
      do_apply = true;
    }
  }

  if (do_apply) {
    g_layer_enable_mask = mask;
    MDFNI_SetLayerEnableMask(mask);
  }

  // 이름 있는 레이어 비트만 의미가 있으니(나머지 비트는 레이어 없음) 보고는 available 비트로 마스킹한다.
  uint64_t avail = (names.size() >= 64) ? ~0ULL : ((1ULL << names.size()) - 1);
  uint64_t report = g_layer_enable_mask & avail;

  std::string resp = "{\"system\":\"";
  resp += json_escape(system_shortname());
  resp += "\",\"layer_names\":[";
  for (size_t i = 0; i < names.size(); i++) {
    if (i) resp += ",";
    resp += "\"";
    resp += json_escape(names[i]);
    resp += "\"";
  }
  char nb[24];
  snprintf(nb, sizeof(nb), "%llu", (unsigned long long)report);
  resp += "],\"mask\":";
  resp += nb;
  resp += ",\"enabled\":[";
  bool first = true;
  for (size_t i = 0; i < names.size(); i++) {
    if ((report >> i) & 1ULL) {
      if (!first) resp += ",";
      resp += "\"";
      resp += json_escape(names[i]);
      resp += "\"";
      first = false;
    }
  }
  resp += "]}";
  reply_ok(id, resp);
}

// get_rom_info: 콘텐츠 신원을 반환한다(PC-98 bridge get_rom_info 미러 + Mednafen 콘텐츠 해시).
//  - name/path/size/media_type: EMUCAP_CONTENT 파일(launch.sh가 export, hello 신원과 동일 출처).
//  - content_md5: MDFNGameInfo->MD5(16B) hex. Mednafen이 계산하는 경로 독립 레이아웃 식별자 —
//    CD는 CalcDiscsLayoutMD5(TOC·트랙·LBA 기반, *경로 독립·디스크 레이아웃 인지*), Saturn은
//    CalcGameID, 카트리지는 로더가 ROM 데이터로 채운다. 같은 디스크/ROM은 경로·파일명과 무관하게
//    같은 값. CUE 트랙 바이트는 포함하지 않으므로 추적 입력 식별자로 사용하지 않는다.
//  - sha1: sha1(EMUCAP_CONTENT 파일 바이트). 보조 — 단일파일 ROM/.chd엔 정확, .cue는 디스크립터-only
//    (참조하는 .bin 미포함)라 충돌 가능. 컨트랙트 일관성(Mesen/PC-98이 sha1 반환) 위해 유지.
// EMUCAP_CONTENT 미설정 → unsupported, MDFNGameInfo null(게임 미로드) → bad_state 에러.
void handle_get_rom_info(long id) {
  if (!MDFNGameInfo) {
    reply_err(id, "bad_state", "MDFNGameInfo is not initialized; no game is loaded");
    return;
  }
  const char* content = getenv("EMUCAP_CONTENT");
  if (!content || !content[0]) {
    reply_err(id, "unsupported", "EMUCAP_CONTENT is unset; content identity is unavailable");
    return;
  }
  std::string path(content);

  // name = basename, media_type = 확장자(소문자, 점 제거).
  std::string name = path;
  size_t slash = name.find_last_of('/');
  if (slash != std::string::npos) name = name.substr(slash + 1);
  std::string media_type;
  size_t dot = name.find_last_of('.');
  if (dot != std::string::npos && dot + 1 < name.size()) {
    media_type = name.substr(dot + 1);
    for (char& c : media_type)
      if (c >= 'A' && c <= 'Z') c += 32;  // locale-독립 소문자화
  }

  // size + sha1: EMUCAP_CONTENT 파일 바이트를 읽어 Mednafen sha1(one-shot). 파일 없음/IO 실패는 에러.
  // sha1 API가 one-shot(스트리밍은 상류 #if 0)이라 파일을 통째 읽으므로, 대형 단일파일 디스크
  // 이미지(.chd/.iso/.bin 직접 지정)의 메모리 스파이크를 막기 위해 64MB 초과면 sha1을 건너뛴다(보조
  // 필드일 뿐이다. CUE의 정확한 입력 그래프는 Control MCP가 launch 전에 별도로 결속한다.
  // 건너뛰면 sha1="skipped:too_large".
  static const uint64 kSha1MaxBytes = 64ull * 1024 * 1024;
  uint64 size = 0;
  std::string sha1hex;
  try {
    FileStream fs(path, FileStream::MODE_READ);
    size = fs.size();
    if (size > kSha1MaxBytes) {
      sha1hex = "skipped:too_large";
    } else {
      std::vector<uint8> buf(size);
      if (size) fs.read(buf.data(), size);
      uint8 dummy = 0;  // 빈 파일에서 buf.data()가 null일 수 있어 유효 포인터 보장
      sha1_digest dg = sha1(size ? (const void*)buf.data() : (const void*)&dummy, size);
      sha1hex = hex_bytes(dg.data(), dg.size());
    }
  } catch (std::exception& e) {
    reply_err(id, "io_error", e.what());
    return;
  }

  std::string content_md5 = hex_bytes(MDFNGameInfo->MD5, sizeof(MDFNGameInfo->MD5));

  std::string resp = "{\"system\":\"";
  resp += json_escape(system_shortname());
  resp += "\",\"adapter\":\"mednafen\",\"name\":\"";
  resp += json_escape(name);
  resp += "\",\"path\":\"";
  resp += json_escape(path);
  resp += "\",\"size\":";
  resp += std::to_string((unsigned long long)size);
  resp += ",\"media_type\":\"";
  resp += json_escape(media_type);
  resp += "\",\"content_md5\":\"";
  resp += content_md5;
  resp += "\",\"sha1\":\"";
  resp += sha1hex;
  resp += "\"}";
  reply_ok(id, resp);
}

// set_trace(enabled): 실행추적 켜기/끄기. 켜면 continuous cb가 매 명령 PC를 링버퍼에 기록한다(hunting 전용,
// 매 명령이라 느림 — 끝나면 끈다). Debugger 필요(SetCPUCallback).
void handle_set_trace(long id, const std::string& line) {
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->SetCPUCallback) {
    reply_err(id, "no_debugger", "debugger is not initialized; set_trace is unavailable");
    return;
  }
  bool enabled = false;
  json_bool(line, "enabled", enabled);
  g_trace_enabled = enabled;
  if (enabled) {
    g_trace_ring.assign(TRACE_CAP, 0);  // 링버퍼 확보·초기화
    g_trace_head = 0;
    g_trace_count = 0;
    g_callstack.clear();  // shadow stack도 새로 시작(추적 시작 이후의 call/return만 반영)
    resolve_sp_reg();     // SP 레지스터 이름 1회 해소(콜스택 SP-기반 반환 감지용)
  }
  rearm_breakpoints();  // continuous 재계산(trace 활성→cb 무장, 비활성→다른 도구 없으면 해제)
  reply_ok(id, std::string("{\"enabled\":") + (enabled ? "true" : "false") + "}");
}

// get_trace(count): 최근 count개(기본 256) 실행 명령을 시간순(오래된→최근) [{pc,text}]로 반환. set_trace(true) 선행.
void handle_get_trace(long id, const std::string& line) {
  long count = 256;
  json_num(line, "count", count);
  if (count < 1) count = 1;
  size_t want = (size_t)count;
  if (want > g_trace_count) want = g_trace_count;
  std::string out = "{\"trace\":[";
  for (size_t i = 0; i < want; i++) {
    // 최근 want개: 링에서 (head-want)..(head-1) 순서. head는 다음 쓸 위치.
    size_t idx = (g_trace_head + TRACE_CAP - want + i) % TRACE_CAP;
    uint32 pc = g_trace_ring[idx];
    std::string text;
    if (CurGame && CurGame->Debugger && CurGame->Debugger->Disassemble) {
      uint32 A = pc;
      char tbuf[256];
      tbuf[0] = 0;
      CurGame->Debugger->Disassemble(A, A, tbuf);  // pc 1명령 디스어셈
      text = tbuf;
    }
    char pcbuf[40];
    snprintf(pcbuf, sizeof(pcbuf), "%s{\"pc\":%u,\"text\":\"", i ? "," : "", (unsigned)pc);
    out += pcbuf;
    out += json_escape(text);
    out += "\"}";
  }
  out += "]}";
  reply_ok(id, out);
}

// watch_register(register, min, max, pause_on_hit): register가 [min,max]를 벗어나는 명령에서 freeze한다
// (SP 폭주 등 derail 포착). register 이름은 get_state의 "name"/"group.name". 매 명령 검사라 hunting 전용.
void handle_watch_register(long id, const std::string& line) {
  if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->RegGroups ||
      !CurGame->Debugger->SetCPUCallback) {
    reply_err(id, "no_debugger", "debugger is not initialized; watch_register is unavailable");
    return;
  }
  std::string reg = json_str(line, "register");
  if (reg.empty()) {
    reply_err(id, "bad_params", "register is required");
    return;
  }
  uint32 probe;
  if (!read_register_by_name(reg, probe)) {
    std::string m =
        "register '" + reg + "' was not found; read valid names from get_state (name or group.name)";
    reply_err(id, "bad_params", m.c_str());
    return;
  }
  uint32 mn = 0, mx = 0;
  if (!json_u32_arg(id, line, "min", mn, true)
      || !json_u32_arg(id, line, "max", mx, true))
    return;
  bool pause = true;
  json_bool(line, "pause_on_hit", pause);
  g_watch_reg = reg;
  g_watch_min = mn;
  g_watch_max = mx;
  g_watch_pause = pause;
  g_watch_enabled = true;
  rearm_breakpoints();
  char buf[128];
  snprintf(buf, sizeof(buf), "{\"watching\":\"%s\",\"min\":%u,\"max\":%u}", json_escape(reg).c_str(),
           (unsigned)g_watch_min, (unsigned)g_watch_max);
  reply_ok(id, buf);
}

// call_stack(): 현재 shadow stack(call-site PC 체인, 바깥→안)을 [{pc,text}]로 반환한다. set_trace(true)
// 선행 필요 — 추적 시작 이후의 call/return만 반영하며 스택 메모리 손상과 독립적이다.
void handle_call_stack(long id) {
  std::string out = "{\"call_stack\":[";
  for (size_t i = 0; i < g_callstack.size(); i++) {
    uint32 pc = g_callstack[i].pc;  // g_callstack[0]=가장 바깥, back()=가장 안
    std::string text;
    if (CurGame && CurGame->Debugger && CurGame->Debugger->Disassemble) {
      uint32 A = pc;
      char tbuf[256];
      tbuf[0] = 0;
      CurGame->Debugger->Disassemble(A, A, tbuf);
      text = tbuf;
    }
    char pcbuf[40];
    snprintf(pcbuf, sizeof(pcbuf), "%s{\"pc\":%u,\"text\":\"", i ? "," : "", (unsigned)pc);
    out += pcbuf;
    out += json_escape(text);
    out += "\"}";
  }
  out += "]}";
  reply_ok(id, out);
}

// Reset entry address: MD reads a big-endian longword at $4, PCE reads a little-endian word at
// $FFFE, and WonderSwan uses the 20-bit physical form of reset CS:IP FFFF:0000 (0xFFFF0).
uint32 read_reset_entry() {
  // WonderSwan has a fixed architectural reset vector and does not need a named
  // Mednafen address space. Check it before the MD/PCE address-space lookup.
  if (is_ws()) return 0xFFFF0u;
  AddressSpaceType* sp = find_aspace("cpu");
  if (!sp) return 0;
  if (is_md()) {
    uint8 b[4] = {0, 0, 0, 0};
    sp->GetAddressSpaceBytes("cpu", 0x4, 4, b);
    return ((uint32)b[0] << 24) | ((uint32)b[1] << 16) | ((uint32)b[2] << 8) | b[3];
  }
  if (is_pce()) {
    uint8 b[2] = {0, 0};
    sp->GetAddressSpaceBytes("cpu", 0xFFFE, 2, b);
    return (uint32)b[0] | ((uint32)b[1] << 8);
  }
  return 0;
}

// break_on_reset(enabled): 게임이 리셋 진입을 실행하면 freeze한다(source="reset"). 카트리지 MD/PCE/WS 전용 —
// 디스크(SS/PSX)는 "리셋"=BIOS 부팅이라 개념이 안 맞아 미advertise·거부(exec BP를 BIOS 엔트리에 거는 대체).
void handle_break_on_reset(long id, const std::string& line) {
  if (!is_md() && !is_pce() && !is_ws()) {
    reply_err(id, "unsupported",
              "break_on_reset is available only for cartridge systems; use an exec breakpoint at the BIOS entry for disc systems");
    return;
  }
  bool enabled = false;
  json_bool(line, "enabled", enabled);
  g_break_on_reset = enabled;
  if (enabled) g_reset_entry = read_reset_entry();
  rearm_breakpoints();
  char buf[64];
  snprintf(buf, sizeof(buf), "{\"enabled\":%s,\"reset_entry\":%u}", enabled ? "true" : "false",
           (unsigned)g_reset_entry);
  reply_ok(id, buf);
}

void handle(const std::string& line) {
  std::string method = json_str(line, "method");
  long id = 0;
  json_num(line, "id", id);  // 봉투 id가 params보다 앞이라 첫 "id"가 봉투 id

  // 어떤 핸들러 예외(std::bad_alloc 등)도 프레임 루프 밖으로 탈출시키지 않고 reply_err로
  // 변환한다 — 안 그러면 std::terminate로 에뮬레이터 프로세스가 죽는다.
  try {
  if (method == "abort_recording") {
    handle_abort_recording(id, line);
    return;
  }
  if (g_recording) {
    reply_err(id, "busy", "record_window owns guest progress until its terminal response");
    return;
  }
  if (method == "hello") {
    const char* sys = system_shortname();
    const bool has_debugger = CurGame && CurGame->Debugger;
    // 강등(Debugger 부재, 예: pce_fast)이면 memory/BP/probe/disassemble 계열은 실제로 no_debugger로
    // 거부되니 광고에서 뺀다 — methods가 런타임 현실을 말하게 한다.
    // get_state도 Debugger->RegGroups 의존이라(handle_get_state가 없으면 no_debugger 거부) 강등 시
    // base에서 빼고 debugger 블록에 넣는다 — 광고가 런타임 현실과 일치하게(틀린 것 주장 금지).
    std::string methods =
        "\"hello\",\"status\",\"save_state\",\"load_state\",\"run_frames\","
        "\"set_input\",\"press_buttons\",\"screenshot\",\"pause\",\"resume\",\"step\",\"reset\"";
    if (has_debugger) {
      // step_instructions는 Debugger->SetCPUCallback(continuous) 의존이라 debugger 있을 때만 advertise.
      methods +=
          ",\"get_state\",\"read_memory\",\"find_pattern\",\"dump_memory\",\"write_memory\",\"probe\","
          "\"set_breakpoint\",\"clear_breakpoint\",\"clear_all_breakpoints\",\"list_breakpoints\","
          "\"poll_events\",\"disassemble\",\"step_instructions\",\"watch_register\"";
      // NGP exposes TLCS execution observation only. Without TLCS call classification and a
      // sound-Z80 clock, generic trace and call-stack results would mix incompatible meanings.
      if (!is_ngp()) methods += ",\"set_trace\",\"get_trace\",\"call_stack\"";
    }
    // Saturn 전용 VDP2 디코드 메서드. SS일 때만 advertise(다른 시스템엔 미advertise — 발견 표면 최소화).
    // PeekRawReg는 ss 코어 심볼이라 ss 외엔 의미 없음. has_debugger와 함께 조건을 확인한다.
    // break_on_reset: 카트리지(MD/PCE/WS)만 리셋 벡터가 있어 advertise한다 — 디스크(SS/PSX)는 "리셋"이 BIOS
    // 부팅이라 개념이 안 맞으므로 미advertise(status.methods가 현실 반영 — "보이는데 안 됨" 없음).
    if (has_debugger && (is_md() || is_pce() || is_ws())) {
      methods += ",\"break_on_reset\"";
    }
    if (has_debugger && is_ss()) {
      methods += ",\"get_video_state\",\"resolve_tile\"";
    }
    // 레이어 토글: LayerNames가 있는 시스템(SS/MD/PCE)만 advertise. 디버거 불요(비디오 enable 마스크 API).
    // PSX는 LayerNames 없음(단일 프레임버퍼) → 미advertise.
    if (MDFNGameInfo && MDFNGameInfo->LayerNames) {
      methods += ",\"set_layer_enable\"";
    }
    // get_rom_info: 콘텐츠 신원(MDFNGameInfo->MD5 해시 + EMUCAP_CONTENT 파일). 디버거 불요 —
    // 게임 로드(MDFNGameInfo 있음) 시 항상 가능하므로 그때만 advertise → status.methods에 노출.
    if (MDFNGameInfo) {
      methods += ",\"get_rom_info\"";
    }
    if (adapter_exception_test_enabled()) {
      methods += ",\"test_adapter_exception\"";
    }
    if (recording_identity_available()) {
      methods += ",\"record_window\"";
    }
    const char* repeatability_conditions = recording_repeatable_conditions();
    // memory_types: 이 게임의 debugger address space 이름들(없으면 빈 배열). read/write_memory의
    // 유효한 memory_type 목록이며, MCP가 status.memory_types로 표면화한다. 정적 추측 아님.
    std::string mtypes;
    if (has_debugger && CurGame->Debugger->AddressSpaces) {
      for (auto& as : *CurGame->Debugger->AddressSpaces) {
        if (!mtypes.empty()) mtypes += ",";
        mtypes += "\"" + json_escape(as.name) + "\"";
      }
    }
    std::string exception_ids;
    auto add_exception = [&](const char* value) {
      if (!exception_ids.empty()) exception_ids += ",";
      exception_ids += "\"" + std::string(value) + "\"";
    };
    add_exception("mednafen.input-hold.port-zero-only");
    add_exception("mednafen.input-pulse.port-zero-only");
    if (has_debugger) {
      if (!is_ngp()) add_exception("mednafen.call-stack.best-effort");
      if (is_md()) {
        add_exception("mednafen.md.cpu-write-absent");
      }
      if (is_pcfx()) {
        add_exception("mednafen.pcfx.protected-write-spaces");
      }
      if (is_ngp()) {
        add_exception("mednafen.ngp.protected-write-spaces");
      }
      if (is_ss()) {
        add_exception("mednafen.ss.physical-read-absent");
        add_exception("mednafen.ss.physical-write-absent");
      }
    } else if (is_pce()) {
      add_exception("mednafen.pce-fast.debugger-absent");
    } else if (is_ngp()) {
      add_exception("mednafen.ngp.debugger-absent");
    }
    std::string contracts =
        "{\"catalog\":\"emucap-feature-contracts/v3\",\"active_exceptions\":[" +
        exception_ids + "]}";
    const char* snapshot_capability = is_pcfx() ? "true" : "false";
    const std::string breakpoint_kinds = !has_debugger
        ? "[]"
        : is_ngp()
            ? "[{\"kind\":\"exec\",\"range_unit\":\"address\",\"range_mode\":\"inclusive\",\"memory_type_used\":true,\"snapshot\":false,\"snapshot_timing\":\"backend_stop\"}]"
            : "[{\"kind\":\"exec\",\"range_unit\":\"address\",\"range_mode\":\"inclusive\",\"memory_type_used\":true,\"snapshot\":" +
              std::string(snapshot_capability) + ",\"snapshot_timing\":\"backend_stop\"},"
              "{\"kind\":\"read\",\"range_unit\":\"address\",\"range_mode\":\"inclusive\",\"memory_type_used\":true,\"snapshot\":" +
              snapshot_capability + ",\"snapshot_timing\":\"backend_stop\"},"
              "{\"kind\":\"write\",\"range_unit\":\"address\",\"range_mode\":\"inclusive\",\"memory_type_used\":true,\"snapshot\":" +
              snapshot_capability + ",\"snapshot_timing\":\"backend_stop\"}]";
    char head[224];
    snprintf(head, sizeof(head),
             "{\"protocol_version\":%d,\"system\":\"%s\",\"adapter\":\"mednafen\",\"build\":\"%s\","
             "\"debugger\":%s,",
             PROTOCOL_VERSION, sys, EMUCAP_BUILD_HASH, has_debugger ? "true" : "false");
    std::string host_features = "[\"controlled_start\"";
    if (repeatability_conditions) host_features += ",\"repeatable_recording\"";
    host_features += "]";
    std::string hello_resp = std::string(head) +
                             "\"host_features\":" + host_features + ",\"methods\":[" + methods +
                             "],\"memory_types\":[" + mtypes + "],\"state_groups\":" +
                             (has_debugger ? register_group_names_json() : "[]") +
                             ",\"cpu_targets\":" + (has_debugger ? cpu_targets_json() : "[]") +
                             ",\"breakpoint_kinds\":" +
                             breakpoint_kinds + ",\"contracts\":" +
                             contracts + ",\"execution_limits\":{\"max_sync_advance_count\":" +
                             std::to_string(MAX_SYNC_ADVANCE) + "}}";
    if (recording_identity_available()) {
      const char* binary_sha256 = getenv("EMUCAP_MEDNAFEN_BINARY_SHA256");
      hello_resp.pop_back();
      hello_resp += ",\"recording\":" +
          emucap_recording_capability_json(
              recording_reset_release_available(), repeatability_conditions) +
          ",\"host_build\":{\"upstream\":\"" +
          json_escape(EMUCAP_MEDNAFEN_UPSTREAM) + "\",\"commit\":\"" +
          json_escape(EMUCAP_MEDNAFEN_UPSTREAM_REVISION) +
          "\",\"patchset_sha256\":\"" + EMUCAP_MEDNAFEN_PATCHSET_SHA256 +
          "\",\"binary_sha256\":\"" + binary_sha256 + "\"}}";
    }
    // broker 등록용 name(EMUCAP_NAME 설정 시 포함, 직접 모드는 무시됨).
    const char* emu_name = getenv("EMUCAP_NAME");
    const char* session_token = getenv("EMUCAP_SESSION_TOKEN");
    const char* content = getenv("EMUCAP_CONTENT");
    const char* launch_id = getenv("EMUCAP_LAUNCH_ID");
    std::string resp = std::move(hello_resp);
    if (emu_name && emu_name[0]) {
      // 닫는 } 전에 ,"name":"..."를 삽입한다. JSON 이스케이프 + std::string 조립으로
      // 특수문자·길이 초과(strncat 잘림)에 의한 깨진 JSON을 막는다.
      if (!resp.empty() && resp.back() == '}') {
        std::string nm(emu_name);
        if (nm.size() > 128) nm.resize(128);  // 과한 길이 방지
        resp.pop_back();
        resp += ",\"name\":\"" + json_escape(nm) + "\"}";
      }
    }
    if (session_token && session_token[0] && !resp.empty() && resp.back() == '}') {
      std::string tok(session_token);
      if (tok.size() > 256) tok.resize(256);
      resp.pop_back();
      resp += ",\"session_token\":\"" + json_escape(tok) + "\"}";
    }
    if (content && content[0] && !resp.empty() && resp.back() == '}') {
      std::string c(content);
      if (c.size() > 512) c.resize(512);
      resp.pop_back();
      resp += ",\"content\":\"" + json_escape(c) + "\"}";
    }
    if (launch_id && launch_id[0] && !resp.empty() && resp.back() == '}') {
      std::string value(launch_id);
      if (value.size() > 128) value.resize(128);
      resp.pop_back();
      resp += ",\"launch_id\":\"" + json_escape(value) + "\"}";
    }
    reply_ok(id, resp);
  } else if (method == "status") {
    // last_game_input: 게임 스레드가 마지막으로 읽은 입력 버퍼 비트(주입이 실제 도달했는지 확인).
    uint16_t g = g_last_game_data.load();
    const char* sys = system_shortname();
    const bool has_debugger = CurGame && CurGame->Debugger;
    char buf[512];
    snprintf(buf, sizeof(buf),
             "{\"connected\":true,\"system\":\"%s\",\"debugger\":%s,\"frame\":%llu,\"state\":\"%s\","
             "\"last_game_input\":\"0x%04x\",\"last_game_buttons\":%s,"
             "\"input_override\":{\"observable\":true,\"engaged\":%s,\"mode\":\"%s\","
             "\"pressed_mask\":%u},\"execution_limits\":{\"max_sync_advance_count\":%ld}}",
             sys, has_debugger ? "true" : "false",
             (unsigned long long)g_frame, g_frozen ? "frozen" : "running",
             (unsigned)g, mask_to_buttons(g).c_str(),
             g_input_override.engaged() ? "true" : "false",
             g_def_is_press ? "timed" : (g_input_override.engaged() ? "persistent" : "native"),
             (unsigned)g_input_override.mask(), MAX_SYNC_ADVANCE);
    std::string resp(buf);
    if (has_debugger) {
      resp.pop_back();
      resp += ",\"state_groups\":" + register_group_names_json() +
              ",\"cpu_targets\":" + cpu_targets_json() + "}";
    }
    if (recording_identity_available()) {
      resp.pop_back();
      resp += ",\"recording\":{\"capability_revision\":\"" +
          std::string(emucap_recording_capability_revision(
              recording_reset_release_available(),
              recording_repeatable_conditions() != nullptr)) +
          "\",\"active\":" + (g_recording ? "true" : "false") +
          ",\"capture_id\":" +
          (g_recording ? "\"" + json_escape(g_recording->capture_id()) + "\"" : "null") +
          ",\"frame\":" + std::to_string(g_frame) +
          ",\"last\":" + recording_last_json() + "}" + "}";
    }
    if (is_ss()) {
      uint32_t a = g_last_smpc_read_addr.load();
      uint32_t v = g_last_smpc_read_value.load();
      char abuf[8] = {0}, vbuf[8] = {0};
      snprintf(abuf, sizeof(abuf), "0x%02x", (unsigned)(a & 0x3F));
      snprintf(vbuf, sizeof(vbuf), "0x%02x", (unsigned)(v & 0xFF));
      resp.pop_back();
      resp += ",\"last_smpc_read_addr\":";
      resp += (a == 0xFFFFFFFFu) ? "null" : ("\"" + std::string(abuf) + "\"");
      resp += ",\"last_smpc_read_value\":";
      resp += (a == 0xFFFFFFFFu) ? "null" : ("\"" + std::string(vbuf) + "\"");
      char mbuf[96];
      snprintf(mbuf, sizeof(mbuf), ",\"smpc_read_count\":%u,\"smpc_read_mask\":\"0x%016llx\",\"last_smpc_oreg\":\"",
               (unsigned)g_smpc_read_count.load(),
               (unsigned long long)g_smpc_read_mask.load());
      resp += mbuf;
      resp += hex_bytes(g_last_smpc_oreg, sizeof(g_last_smpc_oreg));
      resp += "\"}";
    }
    if (g_internal_failure_active) {
      resp.pop_back();
      resp += ",\"adapter_failure_active\":true,\"adapter_failure_operation\":\"";
      resp += json_escape(g_internal_failure_operation);
      resp += "\"}";
    }
    resp.pop_back();
    resp += ",\"launch_start\":{\"requested_frozen\":";
    resp += start_frozen_requested() ? "true" : "false";
    resp += ",\"controlled\":";
    resp += g_launch_start_controlled ? "true" : "false";
    resp += ",\"boundary\":";
    resp += g_launch_start_controlled ? "\"pre_first_instruction\"" : "null";
    resp += "}}";
    reply_ok(id, resp);
    recover_native_failure_after_status();
  } else if (method == "get_rom_info") {
    handle_get_rom_info(id);
  } else if (
      method == "test_adapter_exception" && adapter_exception_test_enabled()) {
    g_test_adapter_exception_id = id;
  } else if (method == "record_window") {
    handle_record_window(id, line);
  } else if (method == "read_memory") {
    handle_read_memory(id, line);
  } else if (method == "find_pattern") {
    handle_find_pattern(id, line);
  } else if (method == "dump_memory") {
    handle_dump_memory(id, line);
  } else if (method == "write_memory") {
    handle_write_memory(id, line);
  } else if (method == "get_state") {
    handle_get_state(id, line);
  } else if (method == "get_video_state") {
    handle_get_video_state(id);
  } else if (method == "resolve_tile") {
    handle_resolve_tile(id, line);
  } else if (method == "set_layer_enable") {
    handle_set_layer_enable(id, line);
  } else if (method == "save_state") {
    handle_save_state(id, line);
  } else if (method == "load_state") {
    handle_load_state(id, line);
  } else if (method == "run_frames") {
    long n = 1;
    json_num(line, "n", n);
    if (!normalize_sync_advance(id, n, false)) return;
    // 어댑터에서 직접 resume한다 — Rust ensure_running이 먼저 resume해도, BP가 메인루프에 있으면 resume 직후
    // 재히트→재freeze되고 그 사이 도착한 run_frames는 게임스레드가 freeze_spin에 park된 채라 g_def가 진행
    // 안 돼 timeout이 났다(freeze_spin 탈출조건이 g_step/g_insn만 봐 g_def는 못 깨움). 여기서 g_frozen=false로
    // freeze_spin을 탈출시켜 g_def가 진행(BP 히트 시 flush가 interrupted 반환).
    g_frozen = false;
    g_def_id = id;             // 지연: N프레임 후 emucap_service가 완료 응답(여기선 응답 안 함)
    g_def_remaining = n;
    g_def_progress_ms = monotonic_millis();
    g_def_is_press = false;
  } else if (method == "set_input") {
    long port = 0;
    if (json_num(line, "port", port) && port != 0) {
      reply_err(id, "bad_params", "Mednafen input supports only controller port 0");
      return;
    }
    uint16_t m = 0;
    std::string input_err;
    if (!buttons_to_mask(line, m, input_err)) { reply_err(id, "bad_params", input_err.c_str()); return; }
    if (m == 0) g_input_override.release();
    else g_input_override.engage(m);
    reset_input_diagnostics();             // 이번 주입 이후 latch/read 진단만 모은다
    // 응답에 적용된 비트마스크·버튼명을 echo한다(보낸 버튼 ↔ 실제 비트 불일치를 즉시 확인).
    char rbuf[256];
    snprintf(rbuf, sizeof(rbuf), "{\"status\":\"ok\",\"applied_mask\":\"0x%04x\",\"applied_buttons\":%s}",
             (unsigned)m, mask_to_buttons(m).c_str());
    reply_ok(id, rbuf);
  } else if (method == "press_buttons") {
    long port = 0;
    if (json_num(line, "port", port) && port != 0) {
      reply_err(id, "bad_params", "Mednafen input supports only controller port 0");
      return;
    }
    long frames = 1;
    json_num(line, "frames", frames);
    if (!normalize_sync_advance(id, frames, false)) return;
    uint16_t m = 0;
    std::string input_err;
    if (!buttons_to_mask(line, m, input_err)) { reply_err(id, "bad_params", input_err.c_str()); return; }
    g_input_override.engage(m);
    reset_input_diagnostics(); // 이번 주입 이후 latch/read 진단만 모은다
    g_frozen = false;          // run_frames와 동일: 어댑터에서 직접 resume(재freeze 레이스로 g_def가 freeze_spin에 갇히는 timeout 방지)
    g_def_id = id;             // 지연: N프레임 누른 뒤 완료 응답 + 입력 해제(emucap_service)
    g_def_remaining = frames;
    g_def_progress_ms = monotonic_millis();
    g_def_is_press = true;
  } else if (method == "pause") {
    g_frozen = true;           // 다음 프레임부터 emucap_service가 스핀
    // via_cb는 핸들러가 정하지 않는다 — park 위치가 정함. pause-from-running은 emucap_service park가
    // false로, pause-while-BP-frozen은 게임스레드가 freeze_spin에 남아 true 보존(회귀 방지).
    reply_ok(id, "{\"state\":\"frozen\"}");
  } else if (method == "resume") {
    g_frozen = false;
    g_step_remaining = 0;
    g_step_id = -1;
    g_step_progress_ms = 0;
    // 명령단위 step 진행/정지 중이었으면 continuous 콜백을 해제해 fast path로 복귀(perf — 매 명령 cb).
    g_insn_remaining = 0;
    g_insn_step_id = -1;
    g_insn_progress_ms = 0;
    g_insn_skip_first = false;
    if (g_insn_armed) rearm_breakpoints();   // continuous 해제(BP만 있으면 BP 모드로, 없으면 콜백 해제)
    reply_ok(id, "{\"state\":\"running\"}");
  } else if (method == "step" || method == "step_instructions") {
    // 공개 step(unit="instructions")은 wire step_instructions로 들어온다. 이전 호스트의
    // step{unit:"instructions",frames:N}도 계속 받아 같은 상태머신으로 처리한다.
    std::string unit = json_str(line, "unit");
    if (method == "step_instructions") unit = "instructions";
    if (unit == "instructions") {
      // frozen(pause/step/BP) 전제 — 정지 지점에서 N명령씩 좁힌다(프레임 step과 달리 running 진입 금지).
      if (!g_frozen) {
        reply_err(id, "not_frozen", "instruction step requires frozen state; call pause first");
        return;
      }
      if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->SetCPUCallback) {
        reply_err(id, "no_debugger", "this core does not expose the CPU callback required for instruction step");
        return;
      }
      long count = 1;
      if (method == "step_instructions")
        json_num(line, "count", count);
      else
        json_num(line, "frames", count);
      if (!normalize_sync_advance(id, count, false)) return;
      g_insn_remaining = count;
      g_insn_step_id = id;               // 완료 응답은 cb가 count 명령 실행 후(지연)
      g_insn_progress_ms = monotonic_millis();
      // cold(콜백 밖) 진입이면 첫 continuous cb를 흡수해 진입명령을 공짜 실행(BP 진입과 동형) → 정확히 N.
      // cb 안 진입(BP 히트/명령단위 연쇄)이면 진입명령이 이미 공짜 실행되므로 skip 안 함.
      g_insn_skip_first = !g_frozen_via_cb;
      rearm_breakpoints();               // g_insn_remaining>0 → continuous 콜백 무장
      // 응답은 지연. 지금 frozen이면 현재 freeze 스핀(cb 내 freeze_spin_until_resume 또는 emucap_service
      // 스핀)이 g_insn_remaining>0 탈출조건으로 빠져나가 진행 → cb가 count 명령 후 재freeze하며 응답.
      return;
    }
    long frames = 1;
    json_num(line, "frames", frames);
    if (!normalize_sync_advance(id, frames, false)) return;
    g_frozen = true;           // 진행 후 재정지
    // via_cb는 핸들러가 정하지 않는다 — frame-step 완료 후 emucap_service frozen park가 false로 설정.
    // 프레임 중 BP가 히트해 cb 안에서 park하면 freeze_spin이 true로(park 위치가 권위).
    g_step_id = id;            // 완료 응답은 emucap_service가 frames 경과 후
    g_step_remaining = frames;
    g_step_progress_ms = monotonic_millis();
  } else if (method == "probe") {
    // probe는 세이브스테이트를 로드해 프레임을 진행시키는 상태-파괴적 측정이다. frozen(pause)
    // frame-boundary park에서는 그대로 수행할 수 있다. CPU callback 안의 instruction-boundary
    // park만 host callback stack과 restored CPU state를 섞을 수 있어 상태를 건드리기 전에 거부한다.
    if (g_frozen && g_frozen_via_cb) {
      reply_err(id, "unsafe_halt", "probe requires a frame-boundary halt; call resume, then pause");
      return;
    }
    std::string probe_mt = json_str(line, "memory_type");
    // Saturn "physical"은 미구현(read=0)이라 타깃 읽기가 조용히 all-zeros를 줘 거짓 probe 결과를 낸다 —
    // read_memory와 동일하게 거부한다(상태-파괴적 savestate 로드/프레임 진행 전에).
    if (reject_ss_physical_read(id, probe_mt)) return;
    std::string path = json_str(line, "state");
    long frames = 0;
    json_num(line, "frame", frames);
    if (!normalize_sync_advance(id, frames, true)) return;
    uint32 probe_addr = 0;
    if (!json_u32_arg(id, line, "address", probe_addr, true)) return;
    long probe_len = 0;
    json_num(line, "length", probe_len);
    if (!validate_aspace_range(probe_mt, probe_addr, probe_len)) {
      reply_err(id, "bad_params", "unknown memory_type or address/length exceeds the region");
      return;
    }
    try {                              // 즉시 복귀(원자적 진입)
      FileStream fs(path, FileStream::MODE_READ);
      MDFNSS_LoadSM(&fs);
      fs.close();
    } catch (std::exception& e) { reply_err(id, "io_error", e.what()); return; }
    if (frames == 0) {
      std::string hex;
      if (!read_aspace_hex(probe_mt, probe_addr, probe_len, hex)) {
        reply_err(id, "bad_params", "memory range became unavailable after state restore");
      } else {
        reply_ok(id, "{\"status\":\"completed\",\"requested_frames\":0,\"completed_frames\":0,\"hex\":\"" + hex + "\"}");
      }
      return;
    }
    g_probe_id = id;                   // 진행·읽기·응답은 emucap_service가(그 사이 새 명령 차단)
    g_probe_requested = frames;
    g_probe_remaining = frames;
    g_probe_progress_ms = monotonic_millis();
    g_probe_mt = probe_mt;
    g_probe_addr = probe_addr;
    g_probe_len = probe_len;
  } else if (method == "set_breakpoint") {
    if (g_bps.size() >= BREAKPOINT_CAP) {
      reply_err(id, "bad_params", "breakpoint limit exceeded");
      return;
    }
    std::string kind = json_str(line, "kind");
    if (kind.empty()) kind = "exec";  // kind 생략 시 기본 exec
    // exec/read/write만 지원한다. nmi/irq/dma 등은 이 디버거에 없다 — 조용히 exec로 처리(silent-wrong,
    // "보이는데 안 됨")하지 않고 supported를 동반해 거부한다.
    if (kind != "exec" && kind != "read" && kind != "write") {
      std::string m = "unsupported breakpoint kind '" + kind + "'; supported: exec, read, write";
      reply_err(id, "unsupported", m.c_str());
      return;
    }
    if (is_ngp() && kind != "exec") {
      reply_err(id, "unsupported",
                "Neo Geo Pocket supports TLCS-900/H exec breakpoints only");
      return;
    }
    std::string mt = json_str(line, "memory_type");
    uint32 start = 0, end = 0;
    if (!json_u32_arg(id, line, "start", start, true)
        || !json_u32_arg(id, line, "end", end, true))
      return;
    if (end < start) end = start;
    const uint32 public_start = start;
    const uint32 public_end = end;
    int type = (kind == "read") ? BPOINT_READ : (kind == "write") ? BPOINT_WRITE : BPOINT_PC;
    bool logical = true;
    bool adapter_bp = false;
    if (is_ngp()) {
      if (!mt.empty() && mt != "cpu") {
        reply_err(id, "unsupported",
                  "Neo Geo Pocket exec breakpoints use memory_type=cpu and absolute 24-bit TLCS addresses");
        return;
      }
      if (start > 0xFFFFFF || end > 0xFFFFFF) {
        reply_err(id, "bad_params",
                  "Neo Geo Pocket TLCS exec breakpoint addresses are 24-bit");
        return;
      }
    } else if (is_psx()) {
      if (type == BPOINT_PC) {
        const uint32 folded_start = fold_psx_exec_address(start);
        const uint32 folded_end = fold_psx_exec_address(end);
        if (folded_end < folded_start) {
          reply_err(id, "bad_params",
                    "PlayStation exec breakpoint range crosses incompatible CPU address mirrors");
          return;
        }
        start = folded_start;
        end = folded_end;
      }
    } else if (is_pce()) {
      // HuC6280 exec BP는 16비트 논리 주소(MPR 뱅킹) — 코어(pce/debug.cpp)가 i<65536만 arm하고 거대 span은
      // O(span) 루프라, MD/SS/WS처럼 범위 밖을 조용히 드롭하지 않고 명확히 거부한다(exec 상한이 DoS도 캡).
      if (type == BPOINT_PC) {
        if (start > 0xFFFF || end > 0xFFFF) {
          reply_err(id, "bad_params",
                    "PCE exec breakpoints use 16-bit logical addresses (0x0000..0xFFFF with MPR banking), not physical or bank addresses");
          return;
        }
      } else if ((type == BPOINT_READ || type == BPOINT_WRITE) && mt == "physical") {
        if (start > 0x1FFFFF || end > 0x1FFFFF) {
          reply_err(id, "bad_params", "PCE physical breakpoints use 21-bit addresses (0x000000..0x1FFFFF)");
          return;
        }
        logical = false;
      } else if ((type == BPOINT_READ || type == BPOINT_WRITE) && (mt == "vram0" || mt == "vram1")) {
        // PCE Debugger AUX BP uses VDC VRAM word addresses, with bit16 selecting VDC-B on SGX.
        // The public memory_type uses byte offsets like read_memory("vram0"), so convert here.
        uint32 vdc = (mt == "vram1") ? 1u : 0u;
        type = (type == BPOINT_READ) ? BPOINT_AUX_READ : BPOINT_AUX_WRITE;
        start = (vdc << 16) | (start >> 1);
        end = (vdc << 16) | (end >> 1);
        logical = true;
      } else if (type == BPOINT_READ || type == BPOINT_WRITE) {
        // 기본 logical(cpu, 16비트) read/write BP: 범위 밖은 GetLastLogicalReadAddr(16비트)와 안 맞아 미발화.
        if (start > 0xFFFF || end > 0xFFFF) {
          reply_err(id, "bad_params", "PCE logical read/write breakpoints use 16-bit addresses; use memory_type=physical for physical addresses");
          return;
        }
      }
    } else if (is_pcfx()) {
      // PC-FX V810 uses 32-bit physical addresses. Exec BP must be an aligned V810 instruction
      // address. Read/write BP accepts raw CPU bus addresses, plus linear RAM/BIOS offset views.
      if (type == BPOINT_PC) {
        if (!mt.empty() && mt != "cpu") {
          reply_err(id, "unsupported", "PC-FX exec BP uses memory_type=cpu and absolute V810 addresses");
          return;
        }
        if ((start & 1) || (end & 1)) {
          reply_err(id, "bad_params", "PC-FX V810 exec BP addresses must be 2-byte aligned");
          return;
        }
      } else if (mt.empty() || mt == "cpu") {
        logical = false;
      } else if (mt == "ram") {
        if (start > 0x1FFFFF || end > 0x1FFFFF) {
          reply_err(id, "bad_params", "PC-FX ram BP offsets are 0x000000..0x1FFFFF");
          return;
        }
        logical = false;  // RAM is linearly mapped at CPU bus 0x00000000.
      } else if (mt == "bios") {
        if (start > 0xFFFFF || end > 0xFFFFF) {
          reply_err(id, "bad_params", "PC-FX bios BP offsets are 0x00000..0xFFFFF");
          return;
        }
        start += 0xFFF00000u;
        end += 0xFFF00000u;
        logical = false;
      } else if (mt == "backup" || mt == "exbackup") {
        reply_err(id, "unsupported",
                  "PC-FX backup BP views are interleaved on the CPU bus; use memory_type=cpu with an exact physical address");
        return;
      } else if (mt == "kram0" || mt == "kram1"
                 || mt == "vdcvram0" || mt == "vdcvram1") {
        EmucapPcfxAuxRange mapped{};
        const EmucapPcfxAuxMapStatus status =
            emucap_pcfx_aux_range(mt, start, end, mapped);
        if (status == EmucapPcfxAuxMapStatus::unaligned) {
          reply_err(id, "bad_params",
                    "PC-FX auxiliary breakpoint ranges must cover complete 16-bit words");
          return;
        }
        if (status != EmucapPcfxAuxMapStatus::mapped) {
          reply_err(id, "bad_params", "PC-FX auxiliary breakpoint range is out of bounds");
          return;
        }
        start = mapped.start;
        end = mapped.end;
        type = type == BPOINT_READ ? BPOINT_AUX_READ : BPOINT_AUX_WRITE;
        logical = true;
      } else {
        reply_err(id, "unsupported",
                  "PC-FX read/write BP supports cpu, ram, bios, kram0/1, and vdcvram0/1; CD track address spaces are read-only debugger views");
        return;
      }
    } else if (is_md()) {
      if (type == BPOINT_READ || type == BPOINT_WRITE) {
        if (mt == "ram") {
          if (start > 0xFFFF || end > 0xFFFF) {
            reply_err(id, "bad_params", "MD ram breakpoint range is 0x0000..0xFFFF");
            return;
          }
          start = 0xFF0000u | (start & 0xFFFFu);
          end = 0xFF0000u | (end & 0xFFFFu);
        } else if (mt == "zram") {
          if (start > 0x1FFF || end > 0x1FFF) {
            reply_err(id, "bad_params", "MD zram breakpoint range is 0x0000..0x1FFF");
            return;
          }
          start = 0xA00000u | (start & 0x1FFFu);
          end = 0xA00000u | (end & 0x1FFFu);
        } else if (mt == "vram" || mt == "cram" || mt == "vsram" || mt == "vdpreg") {
          if (type != BPOINT_WRITE) {
            reply_err(id, "unsupported", "MD VDP read breakpoints are unavailable; only write breakpoints are supported");
            return;
          }
          uint32 max_addr = mt == "vram" ? 0xFFFFu : mt == "vdpreg" ? 0x1Fu : 0x7Fu;
          if (start > max_addr || end > max_addr) {
            reply_err(id, "bad_params", "MD VDP breakpoint range exceeded");
            return;
          }
          adapter_bp = true;  // VDP writes are port/DMA-side effects, not CPU address-space writes.
        } else if (mt != "cpu") {
          reply_err(id, "unsupported", "MD read/write breakpoints support cpu, ram, zram, and writes to vram, cram, vsram, or vdpreg");
          return;
        }
      }
    } else if (is_ss()) {
      // SS read/write BP는 SH-2 raw effective address(ea=R[n])로 비교된다 — CheckRWBreakpoints
      // (sh7095.inc)가 캐시비트/27bit 마스킹 없이 ea를 그대로 MRead/MWrite로 넘기고, DBG_CheckRead/
      // WriteBP(debug.inc)가 그 주소를 bp.A[0..1]과 직접 비교한다. 따라서 RAM/메모리 region memory_type은
      // read_memory와 같은 offset이 아니라 실제 SH-2 외부버스 주소(base+off)로 잡아야 발화한다 — 변환이
      // 없으면 offset(예: workramh 0x537E0)이 실제 write 주소(0x060537E0)와 안 맞아 accept-but-never-fire.
      // logical 플래그는 SS DBG_AddBreakPoint가 무시하므로 주소가 비교 기준이다(logical은 값-read 경로 일관성용).
      // 미러 미커버: SH-2는 ea를 정규화 안 하므로, 다른 미러 영역으로만 접근하는 코드엔 안 잡힌다.
      // (용어 주의: 0x06000000대는 cacheable cache-area, 0x26000000대가 cache-through 미러다.) 우리 base는
      // cache-area form(0x06000000 등) — 사용자가 그 form으로 잡아 발화 확인했으므로 그게 기준이고,
      // cache-through 미러(0x2x..)로만 가는 접근은 미커버.
      if (type == BPOINT_READ || type == BPOINT_WRITE) {
        if (mt == "physical" || mt == "cpu" || mt.empty()) {
          logical = false;  // 이미 raw SH-2 버스 주소 — 그대로 사용(기존 physical BP 동작 유지)
        } else {
          bool matched = false;
          for (const auto& r : kSSBusRegions) {
            if (mt == r.mt) {
              if (start >= r.size || end >= r.size) {
                reply_err(id, "bad_params", "Saturn RAM-region breakpoint offset exceeds the region size");
                return;
              }
              start = r.base + start;
              end = r.base + end;
              logical = false;
              matched = true;
              break;
            }
          }
          if (!matched) {
            // 변환 불가 memory_type은 수락-후-미발화 대신 명확히 거부한다.
            reply_err(id, "unsupported",
                      "Saturn read/write breakpoints support physical raw bus addresses and workraml, workramh, scspram, "
                      "vdp1vram, vdp2vram, or cram. Other auxiliary regions lack a linear SH-2 external-bus address.");
            return;
          }
        }
      }
    } else if (is_ws()) {
      // WonderSwan(V30MZ, x86 세그먼트): exec BP는 현재 CS 안의 16비트 논리 IP로 비교된다(get_state의
      // V30MZ.IP). read/write BP는 20비트 물리 주소((seg<<4)+off)다(MD/SS와 동형) —
      // 범위 밖 주소(예: (CS<<4)+IP 선형값을 exec BP에 준 경우)는 수락-후-미발화 대신 명확히 거부한다.
      if (type == BPOINT_PC) {
        if (start > 0xFFFF || end > 0xFFFF) {
          reply_err(id, "bad_params",
                    "WonderSwan exec breakpoints use the 16-bit logical IP offset from V30MZ.IP, not the linear (CS<<4)+IP address; "
                    "only read/write breakpoints use 20-bit physical addresses.");
          return;
        }
      } else {
        // read/write BP는 물리 주소 기준(Mednafen WS 디버거의 read/write BP가 물리). memory_type을 whitelist로
        // 검증한다(blacklist 아님) — physical(20비트)·ram(16비트 내부RAM=physical 0x0-0xFFFF)만 허용하고, cs/ss/ds/es
        // 세그먼트뷰·오타·미지원 memtype은 거부한다. 안 그러면 세그먼트/미지원 memtype을 조용히 physical offset에
        // 걸어 엉뚱한 주소를 관측한다(DS:off가 아니라 physical:off; 값-조건 read도 physical로 읽어 필터도 틀어짐).
        if (mt.empty() || mt == "physical") {
          if (start > 0xFFFFF || end > 0xFFFFF) {
            reply_err(id, "bad_params", "WonderSwan physical breakpoints use 20-bit addresses (0x00000..0xFFFFF)");
            return;
          }
        } else if (mt == "ram") {
          // ram(0x0000-0xFFFF 내부RAM)은 physical 0x0-0xFFFF에 매핑 — 그 위 offset은 physical 뱅크1로 새므로 bound한다.
          if (start > 0xFFFF || end > 0xFFFF) {
            reply_err(id, "bad_params", "WonderSwan ram breakpoints use 16-bit internal RAM offsets; use physical above 0xFFFF");
            return;
          }
        } else {
          reply_err(id, "bad_params",
                    "WonderSwan read/write breakpoint memory_type must be physical (20-bit) or ram (16-bit). "
                    "Segment views are read-only address-space views; pass (segment<<4)+offset as physical.");
          return;
        }
      }
    }
    // 값-조건(read/write BP만): value 지정 시 접근 값이 (value & value_mask)와 같을 때만 발화.
    // value_len(1~4)은 비교 바이트 수, value_mask 기본 0xFFFFFFFF(전 비트).
    uint32 value = 0, value_mask = 0xFFFFFFFFu;
    long val_len = 1;
    bool has_value = (line.find("\"value\"") != std::string::npos);
    if (has_value) {
      if (!json_u32_arg(id, line, "value", value, true)
          || !json_u32_arg(id, line, "value_mask", value_mask, false))
        return;
      json_num(line, "value_len", val_len);
      if (val_len < 1) val_len = 1;
      if (val_len > 4) val_len = 4;
      // WonderSwan(V30MZ)은 워드/롱을 per-byte로 쓴다(PutMemB) — write BP 훅이 매 바이트를 1바이트씩 주입해
      // value_len>1의 전체 값을 재구성 못 한다(주입 1바이트 vs 다바이트 value 비교라 조용히 미발화). read는
      // physical에서 value_len 바이트 읽어 정확하므로 write만 거부한다(연속 byte-write 누적은 후속 과제).
      if (is_ws() && type == BPOINT_WRITE && val_len > 1) {
        reply_err(id, "unsupported",
                  "WonderSwan write breakpoint value filters support value_len=1 only because V30MZ writes words per byte. "
                  "Use value_len=1 or a read breakpoint for multi-byte matching.");
        return;
      }
      // write BP의 value 필터는 *쓰는 값*과 비교한다 — CPU 메모리 write는 MD/PCE/PSX/SS 전부 어댑터가
      // 쓰는 값을 주입하므로 동작한다(emucap_bp_record_value: MD=클론버스 DBG_BusWrite, PCE=WriteHandler
      // V, PSX=GPR[rt] 콜백 스레딩, SS=디코더 복제 R[m]/CtrlRegs/CheatMemRead+op). MD VDP write는
      // BPOINT_WRITE+adapter_bp로 emucap_md_vdp_record_write가 값을 싣고, SS vdp2vram은 CPU write 경로라
      // 위 주입으로 잡힌다. read BP value는 읽는 값=현재 메모리라 fallback이 정확.
      // 단 보조(AUX) 주소공간 BP(현재 PCE vram0/1 → BPOINT_AUX_*)는 has_value가 강제 false(아래)라
      // value를 *조용히 무시*하게 된다 — 그 VDP 경로엔 값 주입이 아직 없기 때문. 조용한 무시는
      // silent-wrong이므로 거부한다(blanket "미지원"이 아니라 *이 좁은 aux+value*만 — CPU
      // write+value는 전 시스템 동작). aux/VDP write 경로 값 주입은 후속 과제다.
      if (has_value && (type == BPOINT_AUX_READ || type == BPOINT_AUX_WRITE)) {
        reply_err(id, "unsupported",
                  "Value-filtered breakpoints are unavailable in auxiliary VDP/video address spaces because accessed-value "
                  "capture is implemented only on CPU paths. Omit value or use a CPU memory address.");
        return;
      }
      // 좁은 한계: SS on-chip 레지스터 대상 RMW만 CheatMemRead fastmap 밖이라 부정확 — work RAM 대상은
      // 정확하며 값-BP 실사용은 work RAM이다.
    }
    uint32 pc_min = 0, pc_max = 0xFFFFFFFFu;
    bool has_pc_min = (line.find("\"pc_min\"") != std::string::npos);
    bool has_pc_max = (line.find("\"pc_max\"") != std::string::npos);
    if ((has_pc_min && !json_u32_arg(id, line, "pc_min", pc_min, true))
        || (has_pc_max && !json_u32_arg(id, line, "pc_max", pc_max, true)))
      return;
    if (pc_max < pc_min) pc_max = pc_min;
    bool has_pc_filter = has_pc_min || has_pc_max;
    std::vector<EmucapSnapshotSpec> snapshots;
    if (!validate_snapshot_specs(id, line, snapshots)) return;

    if (!adapter_bp && (!CurGame || !CurGame->Debugger || !CurGame->Debugger->AddBreakPoint)) {
      reply_err(id, "no_debugger", "debugger is not initialized");
    } else {
      long bid = g_bp_next_id++;
      BP b{};
      b.id = bid;
      b.type = type;
      b.a1 = start;
      b.a2 = end;
      b.public_a1 = public_start;
      b.public_a2 = public_end;
      b.logical = logical;
      json_bool(line, "pause_on_hit", b.pause_on_hit);
      b.adapter_bp = adapter_bp;
      b.memory_type = mt;
      b.snapshots = snapshots;
      b.has_value = has_value && (type == BPOINT_READ || type == BPOINT_WRITE);
      b.value = value;
      b.value_mask = value_mask;
      b.val_len = (int)val_len;
      b.has_pc_filter = has_pc_filter;
      b.pc_min = pc_min;
      b.pc_max = pc_max;
      g_bps.push_back(b);
      rearm_breakpoints();
      char buf[48];
      snprintf(buf, sizeof(buf), "{\"id\":%ld,\"set\":true}", bid);
      reply_ok(id, buf);
    }
  } else if (method == "clear_breakpoint") {
    long bid = -1;
    json_num(line, "id", bid);  // 봉투 id가 첫째라 둘째 "id"가 인자 — 최소 파서 한계로 재탐색
    {
      std::string pat = "\"id\"";
      size_t k = line.find(pat, line.find(pat) + 1);  // 두 번째 "id"
      if (k != std::string::npos) {
        size_t c = line.find(':', k);
        if (c != std::string::npos) bid = strtol(line.c_str() + c + 1, nullptr, 10);
      }
    }
    size_t before = g_bps.size();
    std::vector<BP> keep;
    for (auto& b : g_bps) if (b.id != bid) keep.push_back(b);
    g_bps.swap(keep);
    rearm_breakpoints();
    char buf[48];
    snprintf(buf, sizeof(buf), "{\"cleared\":%zu}", before - g_bps.size());
    reply_ok(id, buf);
  } else if (method == "clear_all_breakpoints") {
    g_bps.clear();
    rearm_breakpoints();
    reply_ok(id, "{\"cleared\":true}");
  } else if (method == "list_breakpoints") {
    std::string arr = "[";
    for (size_t i = 0; i < g_bps.size(); i++) {
      const char* k = g_bps[i].type == BPOINT_READ ? "read"
                    : g_bps[i].type == BPOINT_WRITE ? "write"
                    : g_bps[i].type == BPOINT_AUX_READ ? "read"
                    : g_bps[i].type == BPOINT_AUX_WRITE ? "write" : "exec";
      char b[192];
      snprintf(b, sizeof(b), "%s{\"id\":%ld,\"kind\":\"%s\",\"start\":%u,\"end\":%u,\"logical\":%s,\"pause_on_hit\":%s",
               i ? "," : "", g_bps[i].id, k, (unsigned)g_bps[i].public_a1,
               (unsigned)g_bps[i].public_a2,
               g_bps[i].logical ? "true" : "false",
               g_bps[i].pause_on_hit ? "true" : "false");
      arr += b;
      if (!g_bps[i].memory_type.empty()) {
        arr += ",\"memory_type\":\"";
        arr += json_escape(g_bps[i].memory_type);
        arr += "\"";
      }
      if (g_bps[i].has_pc_filter) {
        snprintf(b, sizeof(b), ",\"pc_min\":%u,\"pc_max\":%u",
                 (unsigned)g_bps[i].pc_min, (unsigned)g_bps[i].pc_max);
        arr += b;
      }
      arr += "}";
    }
    arr += "]";
    reply_ok(id, "{\"breakpoints\":" + arr + "}");
  } else if (method == "set_trace" && is_ngp()) {
    reply_err(id, "unsupported",
              "Neo Geo Pocket does not expose trace or call-stack classification");
  } else if (method == "get_trace" && is_ngp()) {
    reply_err(id, "unsupported",
              "Neo Geo Pocket does not expose trace or call-stack classification");
  } else if (method == "call_stack" && is_ngp()) {
    reply_err(id, "unsupported",
              "Neo Geo Pocket does not expose trace or call-stack classification");
  } else if (method == "set_trace") {
    handle_set_trace(id, line);
  } else if (method == "get_trace") {
    handle_get_trace(id, line);
  } else if (method == "watch_register") {
    handle_watch_register(id, line);
  } else if (method == "call_stack") {
    handle_call_stack(id);
  } else if (method == "break_on_reset") {
    handle_break_on_reset(id, line);
  } else if (method == "disassemble") {
    // SH-2 디스어셈블: addr부터 count개 명령. 코어의 Debugger->Disassemble(A&, SpecialA, buf)는
    // A를 다음 명령으로 증가시킨다(가변 길이 디코드라 명령 경계가 정확). raw 바이트 수동 디코드 불필요.
    uint32 addr = 0;
    if (!json_u32_arg(id, line, "address", addr, true)) return;
    long count = 1;
    json_num(line, "count", count);
    if (count < 1) count = 1;
    if (count > 256) count = 256;
    if (!CurGame || !CurGame->Debugger || !CurGame->Debugger->Disassemble) {
      reply_err(id, "no_debugger", "disassembler is not initialized; --enable-debugger is required");
    } else {
      uint32 A = addr;
      std::string out = "[";
      for (long i = 0; i < count; i++) {
        if (is_ngp() && !emucap_ngp_disasm_safe(A, 16)) {
          reply_err(id, "unsupported",
                    "Neo Geo Pocket disassembly requires a complete 16-byte window in RAM, cartridge ROM, or BIOS");
          return;
        }
        char tbuf[256]; tbuf[0] = 0;
        uint32 ia = A;
        CurGame->Debugger->Disassemble(A, A, tbuf);  // A 증가
        if (is_ngp() && A <= ia) {
          reply_err(id, "adapter_error",
                    "Neo Geo Pocket disassembler did not advance to the next instruction");
          return;
        }
        char ab[20]; snprintf(ab, sizeof(ab), "0x%08X", (unsigned)ia);
        out += i ? ",{\"addr\":\"" : "{\"addr\":\"";
        out += ab; out += "\",\"text\":\"";
        for (char* p = tbuf; *p; p++) {            // JSON 이스케이프
          unsigned char c = (unsigned char)*p;
          if (c == '"' || c == '\\') { out += '\\'; out += *p; }
          else if (c == '\t') out += ' ';
          else if (c >= 0x20) out += *p;
        }
        out += "\"}";
      }
      out += "]";
      reply_ok(id, out);
    }
  } else if (method == "poll_events") {
    std::string arr = "[";
    for (size_t i = 0; i < g_bp_hits.size(); i++) {
      const BPHit& h = g_bp_hits[i];
      char b[192];
      snprintf(b, sizeof(b), "%s{\"pc\":%u", i ? "," : "", (unsigned)h.pc);
      arr += b;
      if (h.has_breakpoint_id) {
        snprintf(b, sizeof(b), ",\"id\":%ld,\"breakpoint_id\":%ld",
                 h.breakpoint_id, h.breakpoint_id);
        arr += b;
      }
      if (h.has_access) {
        snprintf(b, sizeof(b), ",\"kind\":\"%s\",\"address\":%u,\"length\":%u",
                 h.is_write ? "write" : "read", (unsigned)h.addr, h.len);
        arr += b;
        if (!h.memory_type.empty()) {
          arr += ",\"memory_type\":\"";
          arr += json_escape(h.memory_type);
          arr += "\"";
        }
        if (h.has_value) {
          snprintf(b, sizeof(b), ",\"value\":%u", (unsigned)h.value);
          arr += b;
        }
      }
      // Event provenance applies to execution boundaries such as reset as well
      // as to memory accesses, so it must not be nested under has_access.
      if (!h.source.empty()) {
        arr += ",\"source\":\"";
        arr += json_escape(h.source);
        arr += "\"";
      }
      if (h.has_source_addr) {
        snprintf(b, sizeof(b), ",\"source_address\":%u", (unsigned)h.source_addr);
        arr += b;
      }
      if (!h.registers.empty()) {
        arr += ",\"registers\":";
        arr += h.registers;  // 이미 {name:value} JSON 오브젝트(exec BP 히트 순간 CPU 레지스터)
      }
      if (!h.snapshot_json.empty()) {
        arr += ",\"snapshot\":";
        arr += h.snapshot_json;
      }
      if (!h.snapshot_error.empty()) {
        arr += ",\"snapshot_error\":\"";
        arr += json_escape(h.snapshot_error);
        arr += "\"";
      }
      arr += "}";
    }
    arr += "]";
    uint64_t dropped = g_bp_dropped;
    g_bp_hits.clear();
    g_bp_dropped = 0;
    char tail[64];
    snprintf(tail, sizeof(tail), ",\"dropped\":%llu}", (unsigned long long)dropped);
    reply_ok(id, "{\"events\":" + arr + tail);
  } else if (method == "reset") {
    MDFNI_Reset();
    if (defer_psx_core_refresh(id, CORE_REFRESH_RESET)) return;
    reply_ok(id, "{\"reset\":true}");
  } else if (method == "screenshot") {
    if (!g_last_surface) {
      reply_err(id, "no_frame", "no rendered frame is available yet");
    } else {
      try {
        // 인스턴스별 유니크 경로 — 다중 인스턴스(MEDNAFEN_ALLOWMULTI)가 같은 파일을
        // unlink/write/read로 경합하지 않게 PID를 넣는다.
        std::string tmp = emucap_temp_file("emucap_ss_" + std::to_string((int)getpid()) + ".png");
        ::unlink(tmp.c_str());  // PNGWrite는 MODE_WRITE_SAFE(O_EXCL)라 기존 파일이면 실패 → 먼저 지운다
        { PNGWrite pw(tmp, g_last_surface, g_last_rect, g_last_lw); }  // 생성자가 PNG를 기록
        FileStream fs(tmp, FileStream::MODE_READ);
        uint64 sz = fs.size();
        std::vector<uint8> buf((size_t)sz);
        if (sz) fs.read(buf.data(), sz);
        fs.close();
        ::unlink(tmp.c_str());  // 읽은 뒤 정리 — 임시파일 잔존 방지
        reply_ok(id, "{\"png_base64\":\"" + base64_encode(buf.data(), buf.size()) + "\"}");
      } catch (std::exception& e) {
        reply_err(id, "io_error", e.what());
      }
    }
  } else {
    reply_err(id, "unknown_method", method.c_str());
  }
  } catch (const std::exception& e) {
    reply_err(id, "internal_error", e.what());
  } catch (...) {
    reply_err(id, "internal_error", "unknown exception");
  }
}

// 소켓을 한 사이클 서비스(논블로킹 recv + 줄 단위 처리). 정상 경로와 frozen 스핀 양쪽에서 쓴다.
void serve_socket_once() {
  if (g_fd < 0) return;
  if (!g_tx.empty()) {
    flush_tx_once();
    if (g_fd < 0 || !g_tx.empty()) return;
  }
  char tmp[8192];
  ssize_t n = recv(g_fd, tmp, sizeof(tmp), 0);
  if (n == 0) { emucap_disconnect(); return; }  // 상대 끊김
  if (n < 0) {
    if (emucap_sock_wouldblock()) return;         // EAGAIN/EWOULDBLOCK/EINTR: 데이터 없음
    emucap_disconnect();                          // ECONNRESET 등 hard error → 재접속
    return;
  }
  g_rx.append(tmp, (size_t)n);
  size_t pos;
  while ((pos = g_rx.find('\n')) != std::string::npos) {
    std::string l = g_rx.substr(0, pos);
    g_rx.erase(0, pos + 1);
    if (!l.empty()) handle(l);
  }
}

// CPU 콜백(BP 히트 시 코어가 호출, MDFNI_Emulate 내부). 히트 명령에서 정지해 소켓을 스핀
// 서비스 → 에이전트가 정확히 그 명령 지점의 메모리·상태를 읽는다. resume(g_frozen=false)에서 복귀.
void emucap_cpu_cb(uint32 PC, bool bpoint) {
  // load_state/reset from a PSX debugger halt completes only after the patched CPU has discarded
  // the pre-replacement timestamp/pipeline and called us again at the restored instruction.
  // This callback is the new linearization point; no guest instruction has executed in between.
  if (complete_psx_core_refresh()) return;
  if (!bpoint) {
    // 실행추적 + 콜스택: 매 명령 PC를 원형버퍼에 기록하고, call/return을 분류해 shadow stack을 유지.
    if (g_trace_enabled) {
      if (!g_trace_ring.empty()) {
        g_trace_ring[g_trace_head] = PC;
        g_trace_head = (g_trace_head + 1) % TRACE_CAP;
        if (g_trace_count < TRACE_CAP) g_trace_count++;
      }
      // SP 기반 반환 감지: 현재 SP가 어느 프레임의 call-시점 SP 이상으로 오르면 그 프레임(들)은 반환됨 → pop.
      // RTS/RTR뿐 아니라 JMP-return·JSR (An) 간접·RTE·수동 스택조작 모든 반환을 잡아 루프 중복누적을 없앤다.
      uint32 sp = 0;
      bool have_sp = read_sp(sp);
      bool popped_by_sp = false;  // (B) SP-prune가 이 명령에서 프레임을 pop했나 — 그랬으면 아래 leaf-pop(D) 스킵
      if (have_sp) {
        // 콜리가 프레임을 확립했는지(sp가 call-시점 아래로) 매 명령 표시한다 — register-linkage는 콜이 sp를
        // 안 바꿔 push 직후 sp==frame.sp라, 확립 전에 pop하면 프레임이 즉시 사라진다(MIPS/SH call_stack=[] 버그).
        // 확립된 프레임만 "sp가 다시 그 이상 = 반환"으로 pop한다. JMP-return·간접반환도 established 후 sp 복귀로 잡힌다.
        if (!g_callstack.empty() && sp < g_callstack.back().sp) g_callstack.back().established = true;
        while (!g_callstack.empty() && g_callstack.back().established && sp >= g_callstack.back().sp) {
          g_callstack.pop_back();
          popped_by_sp = true;
        }
      }
      CallKind ck = classify_instr(PC);
      if (ck == CK_CALL) {
        if (g_callstack.size() < CALLSTACK_CAP) g_callstack.push_back({PC, sp, false});
      } else if (ck == CK_RETURN && !popped_by_sp && !g_callstack.empty() &&
                 (!have_sp || !g_callstack.back().established)) {
        // RETURN opcode로 pop: (a) SP를 못 읽는 백엔드(!have_sp, opcode 폴백) 또는 (b) 미확립 프레임 = LEAF
        // 함수(스택프레임을 안 만들어 sp가 call-시점 아래로 안 내려가 established가 안 됨 → sp-pruning이 못 pop,
        // 안 하면 영구 누적). 단 (B) SP-prune가 이 훅에서 이미 반환 프레임을 pop했으면(popped_by_sp) 이 leaf-pop을
        // 건너뛴다 — 안 그러면 established 콜리의 반환에서 (B)가 그 프레임을 pop한 뒤 (D)가 새 top(미확립 non-leaf —
        // PR을 스택 아닌 콜리-세이브 레지스터에 저장한 부모)을 잘못 pop하는 이중-pop 버그가 난다.
        g_callstack.pop_back();
      }
    }
    // 레지스터 워치: register가 [min,max] 밖이면 이 명령에서 freeze(derail 포착). 1회성(히트 후 해제 —
    // resume 재freeze 방지; 재무장은 watch_register 재호출). enqueue_bp_hit로 derail PC를 poll_events에 싣고
    // pause면 freeze_spin(BP 히트와 동일 경로).
    if (g_watch_enabled) {
      uint32 rv;
      if (read_register_by_name(g_watch_reg, rv) && (rv < g_watch_min || rv > g_watch_max)) {
        g_watch_enabled = false;
        BPHit hit{};
        hit.pc = PC;
        enqueue_bp_hit(hit, g_watch_pause);
      }
    }
    // The WonderSwan callback reports only the 16-bit IP. Combine it with CS before comparison;
    // comparing IP=0 alone would falsely report every segment boundary as a reset.
    uint32 reset_address = PC;
    if (g_break_on_reset && is_ws()) {
      uint32 cs = 0;
      reset_address = read_register_by_name("CS", cs)
                          ? (((cs & 0xFFFFu) << 4) + (PC & 0xFFFFu)) & 0xFFFFFu
                          : 0xFFFFFFFFu;
    }
    if (g_break_on_reset && reset_address == g_reset_entry) {
      BPHit hit{};
      hit.pc = PC;
      hit.source = "reset";
      enqueue_bp_hit(hit, true);
    }
    // continuous 모드(step_instructions 무장) — 코어가 매 명령 발화한다. 명령단위 step이 진행 중이면
    // (g_insn_remaining>0) 1씩 줄이고, 0에 도달하면 완료 응답 후 *기존 BP freeze 경로*(콜백 안에서
    // 게임스레드가 스핀하는 freeze_spin_until_resume)로 그 명령 직전에 정지한다 — 새 동기화 없음.
    // SS 듀얼 SH-2: 코어가 DBG.ActiveCPU(기본 master/which=0)일 때만 이 cb를 부른다(ss/debug.inc:475
    // 비활성 CPU 조기 return) → '1 명령'은 active CPU 1명령으로 모호성 없음(cb는 which를 못 보지만
    // 코어가 이미 필터). g_insn_remaining==0이면 무해(잔여 continuous는 resume이 해제).
    if (g_insn_remaining > 0) {
      // cold(pause/프레임-step) 진입 보정: 첫 continuous cb는 그 진입명령의 것이라 카운트하지 않고
      // 흡수해 공짜 실행시킨다 — BP 히트의 공짜 진입명령(bpoint cb가 이미 일어남)과 동형. 이렇게 해야
      // cold도 정확히 N명령 실행한다(흡수 없으면 N-1, N=1이면 PC 불변→검증 실패).
      if (g_insn_skip_first) {
        g_insn_skip_first = false;
        return;
      }
      if (--g_insn_remaining == 0) {
        if (g_insn_step_id >= 0) {
          char buf[96];
          snprintf(buf, sizeof(buf), "{\"status\":\"completed\",\"frame\":%llu}",
                   (unsigned long long)g_frame);
          reply_ok(g_insn_step_id, buf);     // 프레임 step과 동일 응답 형태(지연 완료)
          g_insn_step_id = -1;
        }
        g_insn_progress_ms = 0;
        freeze_spin_until_resume();           // BP 히트와 동일 경로로 이 명령 직전에 정지
      } else if (g_insn_step_id >= 0 && g_tx.empty()
                 && emucap_progress_due(
                     g_insn_progress_ms, monotonic_millis(), PROGRESS_INTERVAL_MS)) {
        reply_ok(g_insn_step_id, "{\"status\":\"working\"}");
      }
    }
    return;
  }
  bool matched = false;
  bool should_freeze = false;
  std::vector<const BP*> matched_breakpoints;
  // 값-조건 BP 필터: read/write BP가 매칭(g_bp_hit_valid)됐고 그 주소를 덮는 has_value BP가 있으면,
  // 접근 주소의 값을 읽어 value/value_mask와 비교한다. 불일치면 그 BP만 스킵(노이즈 격리).
  // read는 읽을 값=메모리 현재라 정확. 일부 코어(MD)는 write 콜백에서 실제 write 값을 함께 기록한다.
  if (g_bp_hit_valid) {
    int hit_type = g_bp_hit_type;
    for (auto& b : g_bps) {
      if (b.adapter_bp) continue;
      if (b.type != hit_type || g_bp_hit_addr < b.a1 || g_bp_hit_addr > b.a2) continue;
      if (!bp_pc_allows(b, PC)) continue;
      if (b.has_value) {
        uint32 v = g_bp_hit_has_value ? g_bp_hit_value
                                      : emucap_read_value_for_bp(b, g_bp_hit_addr, (unsigned)b.val_len);
        if ((v & b.value_mask) != (b.value & b.value_mask)) {
          continue;
        }
      }
      matched = true;
      matched_breakpoints.push_back(&b);
      if (b.pause_on_hit) should_freeze = true;
    }
  } else {
    const uint32 exec_pc = is_psx() ? fold_psx_exec_address(PC) : PC;
    for (auto& b : g_bps) {
      if (b.adapter_bp) continue;
      if (b.type != BPOINT_PC || exec_pc < b.a1 || exec_pc > b.a2) continue;
      if (!bp_pc_allows(b, PC)) continue;
      matched = true;
      matched_breakpoints.push_back(&b);
      if (b.pause_on_hit) should_freeze = true;
    }
  }
  if (!matched) {
    g_bp_hit_valid = false;
    g_bp_hit_has_value = false;
    return;
  }
  BPHit base_hit{};
  base_hit.pc = PC;
  if (g_bp_hit_valid) {
    base_hit.has_access = true;
    base_hit.addr = g_bp_hit_addr;
    base_hit.len = g_bp_hit_len;
    base_hit.is_write = g_bp_hit_type == BPOINT_WRITE
                        || g_bp_hit_type == BPOINT_AUX_WRITE;
    base_hit.has_value = g_bp_hit_has_value;
    base_hit.value = g_bp_hit_value;
  }
  g_bp_hit_valid = false;
  g_bp_hit_has_value = false;
  // PC-FX breakpoint evidence uses one callback window for CPU registers, access metadata,
  // and requested memory snapshots. Other Mednafen systems retain the older exec-only
  // register capture to avoid multiplying hot read/write event cost.
  if (is_pcfx() || !base_hit.has_access) base_hit.registers = capture_registers_json();
  std::vector<BPHit> events;
  for (const BP* matched_breakpoint : matched_breakpoints) {
    const BP& breakpoint = *matched_breakpoint;
    BPHit hit = base_hit;
    hit.has_breakpoint_id = true;
    hit.breakpoint_id = breakpoint.id;
    if (is_pcfx() && hit.has_access) {
      if (breakpoint.type == BPOINT_AUX_READ || breakpoint.type == BPOINT_AUX_WRITE) {
        std::string memory_type;
        uint32 public_address = 0;
        uint32 public_length = 0;
        if (!emucap_pcfx_aux_public_range(
                hit.addr, hit.len, memory_type, public_address, public_length)
            || memory_type != breakpoint.memory_type) {
          g_bp_dropped++;
          continue;
        }
        hit.memory_type = memory_type;
        hit.addr = public_address;
        hit.len = public_length;
      } else {
        hit.memory_type = breakpoint.memory_type.empty() ? "cpu" : breakpoint.memory_type;
        if (hit.memory_type == "bios") hit.addr -= 0xFFF00000u;
      }
      if (!ranges_overlap(
              hit.addr, hit.len, breakpoint.public_a1, breakpoint.public_a2)) {
        g_bp_dropped++;
        continue;
      }
    }
    if (!breakpoint.snapshots.empty()) {
      try {
        hit.snapshot_json = capture_snapshot_json(breakpoint.snapshots);
      } catch (const std::exception& error) {
        hit.snapshot_error = error.what();
      } catch (...) {
        hit.snapshot_error = "unknown snapshot capture failure";
      }
    }
    events.push_back(hit);
  }
  if (events.empty()) {
    // Accepted PC-FX mappings are required to round-trip. Reaching this branch means the
    // adapter can no longer prove which public breakpoint fired, so do not turn it into a
    // normal queue overflow. Persist the native failure and close the debugger surface.
    contain_service_exception(
        "breakpoint_event",
        "accepted PC-FX breakpoint hit could not be mapped back to its public range");
    return;
  }

  // A single backend hit may match several public breakpoint records. Preserve that set
  // atomically: a pausing hit evicts older evidence to make room for the complete current
  // set, while a non-pausing hit either admits the whole set or accounts the whole set as
  // dropped. Partial sibling events would make breakpoint_id evidence ambiguous.
  if (events.size() > EVENT_CAP) {
    contain_service_exception(
        "breakpoint_event",
        "one backend breakpoint hit exceeded the public event queue capacity");
    return;
  }
  if (should_freeze) {
    const size_t required =
        g_bp_hits.size() + events.size() > EVENT_CAP
            ? g_bp_hits.size() + events.size() - EVENT_CAP
            : 0;
    if (required > 0) {
      g_bp_hits.erase(g_bp_hits.begin(), g_bp_hits.begin() + required);
      g_bp_dropped += required;
    }
  } else if (g_bp_hits.size() + events.size() > EVENT_CAP) {
    g_bp_dropped += events.size();
    return;
  }
  for (size_t index = 0; index < events.size(); index++) {
    enqueue_bp_hit(events[index], should_freeze && index + 1 == events.size());
  }
}

}  // namespace

// 게임 스레드 gamepad UpdateInput이 호출 — 코어가 실제로 읽은 입력 버퍼 비트를 기록한다.
// status/set_input 응답으로 노출해 주입이 게임에 실제 도달한 비트를 검증한다.
extern "C" void emucap_game_data_store(unsigned short d) {
  g_last_game_data.fetch_or(d);  // set_input 이후 게임이 본 모든 비트를 누적 — 진동(프레임시작 0
                                 // ↔ MidSync 입력) 위상에 무관하게 "도달했는가"를 확인한다
}

extern "C" void emucap_smpc_read_store(unsigned addr, unsigned value, const unsigned char* oreg, unsigned len) {
  unsigned a = addr & 0x3F;
  unsigned n = len > sizeof(g_last_smpc_oreg) ? sizeof(g_last_smpc_oreg) : len;
  if (oreg && n) memcpy(g_last_smpc_oreg, oreg, n);
  g_last_smpc_read_addr = a;
  g_last_smpc_read_value = value & 0xFF;
  if (a < 64) g_smpc_read_mask.fetch_or(1ULL << a);
  g_smpc_read_count.fetch_add(1);
}

// 값-조건 BP: debug.inc의 DBG_CheckReadBP/WriteBP가 BP 범위에 매칭될 때 호출한다. 접근 주소/길이/
// 유형을 기록하면 직후 emucap_cpu_cb이 그 주소의 값을 읽어 BP value/value_mask로 필터한다.
// anonymous namespace 변수는 같은 TU라 여기서 접근 가능(emucap_apply_input과 동일).
extern "C" void emucap_bp_record(unsigned len, unsigned addr, int is_write) {
  g_bp_hit_addr = addr;
  g_bp_hit_len = len;
  g_bp_hit_type = is_write ? BPOINT_WRITE : BPOINT_READ;
  g_bp_hit_has_value = false;
  g_bp_hit_value = 0;
  g_bp_hit_valid = true;
}

extern "C" void emucap_bp_record_value(unsigned len, unsigned addr, int is_write, unsigned value) {
  g_bp_hit_addr = addr;
  g_bp_hit_len = len;
  g_bp_hit_type = is_write ? BPOINT_WRITE : BPOINT_READ;
  g_bp_hit_has_value = true;
  g_bp_hit_value = value;
  g_bp_hit_valid = true;
}

extern "C" void emucap_bp_record_pcfx_aux(
    unsigned len,
    unsigned addr,
    int is_write,
    unsigned value) {
  g_bp_hit_addr = addr;
  g_bp_hit_len = len;
  g_bp_hit_type = is_write ? BPOINT_AUX_WRITE : BPOINT_AUX_READ;
  g_bp_hit_has_value = is_write && addr < 0x80000;
  g_bp_hit_value = value;
  g_bp_hit_valid = true;
}

extern "C" void emucap_md_vdp_write(const char* memory_type, unsigned address, unsigned length, unsigned value,
                                    unsigned pc, const char* source, unsigned source_address) {
  if (!is_md() || !memory_type || !*memory_type || length == 0) return;

  bool matched = false;
  bool should_freeze = false;
  for (auto& b : g_bps) {
    if (!b.adapter_bp || b.type != BPOINT_WRITE) continue;
    if (b.memory_type != memory_type) continue;
    if (!ranges_overlap(address, length, b.a1, b.a2)) continue;
    if (!bp_pc_allows(b, pc)) continue;
    if (!bp_value_allows(b, true, value)) continue;
    matched = true;
    if (b.pause_on_hit) should_freeze = true;
  }
  if (!matched) return;

  // 버퍼가 가득 찼고 non-freezing이면 힙 std::string(memory_type/source)을 빌드하기 전에 드롭한다 —
  // DMA 중 프레임당 수만 write가 매번 문자열 할당하는 것을 막는다(enqueue_bp_hit의 push/drop 가드와 이중).
  if (g_bp_hits.size() >= EVENT_CAP && !should_freeze) { g_bp_dropped++; return; }

  BPHit hit{};
  hit.pc = pc;
  hit.has_access = true;
  hit.addr = address;
  hit.len = length;
  hit.is_write = true;
  hit.has_value = true;
  hit.value = value;
  hit.memory_type = memory_type;
  if (source && *source) hit.source = source;
  if (source_address != 0xFFFFFFFFu) {
    hit.has_source_addr = true;
    hit.source_addr = source_address;
  }
  enqueue_bp_hit(hit, should_freeze);
}

// MDFNGameInfo->Emulate 직전과 MidSync에서 호출. 주입이 engaged면 포트0 버퍼를 override mask로
// 덮어쓴다. 다음 코어 입력 갱신이 이 버퍼를 읽으므로 step/run_frames 진행 중에도 주입이 반영된다.
extern "C" void emucap_apply_input(unsigned char* port0_data, unsigned port0_len) {
  // 포트0 버퍼는 active-high(눌림=1)로 SS·PSX·PCE·MD 공통이다 — 코어가 읽을 때만 반전한다
  // (Saturn: ~(data[0]|data[1]<<8); PSX gamepad/DualShock: 전송 시 0xFF^buttons). 따라서
  // 마스크를 그대로 기록한다. PSX DualShock의 analog/axis(바이트2~)는 안 건드려 보존된다.
  g_input_override.apply(port0_data, port0_len);
}

// MDFNI_Emulate 직후 훅에서 최신 프레임버퍼를 기록(screenshot이 PNG로 인코딩). 타입 결합을 피하려고
// void*로 받아 캐스팅한다(main.cpp 훅은 extern 인라인 선언으로 호출). 익명 namespace 전역 접근은 같은 파일.
void emucap_capture(const void* surface, const void* rect, const void* line_widths) {
  g_last_surface = (const MDFN_Surface*)surface;
  if (rect) g_last_rect = *(const MDFN_Rect*)rect;
  g_last_lw = (const int32*)line_widths;
}

void emucap_pre_first_frame() {
  static bool first = true;
  if (!first) return;
  first = false;
  if (!start_frozen_requested()) return;

  if (repeatable_profile_requested()) {
    std::string error;
    if (!capture_repeatable_initial_state(error)) {
      fprintf(stderr, "emucap: repeatable initial state unavailable: %s\n", error.c_str());
    }
  }

  // This hook runs after the game is loaded but before the first MDFNI_Emulate call. Keep the
  // emulator on this stack frame until an explicit advancing command arrives. A missing or lost
  // controller is not permission to execute the first guest instruction.
  g_frozen = true;
  g_launch_start_controlled = true;
  while (g_frozen && g_step_remaining == 0 && g_probe_id < 0 && g_insn_remaining == 0) {
    try {
      if (g_fd < 0) emucap_connect();
      if (g_fd >= 0) serve_socket_once();
    } catch (const std::exception& error) {
      contain_service_exception("pre_first_frame", error.what());
    } catch (...) {
      contain_service_exception("pre_first_frame", "unknown native adapter exception");
    }
    usleep(2000);
  }
}

// 프레임 루프에서 매 프레임(MDFNI_Emulate 직후) 호출. 논블로킹이되 frozen이면 스핀해 프레임을 막는다.
void emucap_service(uint64_t frame) {
  try {
  g_frame = frame;
  if (g_fd < 0) {
    if (!g_frozen) {
      emucap_connect();  // While running, retry each frame; a missing server is rejected immediately.
      return;
    }
    // Transport loss is not a release event for an owned freeze. Remain at this exact frame
    // boundary and retry the loopback control connection without returning to MDFNI_Emulate.
    while (g_frozen && g_fd < 0) {
      emucap_connect();
      if (g_fd < 0) usleep(2000);
    }
  }
  // A recording transaction owns every guest frame until its terminal boundary. Its event stream
  // uses a dedicated bounded host sink rather than the live poll_events queue.
  if (g_recording) {
    if (service_recording()) return;
  }
  // 지연 명령(run_frames) 진행 중이면 그것만 진행한다(새 명령은 안 받음 — 에이전트 대기 중).
  if (g_def_id >= 0) {
    if (!g_tx.empty() && flush_tx_once() == TX_ERROR) return;
    if (g_def_id < 0) return;
    g_def_remaining--;
    if (g_def_remaining <= 0) {
      if (g_def_is_press) { g_input_override.release(); g_def_is_press = false; }
      char buf[96];
      snprintf(buf, sizeof(buf), "{\"status\":\"completed\",\"frame\":%llu}",
               (unsigned long long)g_frame);
      reply_ok(g_def_id, buf);
      g_def_id = -1;
      g_def_progress_ms = 0;
    } else if (g_tx.empty()
               && emucap_progress_due(
                   g_def_progress_ms, monotonic_millis(), PROGRESS_INTERVAL_MS)) {
      reply_ok(g_def_id, "{\"status\":\"working\"}");  // keepalive(Rust가 working은 건너뜀)
    }
    return;
  }
  // 원자적 probe 진행 중: N프레임 진행(다른 명령 차단) 후 타깃 읽고 응답.
  if (g_probe_id >= 0) {
    if (!g_tx.empty() && flush_tx_once() == TX_ERROR) return;
    if (g_probe_id < 0) return;
    if (g_probe_remaining > 0) {
      g_probe_remaining--;
      if (g_probe_remaining > 0 && g_tx.empty()
          && emucap_progress_due(
              g_probe_progress_ms, monotonic_millis(), PROGRESS_INTERVAL_MS))
        reply_ok(g_probe_id, "{\"status\":\"working\"}");  // 긴 진행도 타임아웃 안 나게
      if (g_probe_remaining > 0)
        return;  // 프레임 진행만(serve_socket_once 미호출 → 네트워크 갭 없음 → 결정론)
    }
    std::string hex;
    if (!read_aspace_hex(g_probe_mt, g_probe_addr, g_probe_len, hex)) {
      reply_err(g_probe_id, "bad_params", "unknown memory_type or address/length exceeds the region");
    } else {
      reply_ok(g_probe_id, "{\"status\":\"completed\",\"requested_frames\":"
          + std::to_string(g_probe_requested) + ",\"completed_frames\":"
          + std::to_string(g_probe_requested) + ",\"hex\":\"" + hex + "\"}");
    }
    g_probe_id = -1;
    g_probe_requested = 0;
    g_probe_progress_ms = 0;
    return;
  }
  // step(N) 진행: 매 프레임 1씩 줄이고, 0에서 완료 응답 후 frozen 유지(다음 호출이 스핀).
  if (g_step_remaining > 0) {
    if (!g_tx.empty() && flush_tx_once() == TX_ERROR) return;
    if (g_step_id < 0) return;
    g_step_remaining--;
    if (g_step_remaining == 0 && g_step_id >= 0) {
      char buf[96];
      snprintf(buf, sizeof(buf), "{\"status\":\"completed\",\"frame\":%llu}",
               (unsigned long long)g_frame);
      reply_ok(g_step_id, buf);
      g_step_id = -1;
      g_step_progress_ms = 0;
    } else if (g_step_id >= 0 && g_tx.empty()
               && emucap_progress_due(
                   g_step_progress_ms, monotonic_millis(), PROGRESS_INTERVAL_MS)) {
      reply_ok(g_step_id, "{\"status\":\"working\"}");
    }
    return;  // 반환해 프레임 1개 진행
  }
  // frozen: 반환하면 게임루프가 MDFNI_Emulate를 또 부르므로, 여기서 스핀해 프레임을 막는다.
  // step(remaining>0) 또는 resume(frozen=false)이 오면 빠져나가 프레임을 진행시킨다.
  if (g_frozen) {
    // 프레임경계 cold park. 여기로 park하는 시점에 via_cb=false(진입명령 cb 미발화 → step_instructions가
    // 첫 cb를 흡수해 정확히 N). 권위가 park 위치라 핸들러보다 견고 — pause/frame-step 완료가 여기로 오면
    // 자동 false, BP/명령단위 park는 freeze_spin이 true. running 경로(아래 함수 끝 serve_socket_once,
    // 미-frozen)는 안 건드린다.
    g_frozen_via_cb = false;
    // probe가 대기 중이면(g_probe_id>=0) 스핀을 빠져나가 프레임을 진행시켜야 한다(probe는 진행 필요).
    // step_instructions(g_insn_remaining>0)도 마찬가지 — 빠져나가 프레임을 진행시키면 continuous cb가
    // N명령 후 콜백 안에서 재freeze한다(pause에서 명령단위 step 진입 경로).
    while (g_frozen && g_step_remaining == 0 && g_probe_id < 0 && g_insn_remaining == 0) {
      serve_socket_once();
      // A dead control socket is not permission to advance one guest frame. Reconnect inside the
      // same frozen park; returning here would let MDFNI_Emulate run once before the next service
      // call and would move the exact observation boundary merely because the MCP restarted.
      while (g_frozen && g_fd < 0) {
        emucap_connect();
        if (g_fd < 0) usleep(2000);
      }
      usleep(2000);          // 2ms — busy-spin 방지
    }
    return;
  }
  serve_socket_once();
  if (g_test_adapter_exception_id >= 0)
    throw std::runtime_error("injected native adapter service exception");
  } catch (const std::exception& error) {
    contain_service_exception("service", error.what());
  } catch (...) {
    contain_service_exception("service", "unknown native adapter exception");
  }
}
