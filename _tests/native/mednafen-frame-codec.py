#!/usr/bin/env python3
"""Exercise the frame codec with configured native Mednafen surface/allocation code."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
source = args.source.resolve()
adapter = Path(__file__).resolve().parents[2] / 'adapters/mednafen'
code = r'''
#include <mednafen/mednafen.h>
#include "emucap_completed_frame.h"
#include <algorithm>
#include <cassert>

using namespace Mednafen;
void check(uint64 tag) {
 for(int field : {-1,0,1}) {
  const MDFN_PixelFormat format(tag);
  MDFN_Surface input(nullptr, 4, 2, 6, format);
  for (unsigned y = 0; y < 2; ++y)
    for (unsigned x = 0; x < 4; ++x) {
      const auto pixel = format.MakeColor(x * 60, y * 100, (x + y) * 40, 255);
      if (format.opp == 2) input.pixels16[y * 6 + x] = uint16(pixel);
      else input.pixels[y * 6 + x] = pixel;
    }
  const int32 widths[] = {2, 3};
  EmucapCompletedFrame frame;
  frame.capture(input, {1, 0, 0, 2}, widths, field);
  auto restored = EmucapCompletedFrame::decode(frame.encode());
  assert(restored.encode() == frame.encode());
  assert(restored.field()==field);
  assert(restored.rect().x == 1 && restored.rect().w == 0);
  assert(restored.line_widths()[0] == 2 && restored.line_widths()[1] == 3);
  assert(restored.surface()->format.tag == tag && restored.surface()->pitchinpix == 4);
  for (unsigned y = 0; y < 2; ++y)
    for (unsigned x = 0; x < 4; ++x) {
      const uint32 expected = format.opp == 2 ? input.pixels16[y * 6 + x] : input.pixels[y * 6 + x];
      const uint32 actual = format.opp == 2 ? restored.surface()->pixels16[y * 4 + x]
                                           : restored.surface()->pixels[y * 4 + x];
      assert(expected == actual);
    }
  auto bytes=frame.encode();
  for(int invalid : {-2,2}) {
    bool caught=false;
    try { frame.capture(input,{1,0,0,2},widths,invalid); } catch(const std::exception&) {caught=true;}
    assert(caught && frame.encode()==bytes);
    auto bad=bytes;
    for(unsigned i=0;i<4;++i)bad[bad.size()-4+i]=uint32(invalid)>>(8*i);
    caught=false;
    try { auto rejected=EmucapCompletedFrame::decode(bad); } catch(const std::exception&) {caught=true;}
    assert(caught && frame.encode()==bytes);
  }
  for(size_t n=0;n<bytes.size();++n) {
    bool caught=false;
    try { auto rejected=EmucapCompletedFrame::decode({bytes.begin(),bytes.begin()+n}); }
    catch(const std::exception&) {caught=true;}
    assert(caught);
  }
  auto old=bytes;old[7]='1';bool caught=false;
  try { auto rejected=EmucapCompletedFrame::decode(old); } catch(const std::exception&) {caught=true;}
  assert(caught);
  EmucapCompletedFrame empty;
  assert(empty.encode().size()==12 && EmucapCompletedFrame::decode(empty.encode()).field()==-1);
  restored.swap(empty);assert(empty.field()==field && restored.field()==-1);
  restored.swap(empty);assert(restored.field()==field && empty.field()==-1);
  const auto* allocated=frame.surface();
  frame.capture(input,{1,0,0,2},widths,field==1?0:1);
  assert(frame.surface()==allocated && frame.field()==(field==1?0:1));
  frame.capture(input,{1,0,0,2},widths,field);
  input.Fill(0);
  assert(restored.encode() == frame.encode());
 }
}
int main() {
  unsigned shifts[] = {0, 8, 16, 24};
  do {
    check(MDFN_PixelFormat_MakeTag(MDFN_COLORSPACE_RGB, 4,
          shifts[0], shifts[1], shifts[2], shifts[3], 8, 8, 8, 8));
  } while (std::next_permutation(shifts, shifts + 4));
  check(MDFN_PixelFormat::IRGB16_1555);
  check(MDFN_PixelFormat::RGBI16_5551);
  check(MDFN_PixelFormat::RGB16_565);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-frame-codec-') as temp:
    cpp = Path(temp) / 'test.cpp'
    executable = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run([
        'clang++', '-std=c++11', '-DHAVE_CONFIG_H',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I' + str(source / 'include'), '-I' + str(source / 'intl'),
        '-I' + str(adapter), str(cpp),
        str(source / 'src/video/surface.cpp'), str(source / 'src/video/convert.cpp'),
        str(source / 'src/error.cpp'), str(source / 'src/libtrio.a'),
        '-o', str(executable),
    ], check=True)
    subprocess.run([str(executable)], check=True,
                   env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1'))
print('Native frame codec: three field values, malformed fields and truncations; 24 packed 32-bit formats and three 16-bit formats pass ASan/UBSan')
