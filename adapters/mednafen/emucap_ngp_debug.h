#pragma once

#ifdef WANT_DEBUGGER

#include "emucap_ngp.h"

namespace MDFN_IEN_NGP {

void NGPDBG_Init(void);
void NGPDBG_Kill(void);
void NGPDBG_InstructionBoundary(uint32 pc);

uint8 NGPDBG_Peek8(uint32 address);
uint16 NGPDBG_Peek16(uint32 address);
uint32 NGPDBG_Peek32(uint32 address);

extern DebuggerInfoStruct NGPDBGInfo;

}

extern "C" int emucap_ngp_disasm_safe(unsigned address, unsigned length);

#endif
