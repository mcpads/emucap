#!/usr/bin/env python3
"""Exercise native clock-profile admission for fixed, adaptive and disabled clocks."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
source = (ROOT / 'adapters/xemu/work/xemu/accel/tcg/icount-common.c').read_text()
start = source.index('bool xemu_emucap_clock_supported(void)')
end = source.index('\n}', start) + 2
with tempfile.TemporaryDirectory(prefix='xemu-clock-profile-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text('''#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
enum { ICOUNT_DISABLED, ICOUNT_PRECISE, ICOUNT_ADAPTIVE };
enum { QEMU_CLOCK_HOST, QEMU_CLOCK_REALTIME, QEMU_CLOCK_VIRTUAL };
static int rtc_clock;
static int mode, shift;
static bool icount_sleep, icount_align_option;
static int icount_enabled(void) { return mode; }
static int64_t icount_to_ns(int64_t count) { return count << shift; }
''' + source[start:end] + '''
int main(void) {
    for (rtc_clock = 0; rtc_clock < 3; rtc_clock++)
    for (mode = 0; mode < 3; mode++)
      for (shift = 0; shift <= 10; shift++)
        for (int sleep = 0; sleep < 2; sleep++)
          for (int align = 0; align < 2; align++) {
            icount_sleep = sleep;
            icount_align_option = align;
            assert(xemu_emucap_clock_supported() ==
                   (rtc_clock == QEMU_CLOCK_VIRTUAL && mode == ICOUNT_PRECISE && shift <= 3 && !sleep && !align));
          }
    return 0;
}
''')
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', str(temp / 'test.c'),
                    '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('396 native clock-profile combinations passed')
