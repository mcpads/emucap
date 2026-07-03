// Mednafen 포크의 라이브 제어 소켓 서비스(우리 IP). Mesen의 emucap-core.lua에 대응하는
// C++판. main.cpp의 프레임 루프에서 매 프레임 호출한다(에뮬레이션 스레드, 락 불필요).
#ifndef EMUCAP_H
#define EMUCAP_H
#include <cstdint>
void emucap_service(uint64_t frame);
// 입력 주입: 주입 입력이 있으면 포트0 버퍼를 덮어쓴다. namespace Mednafen 안(mednafen.cpp)과
// 밖(드라이버) 양쪽에서 호출하므로 extern "C"로 linkage를 고정한다(C++ mangling 불일치 방지).
extern "C" void emucap_apply_input(unsigned char* port0_data, unsigned port0_len);
// Saturn SMPC game-visible input read diagnostics. Called from ss/smpc.cpp
// after OREG/direct-port reads so status can distinguish "latched" from
// "game-visible read".
extern "C" void emucap_smpc_read_store(unsigned addr, unsigned value, const unsigned char* oreg, unsigned len);
// Mega Drive VDP writes are not exposed through Mednafen's CPU memory
// breakpoints, so md/vdp.cpp calls this directly for VRAM/CRAM/VSRAM/register writes.
extern "C" void emucap_md_vdp_write(const char* memory_type, unsigned address, unsigned length, unsigned value,
                                    unsigned pc, const char* source, unsigned source_address);
// MDFNI_Emulate 직후 훅에서 호출: 최신 프레임버퍼를 기록(screenshot용). 타입 결합 회피로 void*.
// 인자는 const MDFN_Surface* / const MDFN_Rect* / const int32*(LineWidths).
void emucap_capture(const void* surface, const void* rect, const void* line_widths);
#endif
