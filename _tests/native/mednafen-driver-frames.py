#!/usr/bin/env python3
"""Check native driver frame codec, stable pointer restoration and reversible role mapping."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
source = parser.parse_args().source.resolve()
adapter = Path(__file__).resolve().parents[2] / 'adapters/mednafen'
code = r'''
#include <mednafen/mednafen.h>
#include "emucap_driver_frames.h"
#include <cassert>
#include <cstdlib>
#include <new>
static int fail_after=-1;
void* operator new(std::size_t size) {
 if(fail_after==0) { fail_after=-1;throw std::bad_alloc(); }
 if(fail_after>0) --fail_after;
 if(auto p=std::malloc(size?size:1)) return p;
 throw std::bad_alloc();
}
void operator delete(void* p) noexcept { std::free(p); }
using namespace Mednafen;
struct Buffers {
 MDFN_Surface a,b;
 MDFN_Rect rect[2]={{1,1,3,2},{0,0,4,3}};
 int32 widths[2][4]={{-1,0,3,4},{4,3,0,-1}};
 int fields[2]={0,1};
 std::array<EmucapDriverFrames::View,2> views;
 Buffers(uint64 tag,unsigned seed):a(nullptr,4,4,6,MDFN_PixelFormat(tag)),
                                  b(nullptr,4,4,7,MDFN_PixelFormat(tag)) {
  views={{{&a,&rect[0],widths[0],&fields[0]}, {&b,&rect[1],widths[1],&fields[1]}}};
  for(unsigned i=0;i<2;++i) {
   auto& s=*views[i].surface;
   for(int y=0;y<s.h;++y) for(int x=0;x<s.pitchinpix;++x) {
    auto value=x<s.w?seed+i*100+y*10+x:0xA5A5;
    if(s.format.opp==2)s.pixels16[y*s.pitchinpix+x]=value;else s.pixels[y*s.pitchinpix+x]=value;
   }
  }
 }
 void padding() const {
  for(auto view:views) {const auto& s=*view.surface;
   for(int y=0;y<s.h;++y)for(int x=s.w;x<s.pitchinpix;++x)
    assert((s.format.opp==2?s.pixels16[y*s.pitchinpix+x]:s.pixels[y*s.pitchinpix+x])==0xA5A5);
  }
 }
};
auto snapshot(Buffers& b,unsigned back)->std::vector<uint8> {
 return EmucapDriverFrames::capture(b.views,back).encode();
}
int main() {
 for(uint64 tag:{MDFN_PixelFormat::ABGR32_8888,MDFN_PixelFormat::IRGB16_1555,MDFN_PixelFormat::RGB16_565})
 for(unsigned saved_back=0;saved_back<2;++saved_back)
 for(unsigned live_back=0;live_back<2;++live_back) {
  Buffers source(tag,1000),dest(tag,2000);
  const auto original=snapshot(source,saved_back),previous=snapshot(dest,live_back);
  // Independent little-endian and role-order observations.
  assert(original[8]==4 && original[12]==4);
  const unsigned first_pixel=8+36+16;
  unsigned value=1000+saved_back*100;
  assert(original[first_pixel]==(value&255) && original[first_pixel+1]==(value>>8));
  auto staged=EmucapDriverFrames::decode(original);
  auto* a=dest.a.pixels;auto* b=dest.b.pixels;auto* a16=dest.a.pixels16;auto* b16=dest.b.pixels16;
  auto* wa=dest.views[0].widths;auto* wb=dest.views[1].widths;
  const auto& inspection=staged;
  fail_after=0;assert(inspection.can_swap_into(dest.views,live_back));assert(fail_after==0);fail_after=-1;
  assert(snapshot(dest,live_back)==previous && staged.encode()==original);
  fail_after=0;assert(staged.swap_into(dest.views,live_back));assert(fail_after==0);fail_after=-1;
  assert(snapshot(dest,live_back)==original && staged.encode()==previous);
  assert(a==dest.a.pixels && b==dest.b.pixels && a16==dest.a.pixels16 && b16==dest.b.pixels16);
  assert(wa==dest.views[0].widths && wb==dest.views[1].widths);dest.padding();
  assert(staged.swap_into(dest.views,live_back));assert(snapshot(dest,live_back)==previous);
  auto reject=[&](const std::vector<uint8>& bytes) {
   bool caught=false;try{auto invalid=EmucapDriverFrames::decode(bytes);}catch(const std::exception&){caught=true;}
   assert(caught && snapshot(dest,live_back)==previous);
  };
  for(size_t n=0;n<original.size();++n) reject({original.begin(),original.begin()+n});
  auto bad=original;bad.push_back(0);reject(bad);
  for(unsigned offset:{0u,8u,12u,23u,24u,28u,32u,36u,40u,44u}) {
   bad=original;for(unsigned i=0;i<(offset==0||offset==23?1:4);++i)bad[offset+i]=0xFE;reject(bad);
  }
  unsigned second=8+36+16+16*source.a.format.opp;
  bad=original;bad[second]=0;reject(bad);
  for(int n=0;n<4;++n) {
   bool caught=false;fail_after=n;
   try{auto pending=EmucapDriverFrames::decode(original);}catch(const std::bad_alloc&){caught=true;}
   fail_after=-1;assert(caught && snapshot(dest,live_back)==previous);
  }
  // Preflight the second live buffer before changing the first.
  assert(inspection.can_swap_into(dest.views,live_back));
  dest.b.w=3;assert(!inspection.can_swap_into(dest.views,live_back));assert(!staged.swap_into(dest.views,live_back));dest.b.w=4;
  assert(snapshot(dest,live_back)==previous && staged.encode()==original);
  dest.fields[1]=2;assert(!staged.swap_into(dest.views,live_back));dest.fields[1]=1;
  assert(!staged.swap_into(dest.views,2));
  auto alias=dest.views;alias[1]=alias[0];assert(!staged.swap_into(alias,live_back));
  assert(snapshot(dest,live_back)==previous && staged.encode()==original);
  assert(staged.swap_into(dest.views,live_back));assert(snapshot(dest,live_back)==original);
 }
 // Same storage width permits exact channel-format restoration without reallocating.
 Buffers source(MDFN_PixelFormat::ABGR32_8888,100),dest(MDFN_PixelFormat::ARGB32_8888,200);
 auto staged=EmucapDriverFrames::decode(snapshot(source,0));auto before=snapshot(dest,1);
 assert(staged.swap_into(dest.views,1));assert(snapshot(dest,1)==snapshot(source,0));
 assert(staged.swap_into(dest.views,1));assert(snapshot(dest,1)==before);
 Buffers narrow(MDFN_PixelFormat::RGB16_565,300);auto narrow_before=snapshot(narrow,0);
 assert(!staged.swap_into(narrow.views,0));assert(snapshot(narrow,0)==narrow_before);
 auto moved=std::move(staged);
 assert(!staged.can_swap_into(dest.views,1));
 assert(!staged.swap_into(dest.views,1));assert(snapshot(dest,1)==before);
 assert(moved.swap_into(dest.views,1));assert(snapshot(dest,1)==snapshot(source,0));
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-driver-frames-') as temp:
    cpp = Path(temp) / 'test.cpp'
    executable = Path(temp) / 'test'
    cpp.write_text(code)
    units = ['video/surface.cpp', 'video/convert.cpp', 'error.cpp']
    platform_libs = ['-liconv', '-framework', 'CoreFoundation'] if sys.platform == 'darwin' else []
    subprocess.run(['clang++', '-std=c++11', '-DHAVE_CONFIG_H', '-O1',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I' + str(source / 'include'), '-I' + str(source / 'intl'), '-I' + str(adapter),
        str(cpp), *[str(source / 'src' / name) for name in units],
        str(source / 'src/libtrio.a'), str(source / 'intl/libintl.a'), *platform_libs,
        '-o', str(executable)], check=True)
    subprocess.run([str(executable)], check=True,
        env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1'))
print('Native driver frames: 3 formats, 4 role mappings, stable allocations, padding, '
      'nonmutating preflight, rollback, malformed blocks, allocation failures and configuration drift pass ASan/UBSan')
