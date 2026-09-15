#!/usr/bin/env python3
"""Compile the pinned native serialization entrypoints against fault-injected I/O.

Pass the extracted libretro.c before or after applying the adapter patch stack.
The native entrypoints must reject incomplete I/O and failed deserialization.
"""

import argparse
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    source = args.source.read_text()
    source = source[source.index('#define RETRO_NP2_TEMPSTATE'):]
    harness = r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
typedef unsigned UINT;
typedef long FILELEN;
typedef int FILEH;
#define FILEH_INVALID 0
static int mode, loads, closes, deletes;
static char *file_getcd(const char *p) { return (char *)p; }
static int statsave_save_d(const char *p) { return mode == 1; }
static FILEH file_open_rb(const char *p) { return mode == 2 ? 0 : 1; }
static FILEH file_create(const char *p) { return mode == 2 ? 0 : 1; }
static FILELEN file_getsize(FILEH f) { assert(f); return 4; }
static UINT file_read(FILEH f, void *p, UINT n) { assert(f); return n - (mode == 3); }
static UINT file_write(FILEH f, const void *p, UINT n) { assert(f); return n - (mode == 3); }
static short file_close(FILEH f) { assert(f); ++closes; return mode == 4 ? -1 : 0; }
static short file_delete(const char *p) { ++deletes; return 0; }
static int statsave_load_d(const char *p) { ++loads; return mode == 5 ? 1 : 0; }
'''
    checks = r'''
int main(void) {
  char data[4] = {0};
  assert(retro_serialize_size() == 4);
  assert(retro_serialize(data, sizeof(data)));
  assert(retro_unserialize(data, sizeof(data)));
  assert(!retro_unserialize(data, 0));
  for (mode = 2; mode <= 5; ++mode) {
    loads = closes = deletes = 0;
    assert(!retro_unserialize(data, sizeof(data)));
    assert(loads == (mode == 5));
    assert(closes == (mode != 2));
    assert(deletes == (mode != 2));
  }
  for (mode = 1; mode <= 4; ++mode)
    assert(!retro_serialize(data, sizeof(data)));
  for (mode = 1; mode <= 2; ++mode)
    assert(retro_serialize_size() == 0);
  mode = 4;
  assert(retro_serialize_size() == 0);
  return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix="np2kai-state-io-") as directory:
        path = Path(directory)
        (path / "test.c").write_text(harness + source + checks)
        subprocess.run(["cc", "-std=c99", str(path / "test.c"), "-o", str(path / "test")], check=True)
        subprocess.run([str(path / "test")], check=True)
    print("NP2kai state I/O fault cases passed")


if __name__ == "__main__":
    main()
