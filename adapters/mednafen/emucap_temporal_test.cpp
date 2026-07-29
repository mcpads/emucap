#include "emucap.h"

#include <cassert>
#include <cstdint>

int main() {
  std::uint64_t last = 1000;

  assert(!emucap_progress_due(last, 1999, 1000));
  assert(last == 1000);
  assert(emucap_progress_due(last, 2000, 1000));
  assert(last == 2000);
  assert(!emucap_progress_due(last, 2999, 1000));
  assert(emucap_progress_due(last, 3500, 1000));
  assert(last == 3500);

  // A monotonic source should not move backwards, but resetting the baseline fails closed if a
  // platform clock implementation ever does.
  assert(!emucap_progress_due(last, 25, 1000));
  assert(last == 25);
  assert(!emucap_progress_due(last, 5000, 0));
  assert(last == 25);

  return 0;
}
