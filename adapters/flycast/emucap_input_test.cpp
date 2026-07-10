#include "emucap_input.h"

#include <cassert>
#include <cstdint>

int main() {
  EmucapFlycastInputOverride input;
  constexpr std::uint32_t native_kcode = 0xfffffff7u;

  assert(!input.engaged());
  assert(input.pressed_mask() == 0);

  input.set(0x00000018u);
  assert(input.engaged());
  assert(input.pressed_mask() == 0x00000018u);
  assert(input.kcode() == 0xffffffe7u);

  // set_input([]): ownership returns to the native keyboard/controller path. The consumer's
  // conditional override therefore leaves its already sampled native kcode untouched.
  input.set(0);
  assert(!input.engaged());
  std::uint32_t consumed = native_kcode;
  if (input.engaged()) consumed = input.kcode();
  assert(consumed == native_kcode);

  input.set(0x80000000u);
  assert(input.engaged());
  assert(input.pressed_mask() == 0x80000000u);
  input.release();
  assert(!input.engaged());
  return 0;
}
