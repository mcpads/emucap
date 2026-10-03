#!/usr/bin/env python3
"""Native dynamic-view admission must reject every invalid batch before payload reads."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('source_root', type=Path)
a = p.parse_args()
s = (a.source_root / 'sdl/libretro/emucap_debug.c').read_text()
start = s.index('static int emucap_peek_offset(')
end = s.index('int emucap_np2_write_memory(', start)
code = r'''
#include <cassert>
#include <cstdint>
#include <cstddef>
using UINT = unsigned;
#define CPU_MEMREADMAX 0xA4000U
#define EMUCAP_MEMORY_LIMIT 0x100000U
#define VRAM_STEP 0x100000U
#define VOPBIT_ACCESS 0
#define VOPBIT_ANALOG 1
#define VOPBIT_VGA 2
struct { unsigned operate; } vramop;
static unsigned reads;
struct Memory {
 uint8_t operator[](size_t address) const { ++reads; return (address ^ (address >> 16)) & 255; }
} mem;
struct emucap_np2_peek_range { uint32_t address, length; };
''' + s[start:end] + r'''
static void batch(unsigned mode, uint32_t last, bool expected) {
 vramop.operate = mode; reads=0;
 emucap_np2_peek_range ranges[] = {{0x123, 2}, {last, 1}};
 int accepted = emucap_np2_validate_peek_ranges(ranges,2);
 assert(bool(accepted)==expected && reads==0);
 if (accepted) {
  uint8_t out[2];
  for (auto r : ranges) {
   assert(emucap_np2_peek_memory(r.address,out,r.length));
   for (unsigned j=0;j<r.length;j++) {
    auto address=r.address+j;
    if (address>=0xA8000 && (mode&(1U<<VOPBIT_ACCESS))) address+=VRAM_STEP;
    assert(out[j]==((address^(address>>16))&255));
   }
  }
  assert(reads==3);
 }
}
int main() {
 batch(0,0xE0000,false); // valid RAM followed by inaccessible digital I plane
 batch(1U<<VOPBIT_ANALOG,0xE0000,true);
 batch((1U<<VOPBIT_ANALOG)|(1U<<VOPBIT_ACCESS),0xE7FFF,true);
 batch(1U<<VOPBIT_VGA,0xA8000,false);
 batch((1U<<VOPBIT_VGA)|(1U<<VOPBIT_ANALOG),0xE0000,false);
 batch(0,0xA8000,true);
 for (auto bad : {emucap_np2_peek_range{0,0}, {UINT32_MAX,2}, {0,0x4001}, {0xA3FFF,2}}) {
  emucap_np2_peek_range ranges[]={{0,1},bad}; reads=0;
  assert(!emucap_np2_validate_peek_ranges(ranges,2) && reads==0);
 }
 emucap_np2_peek_range ranges[65];
 for(auto& r:ranges) r={0,1024};
 reads=0; assert(emucap_np2_validate_peek_ranges(ranges,64) && reads==0);
 assert(!emucap_np2_validate_peek_ranges(ranges,65));
 ranges[63].length=1025; assert(!emucap_np2_validate_peek_ranges(ranges,64));
 assert(!emucap_np2_validate_peek_ranges(ranges,0));
 assert(!emucap_np2_validate_peek_ranges(nullptr,1));
 assert(reads==0);
}
'''
code = '#include <initializer_list>\n' + code
with tempfile.TemporaryDirectory(prefix='np2-peek-') as d:
    src=Path(d)/'test.cpp';src.write_text(code);exe=Path(d)/'test'
    subprocess.run(['c++','-std=c++17','-Wall','-Wextra','-Werror','-fsanitize=address,undefined',str(src),'-o',str(exe)],check=True)
    subprocess.run([str(exe)],check=True)
print('PASS NP2kai native digital/analog/VGA batch admission, zero payload reads, limits and raw access')
