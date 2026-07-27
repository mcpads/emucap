#include "emucap_ngp.h"

#include <cassert>

int main() {
  assert(emucap_ngp_memory("ram") == EmucapNgpMemory::ram);
  assert(emucap_ngp_memory("rom") == EmucapNgpMemory::rom);
  assert(emucap_ngp_memory("bios") == EmucapNgpMemory::bios);
  assert(emucap_ngp_memory("cpu") == EmucapNgpMemory::invalid);
  assert(emucap_ngp_range_valid(0, 1, 1));
  assert(emucap_ngp_range_valid(0x3FFF, 1, 0x4000));
  assert(!emucap_ngp_range_valid(0x3FFF, 2, 0x4000));
  assert(!emucap_ngp_range_valid(0, 0, 0x4000));
  assert(emucap_ngp_memory_writable(EmucapNgpMemory::ram));
  assert(!emucap_ngp_memory_writable(EmucapNgpMemory::rom));
  assert(!emucap_ngp_memory_writable(EmucapNgpMemory::bios));

  assert(emucap_ngp_mask_pc(0xAB123456) == 0x123456);
  assert(emucap_ngp_rfp(0x0300) == 3);

  auto location = emucap_ngp_code_location(0x4000, 0x400000);
  assert(location.view == EmucapNgpCodeView::ram && location.offset == 0);
  location = emucap_ngp_code_location(0x7FFF, 0x400000);
  assert(location.view == EmucapNgpCodeView::ram && location.offset == 0x3FFF);
  location = emucap_ngp_code_location(0x200000, 0x400000);
  assert(location.view == EmucapNgpCodeView::rom && location.offset == 0);
  location = emucap_ngp_code_location(0x3FFFFF, 0x400000);
  assert(location.view == EmucapNgpCodeView::rom
         && location.offset == 0x1FFFFF);
  location = emucap_ngp_code_location(0x800000, 0x400000);
  assert(location.view == EmucapNgpCodeView::rom
         && location.offset == 0x200000);
  location = emucap_ngp_code_location(0x9FFFFF, 0x400000);
  assert(location.view == EmucapNgpCodeView::rom
         && location.offset == 0x3FFFFF);
  assert(emucap_ngp_code_location(0x800000, 0x200000).view
         == EmucapNgpCodeView::invalid);
  location = emucap_ngp_code_location(0xFF0000, 0);
  assert(location.view == EmucapNgpCodeView::bios && location.offset == 0);
  assert(emucap_ngp_code_location(0x8000, 0x400000).view
         == EmucapNgpCodeView::invalid);

  assert(emucap_ngp_safe_code_window(0x4000, 16, 0x400000));
  assert(emucap_ngp_safe_code_window(0x7FF0, 16, 0x400000));
  assert(!emucap_ngp_safe_code_window(0x7FF1, 16, 0x400000));
  assert(emucap_ngp_safe_code_window(0x3FFFF0, 16, 0x400000));
  assert(!emucap_ngp_safe_code_window(0x3FFFF1, 16, 0x400000));
  assert(emucap_ngp_safe_code_window(0x9FFFF0, 16, 0x400000));
  assert(!emucap_ngp_safe_code_window(0x9FFFF1, 16, 0x400000));
  assert(emucap_ngp_safe_code_window(0xFFFFF0, 16, 0));
  assert(!emucap_ngp_safe_code_window(0xFFFFF1, 16, 0));
  assert(!emucap_ngp_safe_code_window(0x4000, 0, 0));

  std::uint32_t pc = 1;
  std::uint32_t mem = 2;
  int size = 3;
  std::uint8_t first = 4;
  std::uint8_t second = 5;
  std::uint8_t upper_register = 6;
  std::uint8_t register_code = 7;
  bool register_code_used = false;
  {
    EmucapNgpDecodeRestore restore(
        pc, mem, size, first, second, upper_register, register_code,
        register_code_used);
    pc = 10;
    mem = 11;
    size = 12;
    first = 13;
    second = 14;
    upper_register = 15;
    register_code = 16;
    register_code_used = true;
  }
  assert(pc == 1 && mem == 2 && size == 3);
  assert(first == 4 && second == 5 && upper_register == 6);
  assert(register_code == 7 && !register_code_used);

  const EmucapNgpExecRange whole{0, 0xFFFFFF};
  const EmucapNgpExecRange one{0x123456, 0x123456};
  assert(emucap_ngp_exec_range_valid(whole));
  assert(emucap_ngp_exec_matches(one, 0xAB123456));
  assert(!emucap_ngp_exec_matches(one, 0x123457));
  assert(!emucap_ngp_exec_range_valid({2, 1}));
  return 0;
}
