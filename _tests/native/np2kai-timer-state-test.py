#!/usr/bin/env python3
"""The restored timer device must reproduce the next interrupt and clock tick."""

import argparse
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_root", type=Path)
    args = parser.parse_args()
    header = (args.source_root / "io/upd4990.h").read_text()
    end = header.index("} _UPD4990HRT")
    start = header.rindex("typedef struct {", 0, end)
    structure = header[start:header.index(";", end) + 1]
    source = (args.source_root / "io/upd4990.c").read_text()
    start = source.index("void upd4990_hrtimer_proc(")
    end = source.index("static void upd4990_hrtimer_setinterval(int absolute) {", start)
    callback = source[start:end]
    support = r'''
#include <assert.h>
#include <stdint.h>
typedef uint32_t UINT32;
typedef void *NEVENTITEM;
static unsigned interrupts;
static uint8_t mem[0x1000];
static void pic_setirq(int irq) { assert(irq == 15); ++interrupts; }
static void upd4990_hrtimer_setinterval(int absolute) {}
static uint32_t read32(const uint8_t *p) { return p[0] | p[1]<<8 | p[2]<<16 | p[3]<<24; }
static void write32(uint8_t *p, uint32_t x) { for (int n=0;n<4;++n) p[n] = x >> (8*n); }
#define LOADINTELDWORD(p) read32(p)
#define STOREINTELDWORD(p, x) write32(p, x)
'''
    checks = r'''
int main(void) {
  uPD4990HRT.hrtimerdiv = 32;
  uPD4990HRT.hrtimerclock32 = 1;
  upd4990_hrtimer_proc(0);
  _UPD4990HRT saved = uPD4990HRT;
  interrupts = 0; write32(mem+0x04F1, 0);
  upd4990_hrtimer_proc(0);
  unsigned expected_interrupts = interrupts, expected_clock = read32(mem+0x04F1);
  uPD4990HRT = saved;
  interrupts = 0; write32(mem+0x04F1, 0);
  upd4990_hrtimer_proc(0);
  assert(interrupts == expected_interrupts);
  assert(read32(mem+0x04F1) == expected_clock);
}
'''
    with tempfile.TemporaryDirectory(prefix="np2kai-timer-state-") as directory:
        path = Path(directory)
        (path / "test.c").write_text(support + structure + "\nstatic _UPD4990HRT uPD4990HRT;\n" + callback + checks)
        subprocess.run(["cc", "-std=c99", str(path / "test.c"), "-o", str(path / "test")], check=True)
        subprocess.run([str(path / "test")], check=True)
    print("NP2kai restored timer phase passed")


if __name__ == "__main__":
    main()
