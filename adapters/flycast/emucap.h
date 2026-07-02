// emucap — Flycast(Dreamcast) 어댑터 훅. build.sh가 이 헤더/소스를 Flycast 트리(core/)에 넣고
// core/emulator.cpp의 vblank()에 emucap_service() 호출을 주입한다. emu 스레드에서 돌아 락 불필요.
#pragma once
#include <cstdint>   // uint32_t

// vblank마다(에뮬 1프레임 경계, emu 스레드) 호출. 소켓을 폴해 emucap-mcp 요청을 처리하고,
// frozen이면 스핀해 프레임 진행을 막는다(freeze). 예외는 내부에서 전부 삼킨다(프레임 루프 보호).
void emucap_service();

// MapleConfigMap::GetInput(emu 스레드 maple DMA — 게임이 입력을 읽는 소비 지점)에서 pjs->kcode를
// 덮어쓰기 위한 헬퍼. engaged면 emucap_kcode()로 override → UI 스레드 os_UpdateInputState 리셋과
// 경합 없이 결정론적 입력. (kcode[] 전역 직접 쓰기는 경합/드롭 발생.)
bool emucap_input_engaged();
uint32_t emucap_kcode();

// mainui_rend_frame(UI/GL 스레드)에서 매 렌더마다 호출. 최신 프레임 raw를 버퍼에 떠 둬서 screenshot이
// emu 스레드에서 즉시 PNG로 인코딩(GL 불필요)할 수 있게 한다 → freeze 중에도 screenshot 동작.
void emucap_capture_latest();

// exec breakpoint 훅 — sh4_interpreter Run() 루프(주입)가 매 명령 전 사용한다. g_emucap_bp_armed가
// true일 때만 emucap_exec_bp_check(pc)를 보고(핫루프 비용 0), 히트면 emucap_bp_spin(pc)로 명령-정밀 정지.
extern bool g_emucap_bp_armed;
bool emucap_exec_bp_check(uint32_t pc);
void emucap_bp_spin(uint32_t pc);

// 크래시경로 관측 훅(set_trace/get_trace/watch_register/call_stack) — 같은 Run() 루프(주입)가 매 명령 전 사용한다.
// g_emucap_trace_armed(trace 또는 watch 활성 시 true)일 때만 emucap_trace_hook(pc)를 호출해(핫루프 비용 0)
// PC를 실행추적 링·shadow 콜스택에 기록하고, 워치 레지스터가 범위를 벗어나면 그 명령에서 freeze한다.
extern bool g_emucap_trace_armed;
void emucap_trace_hook(uint32_t pc);
