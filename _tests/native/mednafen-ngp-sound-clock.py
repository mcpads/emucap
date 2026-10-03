#!/usr/bin/env python3
"""Restore the actual NGP APU across different destination time origins."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
parser.add_argument('--apu-source', type=Path)
parser.add_argument('--baseline', action='store_true')
args = parser.parse_args()
source = args.source.resolve()
apu_source = (args.apu_source or source / 'src/ngp').resolve()
code = r'''
#include <mednafen/mednafen.h>
#include "T6W28_Apu.h"
#include <cassert>
#include <algorithm>
using namespace MDFN_IEN_NGP;
struct Device {
 Blip_Buffer buffer;
 T6W28_Apu apu;
 Device() {
  assert(!buffer.set_sample_rate(44100,60));buffer.clock_rate(3072000);
  apu.output(&buffer);apu.volume(.3);
  apu.write_data_left(0,0x81);apu.write_data_left(0,0x20);
  apu.write_data_left(0,0x90);apu.write_data_right(0,0x90);
 }
};
void same(const T6W28_ApuState& a,const T6W28_ApuState& b) {
 assert(std::equal(a.delay,a.delay+4,b.delay));
 assert(std::equal(a.sq_phase,a.sq_phase+3,b.sq_phase));
 assert(std::equal(a.sq_period,a.sq_period+3,b.sq_period));
 assert(std::equal(a.volume_left,a.volume_left+4,b.volume_left));
 assert(std::equal(a.volume_right,a.volume_right+4,b.volume_right));
 assert(a.noise_shifter==b.noise_shifter && a.noise_period==b.noise_period);
 assert(a.noise_tap==b.noise_tap && a.noise_period_extra==b.noise_period_extra);
 assert(a.latch_left==b.latch_left && a.latch_right==b.latch_right);
}
int main() {
 for(int destination_time : {0,300,700}) {
  Device source,destination;
  source.apu.write_data_left(100,0x95);
  T6W28_ApuState saved{};source.apu.save_state(&saved);
  destination.apu.write_data_left(destination_time,0x93);
  destination.apu.load_state(&saved);
  for(int time : {120,160,400,800}) {
   source.apu.write_data_left(time,0x94);
   destination.apu.write_data_left(time,0x94);
   T6W28_ApuState a{},b{};source.apu.save_state(&a);destination.apu.save_state(&b);same(a,b);
  }
  source.apu.end_frame(1000);destination.apu.end_frame(1000);
  T6W28_ApuState a{},b{};source.apu.save_state(&a);destination.apu.save_state(&b);same(a,b);
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-ngp-sound-clock-') as temp:
    cpp = Path(temp) / 'test.cpp'
    binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-DHAVE_CONFIG_H', '-O1',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I' + str(source / 'include'), '-I' + str(source / 'intl'), '-I' + str(apu_source), '-I' + str(source / 'src/ngp'),
        str(cpp), str(apu_source / 'T6W28_Apu.cpp'),
        str(source / 'src/sound/Blip_Buffer.cpp'), '-o', str(binary)], check=True)
    result = subprocess.run([str(binary)], capture_output=True, text=True,
        env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1'))
    if args.baseline:
        assert result.returncode != 0 and 'Assertion' in result.stderr, result
        print(result.stderr.strip())
        print('Native NGP APU destination-clock counterexample reproduced')
    else:
        assert result.returncode == 0, result.stderr
        print('Native NGP APU: three destination clock origins and ordered oscillator continuation pass ASan/UBSan')
