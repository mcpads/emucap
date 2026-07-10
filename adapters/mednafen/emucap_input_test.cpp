#include "emucap_input.h"

#include <cassert>

int main() {
  EmucapInputOverride input;
  unsigned char port[2] = {0x34, 0x12};

  input.apply(port, 2);
  assert(port[0] == 0x34 && port[1] == 0x12);

  input.engage(0x0008);
  assert(input.engaged());
  assert(input.mask() == 0x0008);
  input.apply(port, 2);
  assert(port[0] == 0x08 && port[1] == 0x00);

  input.release();
  assert(!input.engaged());
  assert(input.mask() == 0);
  port[0] = 0xA5;
  port[1] = 0x5A;
  input.apply(port, 2);
  assert(port[0] == 0xA5 && port[1] == 0x5A);

  input.engage(0x1234);
  unsigned char zero_length = 0x7E;
  input.apply(&zero_length, 0);
  assert(zero_length == 0x7E);

  unsigned char short_port[1] = {0};
  input.apply(short_port, 1);
  assert(short_port[0] == 0x34);
  return 0;
}
