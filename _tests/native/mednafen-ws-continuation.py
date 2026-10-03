#!/usr/bin/env python3
"""Run the native WS scanline dispatcher against controlled CPU/device seams."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
native = (args.source / 'src/wswan/gfx.cpp').read_text()
body = native[native.index('bool wsExecuteLine('):native.index('void WSwan_SetLayerEnableMask')]
code = r'''
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <cstring>
#include <vector>
using uint8 = uint8_t;
struct MDFN_Surface {};
static uint8 weppy, wsLine, LCDVtotal=158, LineCompare;
static bool line_frame_complete, FrameWhichActive;
static uint8 SpriteCountCache[2], SpriteCount, SpriteTable[2][128][4];
static uint8 wsRAM[65536], SPRBase, SpriteStart;
static unsigned VBCounter, BTimerControl, VBTimerPeriod, HBCounter, HBTimerPeriod;
static unsigned rendered, communications, dma, rtc;
static bool replace_in_cpu;
static std::vector<int> budgets;
static constexpr int WSINT_VBLANK=1, WSINT_VBLANK_TIMER=2, WSINT_HBLANK_TIMER=3, WSINT_LINE_HIT=4;
void wsScanline(MDFN_Surface*) { ++rendered; }
void Comm_Process() { ++communications; }
void WSwan_CheckSoundDMA() { ++dma; }
void WSwan_Interrupt(int) {}
void RTC_Clock(int cycles) { assert(cycles==256); ++rtc; }
void v30mz_execute(int cycles) {
 budgets.push_back(cycles);
 if(replace_in_cpu) {
  replace_in_cpu=false; weppy=3; wsLine=145; line_frame_complete=true;
 }
}
''' + body + r'''
void clear() {
 budgets.clear(); rendered=communications=dma=rtc=0;
 replace_in_cpu=false; FrameWhichActive=false;
}
int main() {
 for(unsigned phase=1;phase<=3;++phase) for(bool completed:{false,true}) {
  clear(); weppy=phase; wsLine=phase==3 ? 145 : 144;
  line_frame_complete=completed;
  assert(wsExecuteLine(nullptr,true)==completed);
  const std::vector<int> expected=phase==1 ? std::vector<int>{0,96,32}
      : phase==2 ? std::vector<int>{0,32} : std::vector<int>{0};
  assert(budgets==expected && weppy==0 && rtc==1);
  assert(rendered==0 && communications==0 && dma==(phase==1 ? 1u : 0u));
 }
 for(unsigned line:{0u,142u,144u}) {
  clear(); weppy=0; wsLine=line; line_frame_complete=true;
  assert(wsExecuteLine(nullptr,true)==(line==144));
  assert((budgets==std::vector<int>{128,96,32}));
  assert(weppy==0 && rtc==1 && communications==1 && dma==2);
 }
 // A load inside the first CPU slice must use restored completion and phase,
 // rather than the old function's local return value or remaining slices.
 clear(); weppy=0; wsLine=0; line_frame_complete=false; replace_in_cpu=true;
 assert(wsExecuteLine(nullptr,true));
 assert((budgets==std::vector<int>{128}));
 assert(weppy==0 && rtc==1);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-ws-continuation-') as temp:
    cpp = Path(temp) / 'test.cpp'; binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Native WS scanline: six restored phases/completions, three ordinary lines, '
      'and in-slice replacement pass ASan/UBSan')
