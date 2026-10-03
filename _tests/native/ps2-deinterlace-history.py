#!/usr/bin/env python3
"""GS field history preserves exact pixels and rejects malformed trailers before publication."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
native = root / 'adapters/pcsx2/work/pcsx2/pcsx2/GS/Renderers/Common'
code = r'''
#include "GSDeinterlaceHistory.h"
#include <cassert>
int main() {
 GSDeinterlaceHistory h;
 h.index=3;h.images[0]={2,2,{0xabcdef12,2,3,4}};h.images[1]={1,2,{5,6}};
 const auto bytes=h.Encode();GSDeinterlaceHistory decoded;
 assert(GSDeinterlaceHistory::Decode(bytes.data(),bytes.size(),decoded));
 assert(decoded.Encode()==bytes);
 for (std::size_t n=1;n<bytes.size();n++) {
  GSDeinterlaceHistory sentinel=h;
  assert(!GSDeinterlaceHistory::Decode(bytes.data(),n,sentinel));assert(sentinel.Encode()==bytes);
 }
 for(unsigned offset : {0u,4u,8u,12u,16u}) {
  auto bad=bytes;for(unsigned i=0;i<4;i++)bad[offset+i]=255;
  assert(!GSDeinterlaceHistory::Decode(bad.data(),bad.size(),decoded));
 }
 auto extra=bytes;extra.push_back(0);assert(!GSDeinterlaceHistory::Decode(extra.data(),extra.size(),decoded));
 assert(GSDeinterlaceHistory::Decode(nullptr,0,decoded));assert(decoded.index==0&&decoded.images[0].pixels.empty());
}
'''

with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / 'probe.cpp'
    binary = Path(directory) / 'probe'
    source.write_text(code)
    subprocess.run(['clang++', '-std=c++20', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined', '-I', str(native), str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('PCSX2 field history: round trip, every truncation, invalid dimensions/index/version, trailing bytes and legacy reset passed')
