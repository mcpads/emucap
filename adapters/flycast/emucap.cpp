// emucap — Flycast(Dreamcast/SH-4) 어댑터. emucap-mcp 서버에 NDJSON 클라이언트로 접속하고,
// vblank마다 emucap_service()로 요청을 처리한다(emu 스레드 — 락 불필요). Mednafen emucap 패턴을
// Flycast API로 적응: 메모리=공통 region(ram/vram/aica)→SH-4 addrspace, 레지스터=Sh4cntx, freeze=vblank 훅 스핀.
// 빌드/주입은 adapters/flycast/build.sh. 광고 메서드는 런타임에 hello/status.methods로 확인한다.
#include "emulator.h"
#include "emucap_failure.h"
#include "emucap_input.h"
#include "emucap_native_failure.h"
#include "hw/sh4/sh4_if.h"
#include "hw/sh4/sh4_opcode_list.h"  // OpDesc[]·Disassemble (disassemble)
#include "hw/mem/addrspace.h"
#include "types.h"                    // settings.content (get_rom_info)
#include "input/gamepad.h"          // DreamcastKey: DC_BTN_* / DC_DPAD_* 비트(kcode active-low)
#include "input/gamepad_device.h"    // extern u32 kcode[4] — Flycast 입력의 원천(Lua pressButtons도 여기에 씀)
#include "serialize.h"               // Serializer/Deserializer + dc_serialize (save/load_state)
// 빌드 hash(build.sh가 $SRC/core/emucap_build.h 생성; 없으면 unknown 폴백 — LSP·build.sh 밖 직접 컴파일 대비).
#if defined(__has_include)
#if __has_include("emucap_build.h")
#include "emucap_build.h"
#endif
#endif
#ifndef EMUCAP_BUILD_HASH
#define EMUCAP_BUILD_HASH "unknown"
#endif

#include <string>
#include <vector>
#include <set>
#include <algorithm>
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <cctype>
#include <atomic>
#include <chrono>
#include <mutex>
#include <stdexcept>
#ifdef _WIN32
#include <winsock2.h>   // Windows 소켓(MinGW) — POSIX sys/socket.h 대체
#include <ws2tcpip.h>   // inet_pton
#include <windows.h>    // Sleep
#else
#include <sys/socket.h>
#include <sys/time.h>                // struct timeval (SO_RCVTIMEO — 동기 핸드셰이크)
#include <netinet/in.h>
#include <arpa/inet.h>
#endif
#include <unistd.h>
#include <fcntl.h>
#include <cerrno>

// ── 소켓 이식성 shim (POSIX ↔ Windows/MinGW) ───────────────────────────────
// Windows는 소켓이 winsock이라 close→closesocket, fcntl(nonblock)→ioctlsocket, errno→WSAGetLastError,
// SO_RCVTIMEO가 timeval 대신 DWORD(ms)로 다르고 socket 전 WSAStartup이 필요하다. fd는 int로 다뤄도
// MinGW/x64에서 핸들이 작아 유효(INVALID_SOCKET→-1 truncate).
#ifdef _WIN32
static inline void emucap_net_init() { static bool d = false; if (!d) { WSADATA w; WSAStartup(MAKEWORD(2, 2), &w); d = true; } }
static inline int  emucap_closesock(int s) { return ::closesocket((SOCKET)s); }
static inline int  emucap_set_nonblock(int s) { u_long m = 1; return ::ioctlsocket((SOCKET)s, FIONBIO, &m); }
static inline bool emucap_sock_wouldblock() { int e = ::WSAGetLastError(); return e == WSAEWOULDBLOCK || e == WSAEINTR; }
static inline bool emucap_sock_eintr() { return ::WSAGetLastError() == WSAEINTR; }
static inline void emucap_sock_wait_ms(unsigned ms) { ::Sleep(ms); }
static inline void emucap_set_rcvtimeo_ms(int s, int ms) { DWORD t = (DWORD)ms; ::setsockopt((SOCKET)s, SOL_SOCKET, SO_RCVTIMEO, (const char*)&t, sizeof(t)); }
#else
static inline void emucap_net_init() {}
static inline int  emucap_closesock(int s) { return ::close(s); }
static inline int  emucap_set_nonblock(int s) { return ::fcntl(s, F_SETFL, O_NONBLOCK); }
static inline bool emucap_sock_wouldblock() { return errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR; }
static inline bool emucap_sock_eintr() { return errno == EINTR; }
static inline void emucap_sock_wait_ms(unsigned ms) { ::usleep(ms * 1000); }
static inline void emucap_set_rcvtimeo_ms(int s, int ms) { struct timeval tv; tv.tv_sec = ms / 1000; tv.tv_usec = (ms % 1000) * 1000; ::setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv)); }
#endif

// gui.cpp에 build.sh가 주입하는 래퍼. capture_raw는 GL 컨텍스트(UI 스레드)에서만 — renderer->GetLastFrame로
// 최신 프레임 raw RGB를 얻는다. encode_png는 GL 불필요(stbi) — 어느 스레드서나 raw→PNG 인코딩.
void emucap_capture_raw(std::vector<unsigned char>& out, int& w, int& h);
void emucap_encode_png(const unsigned char* raw, int w, int h, std::vector<unsigned char>& png);

// exec breakpoint armed 플래그(전역 — sh4_interpreter.cpp Run() 훅이 extern으로 읽는다). BP가 하나라도
// 있을 때만 true → 없으면 인터프리터 핫루프는 bool 한 번만 본다(조회 비용 0). emucap.cpp가 갱신.
bool g_emucap_bp_armed = false;
// 실행추적/레지스터워치 armed 플래그(전역 — 같은 Run() 훅이 extern으로 읽는다). set_trace 또는
// watch_register가 켜졌을 때만 true → 셋 다 off면 인터프리터 핫루프는 bool 한 번만 봐서 훅 비용 0.
// rebuild_trace_armed()가 g_trace_enabled||g_watch_enabled로 매번 재계산해 갱신한다.
bool g_emucap_trace_armed = false;
uint32_t g_emucap_crash_pc_ring[EMUCAP_CRASH_PC_CAP]{};
uint64_t g_emucap_crash_pc_sequence = 0;

namespace {

int g_fd = -1;            // emucap-mcp 서버 소켓(클라이언트). <0이면 미연결.
std::string g_rx;         // 수신 라인 버퍼
std::string g_tx;         // 아직 보내지 못한 NDJSON bytes(keepalive + final을 순서대로 보존)
size_t g_tx_pos = 0;      // g_tx의 다음 전송 byte
static const size_t TX_CAP = 8 * 1024 * 1024;
static const long MAX_SYNC_ADVANCE = 5000;
uint64_t g_frame = 0;     // vblank 카운터(우리 기준)
std::atomic<uint64_t> g_observed_frame{0};  // UI-thread diagnostics read this snapshot.
bool g_frozen = false;    // freeze 상태(스핀으로 프레임 진행 차단)
long g_step_id = -1;      // step(frames) 완료 응답 대기 id
long g_step_remaining = 0;
// 입력 홀드: set_input은 emucap_service(emu 스레드)에서 쓰고, Maple GetInput 소비 지점이 읽는다.
// 소유권+pressed mask 단일 atomic snapshot이며 빈 mask는 네이티브 입력권을 반환한다.
EmucapFlycastInputOverride g_input_override;
// screenshot(연속 버퍼): UI 스레드가 매 렌더마다 최신 프레임 raw를 g_fb_raw에 캡처(emucap_capture_latest),
// emu 스레드는 screenshot 요청 시 그 버퍼를 PNG 인코딩해 즉시 응답한다. freeze(vblank-스핀)는 UI 렌더를
// 막으므로 gui_runOnUiThread/지연 방식은 데드락 → 버퍼 방식이라야 frozen서도 동작(버퍼엔 freeze 직전
// 프레임=frozen 상태가 남는다). cross-thread(UI 쓰기·emu 읽기)라 mutex로 보호.
std::mutex g_fb_mtx;
std::vector<u8> g_fb_raw;     // 최신 프레임 raw RGB(UI 스레드가 채움)
int g_fb_w = 0, g_fb_h = 0;
bool g_fb_fresh = false;      // load_state 뒤 새 render capture가 오기 전 stale 성공을 막는다
// exec breakpoint: PC가 BP 주소에 닿으면 인터프리터 Run() 루프(주입 훅)가 그 명령 실행 전에
// emucap_bp_spin으로 정지(명령-정밀)하고 소켓을 서비스한다. g_bp_addrs는 빠른 조회용 집합.
struct EmuBp { long id; uint32_t addr; };
std::vector<EmuBp> g_bps;
std::set<uint32_t> g_bp_addrs;   // 빠른 히트 조회(Run 루프가 매 명령 확인)
struct FlyHit { uint32_t pc; std::string registers; };  // 히트 PC + 히트 순간 CPU 레지스터({name:value})
std::vector<FlyHit> g_bp_hits;   // 히트 누적(poll_events가 드레인)
long g_bp_next_id = 1;

// ── 크래시경로 관측(set_trace/get_trace/watch_register/call_stack) ─────────────
// 실행추적(set_trace/get_trace): trace armed면 인터프리터 Run() 훅이 매 명령 PC를 원형버퍼에 기록한다 —
// 크래시 직전 실행 경로("어떻게 여기 왔나") 역추적용. exec BP와 같은 훅 자리를 공유한다(PC만 — arch 독립).
bool g_trace_enabled = false;
static const size_t TRACE_CAP = 4096;
std::vector<uint32_t> g_trace_ring;  // 최근 실행 PC(원형; 크기 TRACE_CAP)
size_t g_trace_head = 0;             // 다음 기록 위치
size_t g_trace_count = 0;            // 채워진 개수(≤ TRACE_CAP)
// 레지스터 워치(watch_register): trace armed면 훅이 매 명령 register 값을 읽어 [min,max]를 벗어나면 그
// 명령에서 freeze한다(SP 폭주 등 derail을 발생 지점에서 포착). 히트 시 1회성 해제(resume 재발화 방지).
bool g_watch_enabled = false;
std::string g_watch_reg;
uint32_t g_watch_min = 0, g_watch_max = 0;
bool g_watch_pause = true;
// 콜스택(call_stack): set_trace가 켜진 동안 훅이 call 명령에서 {call-site PC, 그 시점 SP=R15}를 push하고,
// *매 명령 SP가 어느 프레임의 call 시점 SP 이상으로 올라가면 그 프레임을 pop*한다 — RTS뿐 아니라 JMP-return·
// JSR @Rn 간접·수동 스택조작 등 *모든 반환*을 SP로 감지해 루프 중복누적 폴루션을 없앤다. "어떻게 여기 왔나"를
// 스택 메모리 손상과 독립적으로 답한다. set_trace(true) 선행 필요. ISA 분류는 emucap_classify(SH-4). SH-4는
// SP(R15)가 항상 Sh4cntx에서 읽히므로(디버거 불필요) SP-기반 반환 감지가 언제나 유효(opcode 폴백 불필요).
struct CSFrame { uint32_t pc, sp; bool established = false; };
std::vector<CSFrame> g_callstack;
static const size_t CALLSTACK_CAP = 256;

// Fatal state is captured on the emulation thread before the upstream throw. While active, the
// same thread remains at that exact point and serves only read-only diagnostic methods.
bool g_failure_active = false;
std::atomic<bool> g_failure_captured{false};
bool g_failure_file_written = false;
bool g_failure_dismissed = false;
bool g_failure_synthetic = false;
bool g_synthetic_fatal_pending = false;
long g_test_adapter_exception_id = -1;
std::atomic<bool> g_failure_shutdown_requested{false};
std::string g_failure_reason;
uint32_t g_failure_epc = 0;
uint32_t g_failure_event = 0;
bool g_internal_failure_active = false;
char g_internal_failure_operation[128]{};
char g_internal_failure_reason[512]{};
std::atomic<bool> g_capture_disabled{false};
std::mutex g_failure_artifact_mtx;

const char* PROTOCOL_NAME = "flycast";

bool env_enabled(const char* name) {
	const char* value = getenv(name);
	return value != nullptr && (strcmp(value, "1") == 0 || strcmp(value, "true") == 0
		|| strcmp(value, "TRUE") == 0);
}

uint64_t failure_hold_ms() {
	const char* value = getenv("EMUCAP_FAILURE_HOLD_MS");
	if (value == nullptr || value[0] == '\0') return 10 * 60 * 1000;
	char* end = nullptr;
	unsigned long long parsed = strtoull(value, &end, 10);
	if (end == value || (end != nullptr && *end != '\0')) return 10 * 60 * 1000;
	return (uint64_t)parsed;  // zero explicitly means no deadline
}

uint64_t unix_time_ms() {
	return (uint64_t)std::chrono::duration_cast<std::chrono::milliseconds>(
		std::chrono::system_clock::now().time_since_epoch()).count();
}

bool failure_method_allowed(const std::string& method) {
	return method == "hello" || method == "status" || method == "get_state"
		|| method == "read_memory" || method == "screenshot" || method == "get_trace"
		|| method == "call_stack" || method == "disassemble" || method == "find_pattern"
		|| method == "get_rom_info" || method == "poll_events" || method == "dismiss_failure";
}

// ── DC 메모리 region(균일 인터페이스) ────────────────────────
// 다른 어댑터(SS/MD가 ram/vram, Mednafen kSSBusRegions)와 동일하게, read/write_memory/find_pattern은
// memory_type을 공통 region 이름으로 받아 SH-4 절대 base로 해소하고 address를 그 region 내 *0-based
// 오프셋*으로 다룬다(SH-4 flat 절대주소를 직접 받던 동작은 폐기 — 균일화). 읽기/쓰기는
// addrspace::read8/write8(base+offset). nommu 전 SH-4 맵.
//   ram  → 메인 RAM 캐시미러 0x8C000000(16MB) · vram → PowerVR 0x04000000(8MB) · aica → 사운드 0x00800000(2MB)
struct DCRegion { const char* mt; uint32_t base; uint32_t size; };
const DCRegion kDCRegions[] = {
	{"ram",  0x8C000000u, 0x1000000u},  // 메인 RAM 캐시미러 16MB
	{"vram", 0x04000000u, 0x800000u},   // PowerVR VRAM 8MB
	{"aica", 0x00800000u, 0x200000u},   // AICA(사운드) RAM 2MB
};
const DCRegion* find_region(const std::string& mt) {
	for (const auto& r : kDCRegions) if (mt == r.mt) return &r;
	return nullptr;
}
// 다른 어댑터·status.memory_types와 일치하는 supported 목록(에러 message에 동반).
const char* kSupportedMemTypes = "[\"ram\",\"vram\",\"aica\"]";

// ── 소켓 ─────────────────────────────────────────────────────
int emucap_port() {
	const char* p = getenv("EMUCAP_PORT");
	int port = p ? atoi(p) : 47800;
	return (port > 0 && port < 65536) ? port : 47800;
}
void emucap_disconnect() {
	// Request ids are scoped to one TCP session. An unfinished frame command cannot answer on the
	// replacement session before its hello; cancel only request-scoped work and preserve emulator,
	// input-hold, breakpoint, and fatal-quarantine state.
	g_step_id = -1;
	g_step_remaining = 0;
	g_synthetic_fatal_pending = false;
	g_test_adapter_exception_id = -1;
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
// handle()은 아래(요청 디스패치)에 정의 — 동기 핸드셰이크가 hello 요청을 처리하려 전방 선언한다.
void handle(const std::string& line);

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
	sockaddr_in addr;
	memset(&addr, 0, sizeof(addr));
	addr.sin_family = AF_INET;
	addr.sin_port = htons(emucap_port());
	inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
	if (connect(fd, (sockaddr*)&addr, sizeof(addr)) != 0) { emucap_closesock(fd); return; }

	// ── 동기 핸드셰이크(프레임루프 타이밍과 분리) ─────────────────────────────────
	// 서버는 accept 직후 hello 요청 한 줄을 보내고, 우리 응답을 blocking read_line(5s, src/live/tcp.rs
	// handshake_stream)으로 기다린다. GD-ROM 부팅은 인터프리터에서 ~50s로 느려 그동안 프레임루프
	// (vblank→emucap_service)가 굶는다. 핸드셰이크를 O_NONBLOCK + 프레임루프 service에만 맡기면(기존 경로)
	// 그 5s 안에 완전한 hello를 못 주고받아 서버가 불완전한 identity(session_token 없음)로 핸드셰이크해
	// 자기 세션을 foreign으로 오판했다. 그래서 O_NONBLOCK *전에*, connect 호출자(emu 스레드)에서 hello를
	// 원자적·동기적으로 완결한다 — 부팅이 아무리 느려도 핸드셰이크가 굶지 않고 완전한 identity가 즉시 간다.
	// (별도 소켓 스레드를 만들지 않는다 — cross-thread 상태접근 금지가 이 service 모델의 이유. 핸드셰이크도
	// emu 스레드에서.) g_fd를 먼저 세팅해야 handle()→reply_ok→send_line이 이 소켓으로 응답을 보낸다.
	g_fd = fd;
	g_rx.clear();
	g_tx.clear();
	g_tx_pos = 0;
	{
		// 블로킹 recv 타임아웃(SO_RCVTIMEO). 서버는 accept 직후 hello를 보내므로 보통 수 ms; 이 타임아웃은
		// 서버가 bind만 하고 accept 대기자가 없는(idle) 좁은 경우의 상한일 뿐이다. 2s로 잡아 idle-재연결 1회
		// 히치를 줄이되, 서버 handshake read_line(5s)보다 충분히 짧아 폴백 시 서버가 먼저 끊지 않는다.
		emucap_set_rcvtimeo_ms(fd, 2000);
		char tmp[8192];
		bool have_line = false;
		// hello 요청 한 줄(\n 종단)을 받을 때까지 blocking recv로 누적한다. 부분 수신이면 더 받는다.
		// guard는 서버가 \n 없이 바이트만 흘리는 병적 경우의 무한루프 방지(정상 경로는 1회 recv).
		for (int guard = 0; guard < 64 && !have_line; guard++) {
			ssize_t n = recv(fd, tmp, sizeof(tmp), 0);
			if (n <= 0) {
				if (n < 0 && emucap_sock_eintr()) continue;
				break;  // 타임아웃(EAGAIN/EWOULDBLOCK)·피어 종료 → 깨끗이 폴백
			}
			g_rx.append(tmp, (size_t)n);
			if (g_rx.find('\n') != std::string::npos) have_line = true;
		}
		// 받은 완성 라인(정상은 hello 1줄)을 처리하기 전에 nonblocking으로 전환한다. hello는 작아서 보통
		// 한 번에 전송되지만, 송신버퍼가 막혀도 아래 고정 횟수 drain 뒤 연결을 버려 부팅 스레드를 무한히
		// 붙잡지 않는다. 타임아웃으로 한 줄도 못 받았으면 g_rx를 프레임루프 service가 이어 처리한다.
		emucap_set_nonblock(fd);
		size_t pos;
		while ((pos = g_rx.find('\n')) != std::string::npos) {
			std::string l = g_rx.substr(0, pos);
			g_rx.erase(0, pos + 1);
			if (!l.empty()) handle(l);
		}
		for (int guard = 0; guard < 16 && g_fd >= 0 && !g_tx.empty(); guard++) {
			TxFlush status = flush_tx_once();
			if (status == TX_COMPLETE || status == TX_IDLE || status == TX_ERROR) break;
			emucap_sock_wait_ms(1);
		}
		if (g_fd >= 0 && !g_tx.empty()) {
			fprintf(stderr, "emucap: hello TX did not drain within bounded handshake budget\n");
			emucap_disconnect();
		}
	}
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

// ── JSON(최소 추출/생성) ─────────────────────────────────────
std::string json_escape(const std::string& s) {
	std::string o;
	for (char c : s) {
		switch (c) {
		case '"':  o += "\\\""; break;
		case '\\': o += "\\\\"; break;
		case '\n': o += "\\n"; break;
		case '\r': o += "\\r"; break;
		case '\t': o += "\\t"; break;
		default:
			if ((unsigned char)c < 0x20) { char b[8]; snprintf(b, sizeof(b), "\\u%04x", c); o += b; }
			else o += c;
		}
	}
	return o;
}
// "key": "..." 의 문자열 값(최소 이스케이프 해제).
std::string json_str(const std::string& s, const char* key) {
	std::string pat = std::string("\"") + key + "\"";
	size_t k = s.find(pat);
	if (k == std::string::npos) return "";
	size_t c = s.find(':', k + pat.size());
	if (c == std::string::npos) return "";
	size_t i = c + 1;
	while (i < s.size() && (s[i] == ' ' || s[i] == '\t')) i++;
	if (i >= s.size() || s[i] != '"') return "";
	i++;
	std::string out;
	while (i < s.size() && s[i] != '"') {
		if (s[i] == '\\' && i + 1 < s.size()) {
			i++;
			char e = s[i];
			out += (e == 'n') ? '\n' : (e == 't') ? '\t' : e;
		} else {
			out += s[i];
		}
		i++;
	}
	return out;
}
// "key": N 의 숫자 값(strtol base 0 → 0x 16진 자동). envelope id와 params id 구분 위해 from에서 탐색.
bool json_num_from(const std::string& s, const char* key, long& out, size_t from) {
	std::string pat = std::string("\"") + key + "\"";
	size_t k = s.find(pat, from);
	if (k == std::string::npos) return false;
	size_t c = s.find(':', k + pat.size());
	if (c == std::string::npos) return false;
	size_t i = c + 1;
	while (i < s.size() && (s[i] == ' ' || s[i] == '\t' || s[i] == '"')) i++;
	// Windows(LLP64)는 long이 32비트라 strtol이 0x80000000 이상 DC 주소(0x8Cxxxxxx 캐시 미러)를
	// LONG_MAX로 saturate시킨다 — BP가 엉뚱한 주소에 걸리고 disasm이 딴 영역을 푼다. u64로 파싱해
	// 하위 32비트만 남기면 call site의 (uint32_t) 캐스트로 원값이 통과한다(64비트 long은 불변).
	out = (long)strtoull(s.c_str() + i, nullptr, 0);
	return true;
}
bool json_num(const std::string& s, const char* key, long& out) {
	return json_num_from(s, key, out, 0);
}
// "key": true/false/1/0 의 불리언 값(set_trace enabled·watch_register pause_on_hit).
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

// 바이너리를 base64로(screenshot PNG 응답용). 표준 알파벳, 패딩 포함.
std::string base64_encode(const u8* data, size_t len) {
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

void reply_ok(long id, const std::string& result_json) {
	char head[48];
	snprintf(head, sizeof(head), "{\"id\":%ld,\"ok\":true,\"result\":", id);
	send_line(std::string(head) + result_json + "}");
}
void reply_err(long id, const char* kind, const char* msg) {
	char head[96];
	snprintf(head, sizeof(head), "{\"id\":%ld,\"ok\":false,\"error\":{\"kind\":\"%s\",\"message\":\"", id, kind);
	send_line(std::string(head) + json_escape(msg) + "\"}}");
}

bool publish_native_failure(
	const char* operation,
	const char* reason,
	bool active,
	const char* execution_state) noexcept {
	try {
		std::lock_guard<std::mutex> lock(g_failure_artifact_mtx);
		if (g_failure_captured.load()) return false;
		std::string error;
		const bool written = emucap_publish_native_failure(
			"flycast-native",
			EMUCAP_BUILD_HASH,
			g_observed_frame.load(std::memory_order_relaxed),
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
			emucap_sock_wait_ms(1);
		}
	} catch (...) {
	}
}

void contain_service_exception(const char* operation, const char* reason) noexcept {
	const long pending_id =
		g_test_adapter_exception_id >= 0 ? g_test_adapter_exception_id : g_step_id;
	g_frozen = true;
	flush_pending_internal_error(pending_id, reason);
	remember_active_native_failure(operation, reason);
	emucap_disconnect();
}

bool normalize_sync_advance(long id, long& count) {
	if (count < 1) count = 1;
	if (count <= MAX_SYNC_ADVANCE) return true;
	char msg[192];
	snprintf(msg, sizeof(msg),
	         "frame count %ld exceeds synchronous limit %ld; split the request and verify each terminal response",
	         count, MAX_SYNC_ADVANCE);
	reply_err(id, "bad_params", msg);
	return false;
}
// 지연 명령(run_frames/step) 진행 중 keepalive — 서버는 status:"working"을 건너뛰어 요청 타임아웃을
// 방지한다(긴 진행이 5초 읽기 타임아웃에 안 걸리게). 같은 id로 보내야 서버가 같은 호출로 인식한다.
void reply_working(long id) { reply_ok(id, "{\"status\":\"working\"}"); }

// 구조화 에러(균일 인터페이스): 미지원 memory_type은 silent-accept 말고 supported를 동반해
// 거부한다 — 친 시점에 in-context로 자가교정. Rust ProtocolError는 kind+message만 전달하므로(extra
// 필드는 드롭) field/value/supported를 message에 실어 에이전트에 노출한다.
void reply_unsupported_memtype(long id, const std::string& value) {
	std::string msg = "unsupported memory_type \"" + value +
	                  "\"; field=memory_type supported=" + kSupportedMemTypes;
	reply_err(id, "unsupported", msg.c_str());
}

struct DcButton {
	const char* name;
	u32 bit;
	bool canonical;
};

const DcButton kDcButtons[] = {
	{"a", DC_BTN_A, true}, {"b", DC_BTN_B, true}, {"c", DC_BTN_C, true},
	{"x", DC_BTN_X, true}, {"y", DC_BTN_Y, true}, {"z", DC_BTN_Z, true},
	{"d", DC_BTN_D, true}, {"start", DC_BTN_START, true},
	{"enter", DC_BTN_START, false}, {"return", DC_BTN_START, false},
	{"up", DC_DPAD_UP, true}, {"down", DC_DPAD_DOWN, true},
	{"left", DC_DPAD_LEFT, true}, {"right", DC_DPAD_RIGHT, true},
	{nullptr, 0, false},
};

bool dc_button_bit(const std::string& name, u32& bit) {
	for (const DcButton* b = kDcButtons; b->name; b++) {
		if (name == b->name) {
			bit = b->bit;
			return true;
		}
	}
	return false;
}

std::string dc_mask_to_buttons(u32 mask) {
	std::string out = "[";
	u32 seen = 0;
	bool first = true;
	for (const DcButton* b = kDcButtons; b->name; b++) {
		if (!b->canonical || !(mask & b->bit) || (seen & b->bit)) continue;
		if (!first) out += ",";
		out += "\""; out += b->name; out += "\"";
		first = false;
		seen |= b->bit;
	}
	out += "]";
	return out;
}

bool parse_buttons(const std::string& s, u32& mask, std::string& err) {
	mask = 0;
	size_t k = s.find("\"buttons\"");
	if (k == std::string::npos) return true;
	size_t lb = s.find('[', k), rb = s.find(']', k);
	if (lb == std::string::npos || rb == std::string::npos || rb < lb) {
		err = "buttons must be a list";
		return false;
	}
	std::vector<std::string> unknown;
	size_t i = lb + 1;
	while (i < rb) {
		size_t q1 = s.find('"', i);
		if (q1 == std::string::npos || q1 >= rb) break;
		size_t q2 = s.find('"', q1 + 1);
		if (q2 == std::string::npos || q2 > rb) {
			err = "malformed buttons array";
			return false;
		}
		std::string tok = s.substr(q1 + 1, q2 - q1 - 1);
		for (char& c : tok) c = (char)tolower((unsigned char)c);
		u32 bit = 0;
		if (dc_button_bit(tok, bit)) mask |= bit;
		else unknown.push_back(tok);
		i = q2 + 1;
	}
	if (!unknown.empty()) {
		err = "unsupported Dreamcast button";
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

// ── 핸들러 ───────────────────────────────────────────────────
void handle_read_memory(long id, const std::string& line) {
	std::string mt = json_str(line, "memory_type");
	const DCRegion* r = find_region(mt);
	if (!r) { reply_unsupported_memtype(id, mt); return; }
	long addr = 0, len = 0;
	json_num(line, "address", addr);   // region 내 0-based 오프셋
	json_num(line, "length", len);
	if (len <= 0 || len > 0x100000) { reply_err(id, "bad_params", "length 범위(1..0x100000)"); return; }
	if (addr < 0 || (uint64_t)addr + (uint64_t)len > (uint64_t)r->size) {
		char m[128];
		snprintf(m, sizeof(m), "address+length가 %s region 범위(size=0x%X)를 초과", r->mt, (unsigned)r->size);
		reply_err(id, "bad_params", m); return;
	}
	std::string hex;
	hex.reserve((size_t)len * 2);
	for (long i = 0; i < len; i++) {
		u8 b = addrspace::read8((u32)(r->base + addr + i));
		char h[3]; snprintf(h, sizeof(h), "%02x", b);
		hex += h;
	}
	reply_ok(id, std::string("{\"hex\":\"") + hex + "\"}");
}
void handle_write_memory(long id, const std::string& line) {
	std::string mt = json_str(line, "memory_type");
	const DCRegion* r = find_region(mt);
	if (!r) { reply_unsupported_memtype(id, mt); return; }
	long addr = 0;
	json_num(line, "address", addr);   // region 내 0-based 오프셋
	std::string hex = json_str(line, "hex");
	if (hex.size() % 2 != 0) { reply_err(id, "bad_params", "hex는 짝수 길이 hex 문자열이어야"); return; }
	uint64_t nbytes = hex.size() / 2;
	if (addr < 0 || (uint64_t)addr + nbytes > (uint64_t)r->size) {
		char m[128];
		snprintf(m, sizeof(m), "address+hex 길이가 %s region 범위(size=0x%X)를 초과", r->mt, (unsigned)r->size);
		reply_err(id, "bad_params", m); return;
	}
	long n = 0;
	for (size_t i = 0; i + 1 < hex.size(); i += 2) {
		u8 b = (u8)strtol(hex.substr(i, 2).c_str(), nullptr, 16);
		addrspace::write8((u32)(r->base + addr + n), b);
		n++;
	}
	char buf[48];
	snprintf(buf, sizeof(buf), "{\"written\":%ld}", n);
	reply_ok(id, buf);
}
void handle_get_state(long id) {
	// SH-4 주요 레지스터를 cpu.* 키로(리틀엔디언 u32). fr/fpscr는 후속.
	std::string s = "{\"state\":{";
	// Four decimal u32 register fields plus JSON syntax can exceed 64 bytes. Truncating this buffer
	// silently dropped cpu.dbr while still leaving parseable JSON after the next append.
	char t[256];
	for (int i = 0; i < 16; i++) {
		snprintf(t, sizeof(t), "%s\"cpu.r%d\":%u", (i ? "," : ""), i, Sh4cntx.r[i]);
		s += t;
	}
	snprintf(t, sizeof(t), ",\"cpu.pc\":%u,\"cpu.pr\":%u", Sh4cntx.pc, Sh4cntx.pr); s += t;
	snprintf(t, sizeof(t), ",\"cpu.sr\":%u,\"cpu.gbr\":%u,\"cpu.vbr\":%u", Sh4cntx.sr.getFull(), Sh4cntx.gbr, Sh4cntx.vbr); s += t;
	snprintf(t, sizeof(t), ",\"cpu.ssr\":%u,\"cpu.spc\":%u,\"cpu.sgr\":%u,\"cpu.dbr\":%u", Sh4cntx.ssr, Sh4cntx.spc, Sh4cntx.sgr, Sh4cntx.dbr); s += t;
	snprintf(t, sizeof(t), ",\"cpu.mach\":%u,\"cpu.macl\":%u,\"cpu.fpul\":%u", Sh4cntx.mac.h, Sh4cntx.mac.l, Sh4cntx.fpul); s += t;
	s += "}}";
	reply_ok(id, s);
}

// save/load_state: emucap 프로토콜은 path 기반(probe/regression). Flycast dc_savestate는 인덱스 기반이라
// raw Serializer/Deserializer로 내 path에 직접 쓴다(zip/헤더 우회, emucap 자족). vblank/frozen에서 호출되어
// 프레임 경계라 상태 일관(dc_serialize/emu.loadstate 안전).
void handle_save_state(long id, const std::string& line) {
	std::string path = json_str(line, "path");
	if (path.empty()) { reply_err(id, "bad_params", "path 필요"); return; }
	try {
		Serializer sizer;            // 1패스: 크기 산출
		dc_serialize(sizer);
		size_t sz = sizer.size();
		std::vector<u8> buf(sz);
		Serializer ser(buf.data(), sz);
		dc_serialize(ser);           // 2패스: 실제 직렬화
		FILE* f = fopen(path.c_str(), "wb");
		if (!f) { reply_err(id, "io_error", "파일 열기 실패"); return; }
		size_t w = fwrite(buf.data(), 1, sz, f);
		fclose(f);
		if (w != sz) { reply_err(id, "io_error", "쓰기 실패"); return; }
	} catch (std::exception& e) { reply_err(id, "io_error", e.what()); return; }
	reply_ok(id, "{\"status\":\"completed\"}");
}
void handle_load_state(long id, const std::string& line) {
	std::string path = json_str(line, "path");
	if (path.empty()) { reply_err(id, "bad_params", "path 필요"); return; }
	try {
		FILE* f = fopen(path.c_str(), "rb");
		if (!f) { reply_err(id, "io_error", "파일 열기 실패"); return; }
		fseek(f, 0, SEEK_END); long sz = ftell(f); fseek(f, 0, SEEK_SET);
		if (sz <= 0) { fclose(f); reply_err(id, "io_error", "빈/잘못된 파일"); return; }
		std::vector<u8> buf((size_t)sz);
		size_t r = fread(buf.data(), 1, (size_t)sz, f);
		fclose(f);
		if ((long)r != sz) { reply_err(id, "io_error", "읽기 실패"); return; }
		Deserializer deser(buf.data(), (size_t)sz);
		emu.loadstate(deser);
	} catch (std::exception& e) { reply_err(id, "io_error", e.what()); return; }
	{
		std::lock_guard<std::mutex> lk(g_fb_mtx);
		g_fb_fresh = false;
	}
	reply_ok(id, "{\"status\":\"completed\"}");
}

// ── breakpoint(exec) ─────────────────────────────────────────
// SH-4 P0..P3 미러(같은 29비트 외부주소의 0x0C/0x8C/0xAC/0xCC… 폼)를 정규화된 29비트 폼으로 접는다.
// exec BP는 PC와 BP 양쪽을 이 폼으로 접어 비교하므로, get_state가 노출하는 PC 폼(캐시미러 0x8Cxxxxxx)이
// 아닌 언캐시드/물리 미러(0xAC/0x0C…)로 줘도 같은 명령에 걸린다(mirror-form BP의 조용한 미발화 방지).
// P4(온칩 0xE0000000+, 스토어큐·제어레지스터)는 PC가 실행하지 않으므로 접지 않는다.
static inline uint32_t sh4_fold_pc(uint32_t a) {
	return (a < 0xE0000000u) ? (a & 0x1FFFFFFFu) : a;
}
// g_bps에서 빠른 조회 집합 g_bp_addrs와 armed 플래그를 재구성한다(set/clear 후 호출).
// 조회 집합은 fold된 29비트 폼으로 채운다(EmuBp.addr는 list용으로 사용자가 준 원 폼을 보존).
void rearm_breakpoints() {
	g_bp_addrs.clear();
	for (const auto& b : g_bps) g_bp_addrs.insert(sh4_fold_pc(b.addr));
	g_emucap_bp_armed = !g_bp_addrs.empty();
}
void handle_set_breakpoint(long id, const std::string& line) {
	// exec(PC) BP만 지원. read/write 워치포인트는 메모리 접근 훅이 필요 → 미지원(거부).
	std::string kind = json_str(line, "kind");
	if (!kind.empty() && kind != "exec" && kind != "pc") {
		reply_err(id, "unsupported", "이 어댑터는 exec BP만 지원(read/write 워치포인트는 GDB-브리지)");
		return;
	}
	long start = 0;
	if (!json_num(line, "start", start)) json_num(line, "address", start);  // start 우선, 없으면 address
	long bid = g_bp_next_id++;
	g_bps.push_back(EmuBp{bid, (uint32_t)start});
	rearm_breakpoints();
	char buf[48];
	snprintf(buf, sizeof(buf), "{\"id\":%ld,\"set\":true}", bid);
	reply_ok(id, buf);
}
void handle_clear_breakpoint(long id, const std::string& line) {
	long bid = -1;
	// 봉투 id가 첫째라 둘째 "id"가 인자 — 최소 파서 한계로 재탐색.
	size_t first = line.find("\"id\"");
	if (first != std::string::npos) json_num_from(line, "id", bid, first + 4);
	size_t before = g_bps.size();
	std::vector<EmuBp> keep;
	for (const auto& b : g_bps) if (b.id != bid) keep.push_back(b);
	g_bps.swap(keep);
	rearm_breakpoints();
	char buf[48];
	snprintf(buf, sizeof(buf), "{\"cleared\":%zu}", before - g_bps.size());
	reply_ok(id, buf);
}
void handle_list_breakpoints(long id) {
	std::string arr = "[";
	for (size_t i = 0; i < g_bps.size(); i++) {
		char b[80];
		snprintf(b, sizeof(b), "%s{\"id\":%ld,\"kind\":\"exec\",\"start\":%u,\"end\":%u}",
		         i ? "," : "", g_bps[i].id, (unsigned)g_bps[i].addr, (unsigned)g_bps[i].addr);
		arr += b;
	}
	arr += "]";
	reply_ok(id, "{\"breakpoints\":" + arr + "}");
}
void handle_poll_events(long id) {
	std::string arr = "[";
	for (size_t i = 0; i < g_bp_hits.size(); i++) {
		char b[64];
		snprintf(b, sizeof(b), "%s{\"pc\":%u,\"registers\":", i ? "," : "", (unsigned)g_bp_hits[i].pc);
		arr += b;
		arr += g_bp_hits[i].registers;  // {name:value} JSON(히트 순간 CPU 레지스터)
		arr += "}";
	}
	arr += "]";
	g_bp_hits.clear();
	reply_ok(id, "{\"events\":" + arr + ",\"dropped\":0}");
}

// ── find_pattern / disassemble / get_rom_info(어댑터-위임 도구) ──
void handle_find_pattern(long id, const std::string& line) {
	std::string mt = json_str(line, "memory_type");
	const DCRegion* r = find_region(mt);
	if (!r) { reply_unsupported_memtype(id, mt); return; }
	std::string hex = json_str(line, "hex");
	long start = 0, length = 0, max_matches = 256, align = 1;   // start=region 내 0-based 오프셋
	json_num(line, "start", start);
	bool has_length = json_num(line, "length", length);
	json_num(line, "max_matches", max_matches);
	json_num(line, "align", align);
	if (hex.empty() || hex.size() % 2 != 0) { reply_err(id, "bad_params", "hex는 짝수 길이 hex 문자열"); return; }
	if (align < 1) align = 1;
	if (max_matches < 1) max_matches = 256;
	if (start < 0 || (uint64_t)start >= (uint64_t)r->size) {
		char m[128];
		snprintf(m, sizeof(m), "start가 %s region 범위(size=0x%X)를 초과", r->mt, (unsigned)r->size);
		reply_err(id, "bad_params", m); return;
	}
	// length 미지정/<=0 → region의 남은 끝까지. 어댑터-내부 스캔이라 16MB도 ms 수준이므로 한 호출
	// 16MB까지 스캔한다(DC ram 16MB는 1콜로 전부; 매치 리스트는 max_matches·output_path가 제어).
	uint64_t available = (uint64_t)r->size - (uint64_t)start;
	uint64_t requested = (has_length && length > 0) ? (uint64_t)length : available;
	if (requested > available) requested = available;
	bool truncated = requested > 0x1000000ull;      // 한 호출 최대 16MB(초과 시 start를 옮겨 청크)
	uint64_t scan_len = truncated ? 0x1000000ull : requested;
	std::vector<u8> pat;
	for (size_t i = 0; i + 1 < hex.size(); i += 2) pat.push_back((u8)strtol(hex.substr(i, 2).c_str(), nullptr, 16));
	std::vector<u8> buf((size_t)scan_len);
	for (uint64_t i = 0; i < scan_len; i++) buf[(size_t)i] = addrspace::read8((u32)(r->base + start + i));
	std::string matches = "[";
	long count = 0;
	if (pat.size() <= scan_len) {
		for (uint64_t off = 0; off + pat.size() <= scan_len; off++) {
			if (align > 1 && (((uint64_t)start + off) % (uint64_t)align) != 0) continue;
			bool m = true;
			for (size_t k = 0; k < pat.size(); k++) if (buf[(size_t)off + k] != pat[k]) { m = false; break; }
			if (!m) continue;
			if (count < max_matches) {
				// 오프셋은 region 0-based 기준(read_memory(memory_type, address=오프셋)과 같은 주소계).
				char b[24]; snprintf(b, sizeof(b), "%s%llu", count ? "," : "", (unsigned long long)((uint64_t)start + off));
				matches += b;
				count++;
			} else { truncated = true; break; }
		}
	}
	matches += "]";
	char tail[112];
	snprintf(tail, sizeof(tail), ",\"count\":%ld,\"truncated\":%s,\"scanned\":%llu,\"start\":%ld}",
	         count, truncated ? "true" : "false", (unsigned long long)scan_len, start);
	reply_ok(id, "{\"matches\":" + matches + tail);
}
void handle_disassemble(long id, const std::string& line) {
	long addr = 0, count = 8;
	json_num(line, "address", addr);
	json_num(line, "count", count);
	if (count < 1) count = 1;
	if (count > 256) count = 256;
	std::string out = "[";
	for (long i = 0; i < count; i++) {
		u32 a = (u32)(addr + i * 2);                 // SH4 명령은 2바이트 고정
		u16 op = addrspace::read16(a);
		char buf[128]; buf[0] = 0;
		if (OpDesc[op]) OpDesc[op]->Disassemble(buf, a, op);
		char ab[20]; snprintf(ab, sizeof(ab), "0x%08X", (unsigned)a);
		out += i ? ",{\"addr\":\"" : "{\"addr\":\"";
		out += ab; out += "\",\"text\":\"";
		for (char* p = buf; *p; p++) {                // JSON 이스케이프
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
void handle_get_rom_info(long id) {
	std::string s = "{\"gameId\":\"" + json_escape(settings.content.gameId) +
	                "\",\"title\":\"" + json_escape(settings.content.title) +
	                "\",\"fileName\":\"" + json_escape(settings.content.fileName) + "\",\"system\":\"dreamcast\"}";
	reply_ok(id, s);
}

// ── 크래시경로 관측 헬퍼(trace/watch/callstack 공용) ──────────────────────────
// trace/watch armed 플래그를 flag에서 재계산한다(set_trace/watch_register 후 + watch 1회성 히트 시 호출).
// 셋 다 off면 armed=false → 인터프리터 핫루프 훅 비용 0.
void rebuild_trace_armed() { g_emucap_trace_armed = g_trace_enabled || g_watch_enabled; }

// SH-4 레지스터를 이름으로 읽는다(watch_register용). get_state가 노출하는 "cpu.r0".."cpu.r15"/"cpu.pc" 등을
// 대소문자 무시·"cpu." 접두 무시로 매칭한다("r15"/"cpu.r15"/"pc"/"sp"(=r15) 모두 수용). Sh4cntx 직접 매핑
// (Flycast는 디버거 RegGroups가 없어 Mednafen의 read_register_by_name을 이 매핑으로 대체). 못 찾으면 false.
bool emucap_read_reg(const std::string& want, uint32_t& out) {
	std::string n;
	for (char c : want) n += (char)tolower((unsigned char)c);
	if (n.rfind("cpu.", 0) == 0) n = n.substr(4);  // "cpu." 접두 제거
	if (n.size() >= 2 && n[0] == 'r' && isdigit((unsigned char)n[1])) {
		char* end = nullptr;
		long idx = strtol(n.c_str() + 1, &end, 10);
		if (end && *end == 0 && idx >= 0 && idx <= 15) { out = Sh4cntx.r[idx]; return true; }
	}
	if (n == "sp")   { out = Sh4cntx.r[15]; return true; }  // SH-4 SP = R15
	if (n == "pc")   { out = Sh4cntx.pc;   return true; }
	if (n == "pr")   { out = Sh4cntx.pr;   return true; }
	if (n == "sr")   { out = Sh4cntx.sr.getFull(); return true; }
	if (n == "gbr")  { out = Sh4cntx.gbr;  return true; }
	if (n == "vbr")  { out = Sh4cntx.vbr;  return true; }
	if (n == "ssr")  { out = Sh4cntx.ssr;  return true; }
	if (n == "spc")  { out = Sh4cntx.spc;  return true; }
	if (n == "sgr")  { out = Sh4cntx.sgr;  return true; }
	if (n == "dbr")  { out = Sh4cntx.dbr;  return true; }
	if (n == "mach") { out = Sh4cntx.mac.h; return true; }
	if (n == "macl") { out = Sh4cntx.mac.l; return true; }
	if (n == "fpul") { out = Sh4cntx.fpul; return true; }
	return false;
}

// PC의 SH-4 명령을 call/return/other로 분류한다(shadow stack call_stack용). SH-4는 SH-2와 동일 인코딩:
// BSR (op&0xF000)==0xB000, BSRF Rn (op&0xF0FF)==0x0003, JSR @Rn (op&0xF0FF)==0x400B → CALL; RTS 0x000B → RETURN.
enum CallKind { CK_OTHER = 0, CK_CALL, CK_RETURN };
CallKind emucap_classify(uint32_t pc) {
	u16 op = addrspace::read16(pc);
	if ((op & 0xF000) == 0xB000) return CK_CALL;   // BSR label
	if ((op & 0xF0FF) == 0x0003) return CK_CALL;   // BSRF Rn
	if ((op & 0xF0FF) == 0x400B) return CK_CALL;   // JSR @Rn
	if (op == 0x000B) return CK_RETURN;            // RTS
	return CK_OTHER;
}

// pc 1명령을 디스어셈블 텍스트로(get_trace/call_stack용, disassemble와 동일 OpDesc 경로). JSON 이스케이프는 호출부.
std::string emucap_disasm_text(uint32_t a) {
	char buf[128]; buf[0] = 0;
	u16 op = addrspace::read16(a);
	if (OpDesc[op]) OpDesc[op]->Disassemble(buf, a, op);
	return std::string(buf);
}

// set_trace(enabled): 실행추적 켜기/끄기. 켜면 인터프리터 훅이 매 명령 PC를 링버퍼에 기록한다(hunting 전용,
// 매 명령이라 느림 — 끝나면 끈다). 켤 때 링·shadow stack을 초기화한다.
void handle_set_trace(long id, const std::string& line) {
	bool enabled = false;
	json_bool(line, "enabled", enabled);
	g_trace_enabled = enabled;
	if (enabled) {
		g_trace_ring.assign(TRACE_CAP, 0);  // 링버퍼 확보·초기화
		g_trace_head = 0;
		g_trace_count = 0;
		g_callstack.clear();                // shadow stack도 새로 시작(추적 시작 이후의 call/return만 반영)
	}
	rebuild_trace_armed();  // armed 재계산(trace 활성→훅 무장, 비활성→watch도 없으면 비용 0)
	reply_ok(id, std::string("{\"enabled\":") + (enabled ? "true" : "false") + "}");
}

// get_trace(count): fatal quarantine에서는 always-on 512-PC ring, 평상시에는 opt-in hunting ring을 반환.
void handle_get_trace(long id, const std::string& line) {
	long count = 256;
	json_num(line, "count", count);
	if (count < 1) count = 1;
	const bool crash_ring = g_failure_captured.load();
	const uint64_t crash_sequence = g_emucap_crash_pc_sequence;
	const size_t crash_count = (size_t)std::min<uint64_t>(crash_sequence, EMUCAP_CRASH_PC_CAP);
	const size_t available = crash_ring ? crash_count : g_trace_count;
	const size_t capacity = crash_ring ? EMUCAP_CRASH_PC_CAP : TRACE_CAP;
	const size_t head = crash_ring
		? (size_t)(crash_sequence & (EMUCAP_CRASH_PC_CAP - 1)) : g_trace_head;
	size_t want = (size_t)count;
	if (want > available) want = available;
	std::string out = std::string("{\"trace_scope\":\"")
		+ (crash_ring ? "interpreter" : "opt_in") + "\",\"trace\":[";
	for (size_t i = 0; i < want; i++) {
		// 최근 want개: 링에서 (head-want)..(head-1) 순서. head는 다음 쓸 위치.
		size_t idx = (head + capacity - want + i) % capacity;
		uint32_t pc = crash_ring ? g_emucap_crash_pc_ring[idx] : g_trace_ring[idx];
		char pcbuf[40];
		snprintf(pcbuf, sizeof(pcbuf), "%s{\"pc\":%u,\"text\":\"", i ? "," : "", (unsigned)pc);
		out += pcbuf;
		out += json_escape(emucap_disasm_text(pc));
		out += "\"}";
	}
	out += "]}";
	reply_ok(id, out);
}

// watch_register(register, min, max, pause_on_hit): register가 [min,max]를 벗어나는 명령에서 freeze한다
// (SP 폭주 등 derail 포착). register 이름은 get_state의 "cpu.r15"/"r15"/"sp"/"pc" 등. 매 명령 검사라 hunting 전용.
void handle_watch_register(long id, const std::string& line) {
	std::string reg = json_str(line, "register");
	if (reg.empty()) { reply_err(id, "bad_params", "register 필요"); return; }
	uint32_t probe;
	if (!emucap_read_reg(reg, probe)) {
		std::string m = "register '" + reg +
		                "'를 찾을 수 없다 — 유효 이름은 get_state로 확인(cpu.r0..cpu.r15/pc/pr/sr/gbr/vbr/sp 등)";
		reply_err(id, "bad_params", m.c_str());
		return;
	}
	long mn = 0, mx = 0;
	json_num(line, "min", mn);
	json_num(line, "max", mx);
	bool pause = true;
	json_bool(line, "pause_on_hit", pause);
	g_watch_reg = reg;
	g_watch_min = (uint32_t)mn;
	g_watch_max = (uint32_t)mx;
	g_watch_pause = pause;
	g_watch_enabled = true;
	rebuild_trace_armed();
	char buf[160];
	snprintf(buf, sizeof(buf), "{\"watching\":\"%s\",\"min\":%u,\"max\":%u}", json_escape(reg).c_str(),
	         (unsigned)g_watch_min, (unsigned)g_watch_max);
	reply_ok(id, buf);
}

// call_stack(): 현재 shadow stack(call-site PC 체인, 바깥→안)을 [{pc,text}]로 반환한다. set_trace(true)
// 선행 필요 — 추적 시작 이후의 call/return만 반영하며 스택 메모리 손상과 독립적이다.
void handle_call_stack(long id) {
	std::string out = "{\"call_stack\":[";
	for (size_t i = 0; i < g_callstack.size(); i++) {
		uint32_t pc = g_callstack[i].pc;  // g_callstack[0]=가장 바깥, back()=가장 안
		char pcbuf[40];
		snprintf(pcbuf, sizeof(pcbuf), "%s{\"pc\":%u,\"text\":\"", i ? "," : "", (unsigned)pc);
		out += pcbuf;
		out += json_escape(emucap_disasm_text(pc));
		out += "\"}";
	}
	out += "]}";
	reply_ok(id, out);
}

void handle(const std::string& line) {
	std::string method = json_str(line, "method");
	long id = 0;
	json_num(line, "id", id);
	if (g_failure_active && !failure_method_allowed(method)) {
		reply_err(id, "crashed", "Flycast is quarantined at a fatal SH4 exception; mutation refused");
		return;
	}
	try {
		if (method == "hello") {
			std::string r = "{\"protocol_version\":1,\"system\":\"dreamcast\",\"adapter\":\"flycast-native\",\"build\":\"" EMUCAP_BUILD_HASH "\",\"name\":\"";
			const char* nm = getenv("EMUCAP_NAME");
			r += json_escape((nm && nm[0]) ? std::string(nm) : std::string(PROTOCOL_NAME));
			r += "\",\"methods\":[\"hello\",\"status\",\"read_memory\",\"write_memory\",\"get_state\","
			     "\"save_state\",\"load_state\",\"run_frames\",\"screenshot\","
			     "\"set_input\",\"pause\",\"resume\",\"step\",\"reset\","
			     "\"set_breakpoint\",\"clear_breakpoint\",\"clear_all_breakpoints\",\"list_breakpoints\",\"poll_events\","
			     "\"find_pattern\",\"disassemble\",\"get_rom_info\","
			     "\"set_trace\",\"get_trace\",\"watch_register\",\"call_stack\",\"dismiss_failure\"";
			if (env_enabled("EMUCAP_ENABLE_TEST_FATAL")) r += ",\"test_fatal\"";
			if (env_enabled("EMUCAP_ENABLE_TEST_ADAPTER_EXCEPTION"))
				r += ",\"test_adapter_exception\"";
			r += "],"
			     // Advertise the memory types accepted by read_memory, write_memory, and find_pattern.
			     "\"memory_types\":[\"ram\",\"vram\",\"aica\"],"
			     "\"breakpoint_kinds\":[{\"kind\":\"exec\",\"range_unit\":\"address\","
			     "\"range_mode\":\"exact\",\"memory_type_used\":false,\"snapshot\":false}],"
			     "\"execution_limits\":{\"max_sync_advance_count\":";
			r += std::to_string(MAX_SYNC_ADVANCE);
			r += "},\"contracts\":{\"catalog\":\"emucap-feature-contracts/v3\","
			     "\"active_exceptions\":[\"flycast.execution.instruction-step-absent\","
			     "\"flycast.call-stack.best-effort\",\"flycast.input-hold.port-zero-only\"]}}";
			const char* tok = getenv("EMUCAP_SESSION_TOKEN");
			if (tok && tok[0] && !r.empty() && r.back() == '}') {
				std::string s(tok);
				if (s.size() > 256) s.resize(256);
				r.pop_back();
				r += ",\"session_token\":\"" + json_escape(s) + "\"}";
			}
			const char* content = getenv("EMUCAP_CONTENT");
			if (content && content[0] && !r.empty() && r.back() == '}') {
				std::string s(content);
				if (s.size() > 512) s.resize(512);
				r.pop_back();
				r += ",\"content\":\"" + json_escape(s) + "\"}";
			}
			const char* launch_id = getenv("EMUCAP_LAUNCH_ID");
			if (launch_id && launch_id[0] && !r.empty() && r.back() == '}') {
				std::string s(launch_id);
				if (s.size() > 128) s.resize(128);
				r.pop_back();
				r += ",\"launch_id\":\"" + json_escape(s) + "\"}";
			}
			reply_ok(id, r);
		} else if (method == "status") {
			std::string state = g_failure_captured.load() ? "crashed" : (g_frozen ? "frozen" : "running");
			std::string result = "{\"connected\":true,\"frame\":" + std::to_string(g_frame)
				+ ",\"state\":\"" + state + "\",\"adapter\":\"flycast\""
				+ ",\"input_override\":{\"observable\":true,\"engaged\":"
				+ (g_input_override.engaged() ? std::string("true") : std::string("false"))
				+ ",\"mode\":\"" + (g_input_override.engaged() ? std::string("persistent") : std::string("native"))
				+ "\",\"pressed_mask\":" + std::to_string(g_input_override.pressed_mask()) + "}"
				+ ",\"execution_limits\":{\"max_sync_advance_count\":" + std::to_string(MAX_SYNC_ADVANCE) + "}";
			{
				std::lock_guard<std::mutex> lk(g_fb_mtx);
				result += std::string(",\"framebuffer_fresh\":") + (g_fb_fresh ? "true" : "false");
			}
			if (g_failure_captured.load()) {
				result += ",\"reason\":\"" + json_escape(g_failure_reason) + "\""
					+ std::string(",\"failure_context_available\":")
					+ (g_failure_file_written ? "true" : "false")
					+ ",\"quarantine_active\":" + (g_failure_active ? std::string("true") : std::string("false"))
					+ ",\"dismissed\":" + (g_failure_dismissed ? std::string("true") : std::string("false"))
					+ ",\"epc\":" + std::to_string(g_failure_epc)
					+ ",\"incoming_event\":" + std::to_string(g_failure_event)
					+ ",\"trace_scope\":\"interpreter\"";
			}
			if (g_internal_failure_active) {
				result += ",\"adapter_failure_active\":true,\"adapter_failure_operation\":\""
					+ json_escape(g_internal_failure_operation) + "\"";
			}
			result += std::string(",\"frame_capture_available\":")
				+ (g_capture_disabled.load() ? "false" : "true");
			result += "}";
			reply_ok(id, result);
			recover_native_failure_after_status();
		} else if (method == "dismiss_failure") {
			if (!g_failure_active) {
				reply_err(id, "no_active_failure", "no active fatal quarantine");
			} else {
				reply_ok(id, std::string("{\"dismissed\":true,\"process_will_exit\":")
					+ (g_failure_synthetic ? "false" : "true") + "}");
				for (int guard = 0; guard < 100 && g_fd >= 0 && !g_tx.empty(); guard++) {
					TxFlush status = flush_tx_once();
					if (status == TX_COMPLETE || status == TX_IDLE || status == TX_ERROR) break;
					emucap_sock_wait_ms(1);
				}
				g_failure_dismissed = true;
				g_failure_active = false;
			}
		} else if (method == "test_fatal" && env_enabled("EMUCAP_ENABLE_TEST_FATAL")) {
			g_synthetic_fatal_pending = true;
			reply_ok(id, "{\"scheduled\":true}");
		} else if (
			method == "test_adapter_exception"
			&& env_enabled("EMUCAP_ENABLE_TEST_ADAPTER_EXCEPTION")) {
			g_test_adapter_exception_id = id;
		} else if (method == "read_memory") {
			handle_read_memory(id, line);
		} else if (method == "write_memory") {
			handle_write_memory(id, line);
		} else if (method == "get_state") {
			handle_get_state(id);
		} else if (method == "save_state") {
			handle_save_state(id, line);
		} else if (method == "load_state") {
			handle_load_state(id, line);
		} else if (method == "run_frames") {
			// N프레임 진행 후 완료 응답(지연 — emucap_service가 카운트다운). run_frames의
			// terminal state는 항상 running; frozen으로 끝내는 exact advance는 step이 소유한다.
			long n = 1;
			json_num(line, "n", n);
			if (!normalize_sync_advance(id, n)) return;
			g_frozen = false;
			g_step_id = id;
			g_step_remaining = n;
		} else if (method == "set_input") {
			long port = 0;
			if (json_num(line, "port", port) && port != 0) {
				reply_err(id, "bad_params", "Flycast input supports only controller port 0");
				return;
			}
			// 홀드: Maple 소비 지점에서 kcode를 덮어쓴다. 빈 배열은 0 강제가 아니라 네이티브 입력권 반환.
			u32 mask = 0;
			std::string input_err;
			if (!parse_buttons(line, mask, input_err)) { reply_err(id, "bad_params", input_err.c_str()); return; }
			g_input_override.set(mask);
			char rbuf[256];
			snprintf(rbuf, sizeof(rbuf), "{\"applied\":true,\"applied_mask\":\"0x%08x\",\"applied_buttons\":%s}",
			         (unsigned)mask, dc_mask_to_buttons(mask).c_str());
			reply_ok(id, rbuf);
		} else if (method == "pause") {
			g_frozen = true;
			reply_ok(id, "{\"state\":\"frozen\"}");
		} else if (method == "resume") {
			g_frozen = false;
			g_step_remaining = 0;
			g_step_id = -1;
			reply_ok(id, "{\"state\":\"running\"}");
		} else if (method == "step") {
			// 프레임 step(frozen에서). 명령 단위는 vblank-스핀 freeze 모델로 불가 → 명시 거부
			// (조용히 프레임 진행하면 1명령씩 좁히려던 호출이 수천 명령을 건너뛴다).
			std::string unit = json_str(line, "unit");
			if (unit == "instructions") {
				reply_err(id, "unsupported", "이 어댑터는 명령 단위 step 미지원 — step(frames)을 쓰라");
				return;
			}
			long frames = 1;
			json_num(line, "frames", frames);
			if (!normalize_sync_advance(id, frames)) return;
			g_frozen = true;
			g_step_id = id;        // 완료 응답은 emucap_service가 frames 경과 후
			g_step_remaining = frames;
		} else if (method == "step_instructions") {
			reply_err(id, "unsupported", "이 어댑터는 명령 단위 step 미지원(vblank-프레임 freeze) — step(frames)/run_frames를 쓰라");
		} else if (method == "reset") {
			emu.requestReset();
			reply_ok(id, "{\"reset\":true}");
		} else if (method == "screenshot") {
			if (g_capture_disabled.load()) {
				reply_err(
					id,
					"internal_error",
					"frame capture was disabled after a native adapter exception; inspect get_failure_context");
				return;
			}
			// 연속 버퍼에서 즉시 PNG 인코딩(emu 스레드, GL 불필요). UI 스레드가 매 렌더마다 raw를 채워두므로
			// frozen서도 동작한다(버퍼=freeze 직전 프레임=frozen 상태). gui_runOnUiThread/지연은 freeze 중
			// UI 렌더가 막혀 데드락이라 쓰지 않는다.
			std::vector<u8> raw; int w = 0, h = 0; bool fresh = false;
			{
				std::lock_guard<std::mutex> lk(g_fb_mtx);
				raw = g_fb_raw; w = g_fb_w; h = g_fb_h; fresh = g_fb_fresh;
			}
			if (raw.empty() || w <= 0 || h <= 0) { reply_err(id, "no_frame", "렌더된 프레임 없음(게임 미시작?)"); return; }
			if (!fresh) {
				reply_err(id, "bad_state",
					"load_state 이후 새 렌더 프레임이 아직 없음; step(1) 또는 resume 후 다시 캡처");
				return;
			}
			std::vector<u8> png;
			emucap_encode_png(raw.data(), w, h, png);
			if (png.empty()) { reply_err(id, "io_error", "PNG 인코딩 실패"); return; }
			reply_ok(id, std::string("{\"png_base64\":\"") + base64_encode(png.data(), png.size())
				+ "\",\"freshness\":\"current\",\"frame\":" + std::to_string(g_frame) + "}");
		} else if (method == "set_breakpoint") {
			handle_set_breakpoint(id, line);
		} else if (method == "clear_breakpoint") {
			handle_clear_breakpoint(id, line);
		} else if (method == "clear_all_breakpoints") {
			g_bps.clear();
			rearm_breakpoints();
			reply_ok(id, "{\"cleared\":true}");
		} else if (method == "list_breakpoints") {
			handle_list_breakpoints(id);
		} else if (method == "poll_events") {
			handle_poll_events(id);
		} else if (method == "find_pattern") {
			handle_find_pattern(id, line);
		} else if (method == "disassemble") {
			handle_disassemble(id, line);
		} else if (method == "get_rom_info") {
			handle_get_rom_info(id);
		} else if (method == "set_trace") {
			handle_set_trace(id, line);
		} else if (method == "get_trace") {
			handle_get_trace(id, line);
		} else if (method == "watch_register") {
			handle_watch_register(id, line);
		} else if (method == "call_stack") {
			handle_call_stack(id);
		} else {
			reply_err(id, "unknown_method", method.c_str());
		}
	} catch (const std::exception& e) {
		reply_err(id, "internal_error", e.what());
	} catch (...) {
		reply_err(id, "internal_error", "알 수 없는 예외");
	}
}

void serve_socket_once() {
	if (g_fd < 0) return;
	if (!g_tx.empty()) {
		flush_tx_once();
		if (g_fd < 0 || !g_tx.empty()) return;
	}
	char tmp[8192];
	ssize_t n = recv(g_fd, tmp, sizeof(tmp), 0);
	if (n == 0) { emucap_disconnect(); return; }  // 피어 종료(FIN)
	if (n < 0) {
		// 논블로킹이라 EAGAIN/EWOULDBLOCK은 "데이터 없음"(정상). 그 외(ECONNRESET 등)는 죽은 링크다 —
		// 끊어 g_fd<0로 만들어 재연결을 유도한다. 안 그러면 RST 시 frozen 스핀이 영영 빠져나오지 못한다
		// (서버 P0 타임아웃 드롭이 unread 바이트 때문에 FIN이 아닌 RST를 보내는 케이스가 정확히 이것).
		if (emucap_sock_wouldblock()) return;
		emucap_disconnect();
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

}  // namespace

void emucap_capture_fatal_sh4(
	const char* reason,
	uint32_t epc,
	uint32_t incoming_event,
	uint32_t existing_expevt,
	uint32_t existing_intevt,
	uint32_t tea) noexcept {
	// Copy all SH4 and ring fields before any file, socket, allocation-heavy, or quarantine work.
	EmucapSh4FailureSnapshot snapshot;
	snapshot.observed_at_unix_ms = unix_time_ms();
	snapshot.frame = g_frame;
	snapshot.epc = epc;
	snapshot.incoming_event = incoming_event;
	snapshot.existing_expevt = existing_expevt;
	snapshot.existing_intevt = existing_intevt;
	snapshot.tea = tea;
	for (size_t i = 0; i < snapshot.r.size(); i++) snapshot.r[i] = Sh4cntx.r[i];
	for (size_t i = 0; i < snapshot.r_bank.size(); i++) snapshot.r_bank[i] = Sh4cntx.r_bank[i];
	snapshot.pc = Sh4cntx.pc;
	snapshot.pr = Sh4cntx.pr;
	snapshot.gbr = Sh4cntx.gbr;
	snapshot.vbr = Sh4cntx.vbr;
	snapshot.mach = Sh4cntx.mac.h;
	snapshot.macl = Sh4cntx.mac.l;
	snapshot.sr = Sh4cntx.sr.getFull();
	snapshot.ssr = Sh4cntx.ssr;
	snapshot.spc = Sh4cntx.spc;
	snapshot.sgr = Sh4cntx.sgr;
	snapshot.dbr = Sh4cntx.dbr;
	snapshot.fpul = Sh4cntx.fpul;
	snapshot.fpscr = Sh4cntx.fpscr.full;
	for (size_t i = 0; i < EMUCAP_CRASH_PC_CAP; i++)
		snapshot.pc_ring[i] = g_emucap_crash_pc_ring[i];
	const uint64_t crash_sequence = g_emucap_crash_pc_sequence;
	snapshot.pc_ring_head = (size_t)(crash_sequence & (EMUCAP_CRASH_PC_CAP - 1));
	snapshot.pc_ring_count = (size_t)std::min<uint64_t>(crash_sequence, EMUCAP_CRASH_PC_CAP);

	g_failure_active = true;
	{
		std::lock_guard<std::mutex> lock(g_failure_artifact_mtx);
		g_failure_captured.store(true);
	}
	g_failure_dismissed = false;
	g_failure_synthetic = incoming_event == 0xFFFFFFFFu;
	g_failure_shutdown_requested.store(false);
	g_failure_file_written = false;
	g_failure_epc = epc;
	g_failure_event = incoming_event;
	try {
		g_failure_reason = reason != nullptr ? reason : "Fatal SH4 exception";
		snapshot.reason = g_failure_reason;
		const char* launch_id = getenv("EMUCAP_LAUNCH_ID");
		snapshot.launch_id = launch_id != nullptr ? launch_id : "";
		snapshot.emulator_build = EMUCAP_BUILD_HASH;
		const char* content = getenv("EMUCAP_CONTENT");
		snapshot.content = content != nullptr ? content : "";
		const std::string failure_json = emucap_failure_json(snapshot);
		const char* failure_path = getenv("EMUCAP_FAILURE_FILE");
		if (failure_path != nullptr && failure_path[0] != '\0') {
			std::string error;
			g_failure_file_written = emucap_write_failure_atomic(failure_path, failure_json, &error);
			if (!g_failure_file_written)
				fprintf(stderr, "emucap: cannot persist fatal context: %s\n", error.c_str());
		} else {
			fprintf(stderr, "emucap: EMUCAP_FAILURE_FILE missing; fatal context is live-only\n");
		}
	} catch (const std::exception& error) {
		fprintf(stderr, "emucap: fatal snapshot serialization failed: %s\n", error.what());
	} catch (...) {
		fprintf(stderr, "emucap: fatal snapshot serialization failed\n");
	}

	const uint64_t hold_ms = failure_hold_ms();
	const auto started = std::chrono::steady_clock::now();
	while (g_failure_active && !g_failure_shutdown_requested.load()) {
		if (hold_ms != 0) {
			const uint64_t elapsed = (uint64_t)std::chrono::duration_cast<std::chrono::milliseconds>(
				std::chrono::steady_clock::now() - started).count();
			if (elapsed >= hold_ms) {
				fprintf(stderr, "emucap: fatal quarantine hold expired after %llu ms\n",
					(unsigned long long)elapsed);
				g_failure_active = false;
				break;
			}
		}
		try {
			if (g_fd < 0) emucap_connect();
			if (g_fd >= 0) serve_socket_once();
		} catch (const std::exception& error) {
			fprintf(stderr, "emucap: fatal quarantine socket service failed: %s\n", error.what());
			emucap_disconnect();
		} catch (...) {
			fprintf(stderr, "emucap: fatal quarantine socket service failed\n");
			emucap_disconnect();
		}
		emucap_sock_wait_ms(2);
	}
	// Flycast's normal fatal catch calls dc_exit(), whose unload path can wait forever after this
	// blocked exception. Quarantine completion is a lifecycle decision, not recovery: terminate on
	// dismiss, deadline, or UI shutdown without re-entering cleanup on corrupted guest state.
	if (!g_failure_synthetic)
		std::_Exit(EXIT_FAILURE);
	if (g_failure_synthetic && g_failure_dismissed) {
		std::lock_guard<std::mutex> lock(g_failure_artifact_mtx);
		g_failure_captured.store(false);
	}
}

void emucap_notify_shutdown() noexcept {
	g_failure_shutdown_requested.store(true);
}

// vblank마다(emu 스레드). 예외는 프레임 루프 밖으로 내보내지 않고 현재 요청과 세션을 닫는다.
void emucap_service() {
	try {
		g_frame++;
		g_observed_frame.store(g_frame, std::memory_order_relaxed);
		// 입력 주입은 MapleConfigMap::GetInput(emu 스레드 소비 지점)의 pjs->kcode override에서 이뤄진다
		// (maple_cfg.cpp, build.sh 주입). 여기선 kcode[] 전역을 쓰지 않는다: 게임 입력엔 불필요(GetInput
		// override가 항상 이김)하고 UI 스레드 gamepad 핸들러와 경쟁하므로 주입 상태는 emucap_kcode()에서 합친다.
		if (g_fd < 0) { emucap_connect(); return; }   // 매 프레임 재연결 시도
		// step(frames)/run_frames: 카운트다운 — 이 프레임은 진행시킨다(return → vblank 반환 → 1프레임 진행).
		if (g_step_remaining > 0) {
			if (!g_tx.empty()) flush_tx_once();
			// 긴 진행은 30프레임마다 keepalive를 보내 서버 읽기 타임아웃(5s)을 막는다(인터프리터가 느려
			// 수백 프레임이 5s를 넘는다). 서버는 status:"working"을 건너뛰므로 같은 호출이 유지된다.
			if (g_step_id >= 0 && (g_step_remaining % 30) == 0 && g_tx.empty()) reply_working(g_step_id);
			g_step_remaining--;
			if (g_step_remaining == 0 && g_step_id >= 0) {
				char buf[96];
				snprintf(buf, sizeof(buf), "{\"status\":\"completed\",\"frame\":%llu,\"state\":\"%s\"}",
					(unsigned long long)g_frame, g_frozen ? "frozen" : "running");
				reply_ok(g_step_id, buf);
				g_step_id = -1;
				// step이면 frozen을 유지해 다음 프레임부터 스핀, run_frames이면 running 유지.
			}
			return;
		}

		// freeze: 스핀하며 소켓만 서비스 → vblank가 반환 안 돼 프레임 진행이 막힌다.
		// 연결끊김(/mcp 재연결 등) 시 제자리에서 재접속한다 — 스핀을 나가면 vblank가 반환돼 프레임이
		// 진행해(frozen 장면 유실), emucap_bp_spin과 동일하게 frozen을 유지한 채 재연결한다(장면 보존).
		if (g_frozen) {
			while (g_frozen && g_step_remaining == 0) {
				if (g_fd < 0) { emucap_connect(); if (g_fd < 0) { usleep(2000); continue; } }
				serve_socket_once();
				usleep(2000);
			}
			return;
		}

		serve_socket_once();
		if (g_test_adapter_exception_id >= 0)
			throw std::runtime_error("injected native adapter service exception");
		if (g_synthetic_fatal_pending) {
			g_synthetic_fatal_pending = false;
			emucap_capture_fatal_sh4(
				"Synthetic SH4 fatal (test gate)", Sh4cntx.pc, 0xFFFFFFFFu, 0, 0, 0);
		}
	} catch (const std::exception& error) {
		contain_service_exception("service", error.what());
	} catch (...) {
		contain_service_exception("service", "unknown native adapter exception");
	}
}

// 입력 소비 지점(MapleConfigMap::GetInput, emu 스레드 maple DMA)에서 직접 override하기 위한 헬퍼.
// kcode[] 전역 쓰기는 os_UpdateInputState(UI 스레드)가 매 프레임 리셋해 경합·드롭이 났다 → 게임이
// 실제 읽는 pjs->kcode를 GetInput에서 덮으면 emu 스레드 동기라 경합 zero(결정론적 입력).
bool emucap_input_engaged() { return g_input_override.engaged(); }
uint32_t emucap_kcode() { return g_input_override.kcode(); }  // Active-low: pressed bits are clear.

// 인터프리터 Run() 훅(주입)이 매 명령 전 호출 — pc가 exec BP면 true. armed(전역 bool)가 true일 때만
// 불리므로(핫루프 보호) 여기선 집합 조회만. emu 스레드 단독 접근이라 락 불필요.
bool emucap_exec_bp_check(uint32_t pc) { return g_bp_addrs.find(sh4_fold_pc(pc)) != g_bp_addrs.end(); }

// 히트 순간 CPU 레지스터를 {name:value} JSON으로 — exec BP 히트에 pc뿐 아니라 D0등 문맥을 싣는다(MD와 동형).
// Flycast BP는 exec 전용이라 firehose 걱정이 없어 모든 히트에 캡처한다(예: 렌더진입 collect BP+step으로
// registers.rN=문자코드를 쌓아 화면텍스트와 대조 → 인코딩테이블 추출, MD 어댑터 워크플로와 동일).
std::string emucap_capture_regs() {
	char t[64];
	std::string s = "{";
	for (int i = 0; i < 16; i++) {
		snprintf(t, sizeof(t), "%s\"r%d\":%u", (i ? "," : ""), i, Sh4cntx.r[i]);
		s += t;
	}
	snprintf(t, sizeof(t), ",\"pc\":%u,\"pr\":%u,\"sr\":%u}", Sh4cntx.pc, Sh4cntx.pr,
	         Sh4cntx.sr.getFull());
	s += t;
	return s;
}

// BP 히트 시(명령 실행 직전) 그 자리에서 정지 — 히트 PC를 기록하고 frozen 스핀하며 소켓을 서비스한다.
// resume(g_frozen=false) 또는 step/run_frames(g_step_remaining>0) 시 반환 → 호출부가 BP 명령을 실행하고
// 진행을 잇는다. 명령-정밀(BP 주소에서 정확히 멈춤). emu 스레드라 락 불필요.
void emucap_bp_spin(uint32_t pc) {
	try {
		if (g_bp_hits.size() < 4096) g_bp_hits.push_back({pc, emucap_capture_regs()});  // poll_events 드레인용(미드레인 시 폭주 방지 캡)
		g_frozen = true;
		while (g_frozen && g_step_remaining == 0) {
			if (g_fd < 0) { emucap_connect(); if (g_fd < 0) { usleep(2000); continue; } }
			serve_socket_once();
			usleep(2000);
		}
	} catch (const std::exception& error) {
		contain_service_exception("breakpoint_spin", error.what());
	} catch (...) {
		contain_service_exception("breakpoint_spin", "unknown native adapter exception");
	}
}

// 실행추적/콜스택/레지스터워치 훅(주입) — 인터프리터 Run() 루프가 매 명령 실행 직전 호출한다. armed(전역 bool)가
// true일 때만 불리므로(핫루프 보호) 셋 다 off면 이 함수는 아예 안 불린다(비용 0). emu 스레드 단독 접근이라 락 불필요.
// (a) trace: PC를 원형버퍼에 push. (b) trace: SP(R15) 기반 pruning(현재 SP≥frame.sp면 pop) 후 CALL이면 push.
// (c) watch: register가 [min,max] 밖이면 1회성 해제 후 pause면 emucap_bp_spin으로 그 명령에서 freeze(derail 포착).
void emucap_trace_hook(uint32_t pc) {
	try {
		if (g_trace_enabled) {
			if (!g_trace_ring.empty()) {
				g_trace_ring[g_trace_head] = pc;
				g_trace_head = (g_trace_head + 1) % TRACE_CAP;
				if (g_trace_count < TRACE_CAP) g_trace_count++;
			}
			// SP 기반 반환 감지: 현재 SP(R15)가 어느 프레임의 call-시점 SP 이상으로 오르면 그 프레임(들)은 반환됨 → pop.
			// RTS뿐 아니라 JMP-return·JSR @Rn 간접·수동 스택조작 모든 반환을 잡아 루프 중복누적을 없앤다. SH-4 SP는
			// 항상 Sh4cntx에서 읽히므로(디버거 불필요) opcode 기반 RETURN 폴백이 불필요하다.
			uint32_t sp = Sh4cntx.r[15];
			// SH-4 register-linkage: 콜(BSR/JSR)이 반환주소를 PR에 둬 SP를 안 바꿔 push 직후 sp==frame.sp →
			// 순진한 `sp>=frame.sp`는 프레임을 즉시 pop한다(call_stack=[] 버그). 콜리가 확립(sp가 call-시점
			// 아래로 — prologue mov.l Rn,@-r15)한 뒤에만 "sp 복귀=반환"으로 pop한다.
			bool popped_by_sp = false;  // (B) SP-prune가 이 명령서 pop했나 — 그랬으면 아래 leaf-pop 스킵(이중-pop 방지)
			if (!g_callstack.empty() && sp < g_callstack.back().sp) g_callstack.back().established = true;
			while (!g_callstack.empty() && g_callstack.back().established && sp >= g_callstack.back().sp) {
				g_callstack.pop_back();
				popped_by_sp = true;
			}
			CallKind ck = emucap_classify(pc);
			if (ck == CK_CALL) {
				if (g_callstack.size() < CALLSTACK_CAP) g_callstack.push_back({pc, sp});
			} else if (ck == CK_RETURN && !popped_by_sp && !g_callstack.empty() && !g_callstack.back().established) {
				// 미확립 프레임 = LEAF 함수(스택프레임 없어 sp가 call-시점 아래로 안 내려가 established 안 됨 →
				// sp-pruning이 못 pop, 안 하면 영구 누적). 단 (B) SP-prune가 이 훅서 반환 프레임을 이미 pop했으면
				// (popped_by_sp) 스킵 — established 콜리 반환에서 (B)가 pop한 뒤 (D)가 새 top(미확립 non-leaf)을 잘못
				// pop하는 이중-pop 방지.
				g_callstack.pop_back();
			}
		}
		// 레지스터 워치: register가 [min,max] 밖이면 이 명령에서 정지(derail 포착). 1회성(히트 후 해제 — resume
		// 재freeze 방지; 재무장은 watch_register 재호출). pause면 emucap_bp_spin(BP 히트와 동일 경로 — 히트 PC를
		// g_bp_hits에 싣고 freeze), pause=false면 히트만 기록. 해제 후 rebuild로 armed 재계산(trace만 남으면 유지).
		if (g_watch_enabled) {
			uint32_t rv;
			if (emucap_read_reg(g_watch_reg, rv) && (rv < g_watch_min || rv > g_watch_max)) {
				g_watch_enabled = false;
				rebuild_trace_armed();
				if (g_watch_pause) emucap_bp_spin(pc);
				else if (g_bp_hits.size() < 4096) g_bp_hits.push_back({pc, emucap_capture_regs()});
			}
		}
	} catch (const std::exception& error) {
		g_trace_enabled = false;
		g_watch_enabled = false;
		rebuild_trace_armed();
		(void)publish_native_failure("trace", error.what(), false, "running");
	} catch (...) {
		g_trace_enabled = false;
		g_watch_enabled = false;
		rebuild_trace_armed();
		(void)publish_native_failure(
			"trace", "unknown native adapter exception", false, "running");
	}
}

// mainui_rend_frame(UI/GL 스레드)에서 매 렌더마다 호출 — 최신 프레임 raw를 버퍼에 캡처해 둔다. screenshot은
// 이 버퍼를 emu 스레드에서 PNG 인코딩하므로(GL 불필요) frozen서도 동작한다(버퍼=freeze 직전=frozen 프레임).
// freeze 중엔 UI 렌더가 멈춰 이 함수가 안 불리므로 버퍼는 정확히 frozen 프레임으로 고정된다.
void emucap_capture_latest() {
	// GetLastFrame은 FBO 블릿 + glReadPixels(전 프레임)이라 매 렌더 호출은 비용이 크다 → N프레임마다만
	// 캡처(버퍼는 ~N/60초 이내라 screenshot엔 충분, frozen 직전 프레임도 충분히 최신). running 시에만 도는
	// 함수라(frozen 중엔 UI 렌더가 멈춤) 캡처 정지=비용 0.
	if (g_capture_disabled.load()) return;
	static unsigned tick = 0;
	if ((tick++ & 3) != 0) return;   // 4프레임마다(약 15Hz)
	try {
		std::lock_guard<std::mutex> lk(g_fb_mtx);
		emucap_capture_raw(g_fb_raw, g_fb_w, g_fb_h);
		g_fb_fresh = !g_fb_raw.empty() && g_fb_w > 0 && g_fb_h > 0;
	} catch (const std::exception& error) {
		g_capture_disabled.store(true);
		(void)publish_native_failure("frame_capture", error.what(), false, "running");
	} catch (...) {
		g_capture_disabled.store(true);
		(void)publish_native_failure(
			"frame_capture", "unknown native adapter exception", false, "running");
	}
}
