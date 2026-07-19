// emucap — Dolphin(GameCube/Wii) 네이티브 어댑터 훅.
// GDB-스텁 브리지가 못 주는 savestate/screenshot/입력/frame까지 포함한 풀 제어를
// 위해, Dolphin 내부에 emucap 프로토콜 서버를 임베드한다.
//
// 브리지와 동일한 연결 모델: emucap-mcp 서버가 listening_port에서 어댑터 연결을
// accept 하고, 이 서버는 그 포트로 접속하는 TCP **클라이언트**로서 NDJSON
// {"v":1,"id","method","params"} 요청에 {"id","ok","result|error"}로 답한다.
//
// 기동: 환경변수 EMUCAP_PORT 가 있으면 Core::Init 에서 Start() 가 백그라운드 스레드를
// 띄운다. EMUCAP_SESSION_TOKEN / EMUCAP_NAME / EMUCAP_CONTENT 를 hello 에 실어 보낸다.
#pragma once

#include "Common/CommonTypes.h"

struct GCPadStatus;

namespace Core
{
class System;
}

namespace EmuCap
{
// EMUCAP_PORT 가 설정돼 있으면 어댑터 서버 스레드를 시작한다(한 번만). 없으면 무동작.
void Start(Core::System& system);

// Core 종료 시 스레드를 정리한다.
void Stop();

// GCPad::GetStatus 폴 지점에서 호출. 해당 패드에 set_input 오버라이드가 걸려 있으면
// status 를 덮어쓴다(결정론적 입력 주입). 걸려 있지 않으면 무동작.
void ApplyInputOverride(int pad_num, GCPadStatus* status);

// Called by the PowerPC breakpoint handler after it has confirmed a real hit.
// The adapter filters this against breakpoints registered through emucap.
void NotifyBreakpointHit(u32 address);
}  // namespace EmuCap
