#include "emucap_psx.h"
#include <cassert>
#include <limits>
int main() {
  for (std::uint64_t alias : {0ULL, 0x80000000ULL, 0xa0000000ULL}) {
    assert(emucap_psx_cpu_peek_range(alias, 0x200000));
    assert(emucap_psx_cpu_peek_range(alias + 0x1f800000, 0x400));
    assert(emucap_psx_cpu_peek_range(alias + 0x1fc00000, 0x80000));
    assert(emucap_psx_cpu_peek_range(alias + 0x1f801000, 0x24));
    assert(!emucap_psx_cpu_peek_range(alias + 0x1f801023, 2));
    assert(!emucap_psx_cpu_peek_range(alias + 0x1f801024, 1));
    assert(!emucap_psx_cpu_peek_range(alias + 0x1f801810, 4));
    assert(!emucap_psx_cpu_peek_range(alias + 0x1f802fff, 2));
    assert(emucap_psx_cpu_peek_range(alias + 0x1f803000, 1));
  }
  assert(!emucap_psx_cpu_peek_range(0, 0));
  assert(emucap_psx_cpu_peek_range(0xffffffffULL, 1));
  assert(!emucap_psx_cpu_peek_range(0xffffffffULL, 2));
  assert(!emucap_psx_cpu_peek_range(0, std::numeric_limits<std::uint64_t>::max()));
  for (const auto& window : emucap_psx_cpu_peek_windows())
    assert(emucap_psx_cpu_peek_range(window.address, window.length));
}
