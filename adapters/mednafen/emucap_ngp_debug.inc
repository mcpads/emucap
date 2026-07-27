#ifdef WANT_DEBUGGER

#include <algorithm>

namespace {

std::vector<EmucapNgpExecRange> ngp_exec_breakpoints;
void (*ngp_cpu_hook)(uint32, bool) = nullptr;
bool ngp_cpu_hook_continuous = false;

enum NgpRegisterId {
  NGP_REG_PC,
  NGP_REG_SR,
  NGP_REG_F_DASH,
  NGP_REG_RFP,
  NGP_REG_XWA,
  NGP_REG_XBC,
  NGP_REG_XDE,
  NGP_REG_XHL,
  NGP_REG_XIX,
  NGP_REG_XIY,
  NGP_REG_XIZ,
  NGP_REG_XSP,
};

std::uint64_t ngp_memory_size(EmucapNgpMemory memory) {
  switch (memory) {
    case EmucapNgpMemory::ram:
      return sizeof(CPUExRAM);
    case EmucapNgpMemory::rom:
      return ngpc_rom.length;
    case EmucapNgpMemory::bios:
      return 0x10000;
    case EmucapNgpMemory::invalid:
      return 0;
  }
  return 0;
}

const uint8* ngp_memory_read_pointer(EmucapNgpMemory memory) {
  switch (memory) {
    case EmucapNgpMemory::ram:
      return CPUExRAM;
    case EmucapNgpMemory::rom:
      return ngpc_rom.data;
    case EmucapNgpMemory::bios:
      return ngpc_bios;
    case EmucapNgpMemory::invalid:
      return nullptr;
  }
  return nullptr;
}

uint8* ngp_memory_write_pointer(EmucapNgpMemory memory) {
  return memory == EmucapNgpMemory::ram ? CPUExRAM : nullptr;
}

void NGPDBG_GetAddressSpaceBytes(
    const char* name,
    uint32 address,
    uint32 length,
    uint8* buffer) {
  if (!buffer || !name) return;
  const EmucapNgpMemory memory = emucap_ngp_memory(name);
  const uint8* source = ngp_memory_read_pointer(memory);
  if (!source
      || !emucap_ngp_range_valid(address, length, ngp_memory_size(memory))) {
    if (length) memset(buffer, 0, length);
    return;
  }
  memcpy(buffer, source + address, length);
}

void NGPDBG_PutAddressSpaceBytes(
    const char* name,
    uint32 address,
    uint32 length,
    uint32,
    bool,
    const uint8* buffer) {
  if (!buffer || !name) return;
  const EmucapNgpMemory memory = emucap_ngp_memory(name);
  uint8* destination = ngp_memory_write_pointer(memory);
  if (!destination
      || !emucap_ngp_range_valid(address, length, ngp_memory_size(memory)))
    return;
  memcpy(destination + address, buffer, length);
}

uint32 NGPDBG_MemPeek(uint32 address, unsigned int byte_size, bool, bool) {
  if (byte_size == 0 || byte_size > 4
      || !emucap_ngp_safe_code_window(address, byte_size, ngpc_rom.length))
    return 0;
  uint32 value = 0;
  for (unsigned int index = 0; index < byte_size; index++)
    value |= static_cast<uint32>(NGPDBG_Peek8(address + index)) << (index * 8);
  return value;
}

void NGPDBG_Disassemble(uint32& address, uint32, char* text) {
  if (!text) return;
  const uint32 start = emucap_ngp_mask_pc(address);
  if (!emucap_ngp_safe_code_window(start, 16, ngpc_rom.length)) {
    strcpy(text, "unsafe NGP code address");
    address = start;
    return;
  }

  EmucapNgpDecodeRestore restore(
      pc, mem, size, first, second, R, rCode, brCode);
  pc = start;
  char* decoded = TLCS900H::TLCS900h_disassemble();
  const uint32 next = emucap_ngp_mask_pc(pc);
  if (!decoded || next <= start) {
    if (decoded) free(decoded);
    strcpy(text, "invalid NGP instruction");
    address = start;
    return;
  }
  snprintf(text, 256, "%s", decoded);
  free(decoded);
  address = next;
}

void NGPDBG_FlushBreakPoints(int type) {
  if (type == BPOINT_PC) ngp_exec_breakpoints.clear();
}

void NGPDBG_AddBreakPoint(
    int type,
    unsigned int start,
    unsigned int end,
    bool) {
  const EmucapNgpExecRange range{
      emucap_ngp_mask_pc(start), emucap_ngp_mask_pc(end)};
  if (type == BPOINT_PC && emucap_ngp_exec_range_valid(range))
    ngp_exec_breakpoints.push_back(range);
}

void NGPDBG_SetCPUCallback(
    void (*callback)(uint32 pc, bool breakpoint),
    bool continuous) {
  ngp_cpu_hook = callback;
  ngp_cpu_hook_continuous = continuous;
}

uint32 NGPDBG_GetRegister(
    const unsigned int id,
    char*,
    const uint32) {
  switch (id) {
    case NGP_REG_PC:
      return emucap_ngp_mask_pc(pc);
    case NGP_REG_SR:
      return sr;
    case NGP_REG_F_DASH:
      return f_dash;
    case NGP_REG_RFP:
      return emucap_ngp_rfp(sr);
    case NGP_REG_XWA:
    case NGP_REG_XBC:
    case NGP_REG_XDE:
    case NGP_REG_XHL:
      return gprBank[emucap_ngp_rfp(sr)][id - NGP_REG_XWA];
    case NGP_REG_XIX:
    case NGP_REG_XIY:
    case NGP_REG_XIZ:
    case NGP_REG_XSP:
      return gpr[id - NGP_REG_XIX];
  }
  return 0;
}

void NGPDBG_SetRegister(const unsigned int id, uint32 value) {
  switch (id) {
    case NGP_REG_PC:
      pc = emucap_ngp_mask_pc(value);
      break;
    case NGP_REG_SR:
      sr = value;
      changedSP();
      break;
    case NGP_REG_F_DASH:
      f_dash = value;
      break;
    case NGP_REG_RFP:
      sr = (sr & ~0x300) | ((value & 3) << 8);
      changedSP();
      break;
    case NGP_REG_XWA:
    case NGP_REG_XBC:
    case NGP_REG_XDE:
    case NGP_REG_XHL:
      gprBank[emucap_ngp_rfp(sr)][id - NGP_REG_XWA] = value;
      break;
    case NGP_REG_XIX:
    case NGP_REG_XIY:
    case NGP_REG_XIZ:
    case NGP_REG_XSP:
      gpr[id - NGP_REG_XIX] = value;
      break;
  }
}

const RegType NGPDBG_Registers[] = {
    {NGP_REG_PC, "PC", "Program Counter", 4},
    {NGP_REG_SR, "SR", "Status Register", 2},
    {NGP_REG_F_DASH, "F'", "Alternate Flags", 1},
    {NGP_REG_RFP, "RFP", "Register File Pointer", 1},
    {NGP_REG_XWA, "XWA", "Current Bank XWA", 4},
    {NGP_REG_XBC, "XBC", "Current Bank XBC", 4},
    {NGP_REG_XDE, "XDE", "Current Bank XDE", 4},
    {NGP_REG_XHL, "XHL", "Current Bank XHL", 4},
    {NGP_REG_XIX, "XIX", "Index Register X", 4},
    {NGP_REG_XIY, "XIY", "Index Register Y", 4},
    {NGP_REG_XIZ, "XIZ", "Index Register Z", 4},
    {NGP_REG_XSP, "XSP", "Stack Pointer", 4},
    {0, "", "", 0},
};

const RegGroupType NGPDBG_RegisterGroup = {
    "TLCS900H",
    NGPDBG_Registers,
    NGPDBG_GetRegister,
    NGPDBG_SetRegister,
};

}

uint8 NGPDBG_Peek8(uint32 address) {
  const EmucapNgpCodeLocation location =
      emucap_ngp_code_location(address, ngpc_rom.length);
  switch (location.view) {
    case EmucapNgpCodeView::ram:
      return CPUExRAM[location.offset];
    case EmucapNgpCodeView::rom:
      return ngpc_rom.data[location.offset];
    case EmucapNgpCodeView::bios:
      return ngpc_bios[location.offset];
    case EmucapNgpCodeView::invalid:
      return 0;
  }
  return 0;
}

uint16 NGPDBG_Peek16(uint32 address) {
  return static_cast<uint16>(NGPDBG_Peek8(address))
      | static_cast<uint16>(NGPDBG_Peek8(address + 1)) << 8;
}

uint32 NGPDBG_Peek32(uint32 address) {
  return static_cast<uint32>(NGPDBG_Peek16(address))
      | static_cast<uint32>(NGPDBG_Peek16(address + 2)) << 16;
}

void NGPDBG_InstructionBoundary(uint32 boundary_pc) {
  if (!ngp_cpu_hook) return;
  const uint32 masked_pc = emucap_ngp_mask_pc(boundary_pc);
  const bool matched = std::any_of(
      ngp_exec_breakpoints.begin(),
      ngp_exec_breakpoints.end(),
      [masked_pc](const EmucapNgpExecRange& range) {
        return emucap_ngp_exec_matches(range, masked_pc);
      });
  if (matched || ngp_cpu_hook_continuous)
    ngp_cpu_hook(masked_pc, matched);
}

void NGPDBG_Init(void) {
  ngp_exec_breakpoints.clear();
  ngp_cpu_hook = nullptr;
  ngp_cpu_hook_continuous = false;
  MDFNDBG_AddRegGroup(&NGPDBG_RegisterGroup);
  ASpace_Add(
      NGPDBG_GetAddressSpaceBytes, NGPDBG_PutAddressSpaceBytes,
      "ram", "CPU External RAM", 14);
  ASpace_Add(
      NGPDBG_GetAddressSpaceBytes, NGPDBG_PutAddressSpaceBytes,
      "rom", "Cartridge ROM", 0, ngpc_rom.length);
  ASpace_Add(
      NGPDBG_GetAddressSpaceBytes, NGPDBG_PutAddressSpaceBytes,
      "bios", "HLE BIOS", 16);
}

void NGPDBG_Kill(void) {
  ngp_cpu_hook = nullptr;
  ngp_cpu_hook_continuous = false;
  ngp_exec_breakpoints.clear();
}

DebuggerInfoStruct NGPDBGInfo = {
    false,
    "shift_jis",
    16,
    1,
    24,
    24,
    0x4000,
    ~0U,
    NGPDBG_MemPeek,
    NGPDBG_Disassemble,
    nullptr,
    nullptr,
    nullptr,
    NGPDBG_FlushBreakPoints,
    NGPDBG_AddBreakPoint,
    NGPDBG_SetCPUCallback,
    nullptr,
    nullptr,
    nullptr,
    nullptr,
};

extern "C" int emucap_ngp_disasm_safe(
    unsigned address,
    unsigned length) {
  return emucap_ngp_safe_code_window(
      address, length, ngpc_rom.length) ? 1 : 0;
}

#endif
