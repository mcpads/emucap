#!/usr/bin/env python3
"""Verify native presentation conversion parity without modifying driver storage."""
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
#include "emucap_presentation_surface.h"
#include <algorithm>
#include <cassert>
#include <cstdlib>
#include <new>
#include <vector>
static bool fail_allocation=false;
void* operator new(std::size_t size) {
 if(fail_allocation) {fail_allocation=false;throw std::bad_alloc();}
 if(auto p=std::malloc(size?size:1))return p;throw std::bad_alloc();
}
void operator delete(void* p) noexcept {std::free(p);}
using namespace Mednafen;
std::vector<uint8> bytes(const MDFN_Surface& s) {
 const auto* p=s.format.opp==2?static_cast<const void*>(s.pixels16):static_cast<const void*>(s.pixels);
 const auto* b=static_cast<const uint8*>(p);
 return {b,b+std::size_t(s.pitchinpix)*s.h*s.format.opp};
}
uint32 pixel(const MDFN_Surface& s,unsigned x,unsigned y) {
 return s.format.opp==2?s.pixels16[y*s.pitchinpix+x]:s.pixels[y*s.pitchinpix+x];
}
void fill(MDFN_Surface& s,unsigned n) {
 for(int y=0;y<s.h;++y)for(int x=0;x<s.pitchinpix;++x) {
  uint32 v=s.MakeColor((x*53+n*47)&255,(y*89+n*23)&255,(x*71+y*41+n*13)&255,255);
  if(s.format.opp==2)s.pixels16[y*s.pitchinpix+x]=v;else s.pixels[y*s.pitchinpix+x]=v;
 }
}
int main() {
 std::vector<uint64> formats;unsigned shifts[]={0,8,16,24};
 do{formats.push_back(MDFN_PixelFormat_MakeTag(MDFN_COLORSPACE_RGB,4,
       shifts[0],shifts[1],shifts[2],shifts[3],8,8,8,8));}while(std::next_permutation(shifts,shifts+4));
 formats.insert(formats.end(),{MDFN_PixelFormat::IRGB16_1555,MDFN_PixelFormat::RGBI16_5551,MDFN_PixelFormat::RGB16_565});
 unsigned native_mutations=0;
 EmucapPresentationSurface presentation;
 for(auto from:formats)for(auto to:formats)for(int width:{4,7}) {
  MDFN_Surface source(nullptr,width,3,width+3,MDFN_PixelFormat(from));
  const MDFN_Surface* last=nullptr;
  for(unsigned n=0;n<3;++n) {
   fill(source,n);const auto before=bytes(source);auto* p32=source.pixels;auto* p16=source.pixels16;
   MDFN_Surface reference(nullptr,width,3,width+3,MDFN_PixelFormat(from));fill(reference,n);
   reference.SetFormat(MDFN_PixelFormat(to),true);
   native_mutations+=(reference.format.tag!=from);
   const auto& result=presentation.convert(source,MDFN_PixelFormat(to));
   assert(result.format.tag==to && result.w==width && result.h==3);
   for(unsigned y=0;y<3;++y)for(int x=0;x<width;++x)assert(pixel(result,x,y)==pixel(reference,x,y));
   assert(bytes(source)==before && source.format.tag==from && source.pixels==p32 && source.pixels16==p16);
   if(last)assert(last==&result);last=&result;
   if(from==to)assert(&result==&source);else assert(&result!=&source);
   fail_allocation=true;assert(&presentation.convert(source,MDFN_PixelFormat(to))==last);
   assert(fail_allocation);fail_allocation=false;
  }
 }
 assert(native_mutations>0);
 MDFN_Surface source(nullptr,4,3,6,MDFN_PixelFormat::ABGR32_8888);fill(source,1);
 presentation.clear();
 auto& last=presentation.convert(source,MDFN_PixelFormat::RGB16_565);
 const auto previous=bytes(last),unchanged=bytes(source);
 bool failed=false;fail_allocation=true;
 try{presentation.convert(source,MDFN_PixelFormat::IRGB16_1555);}catch(const std::bad_alloc&){failed=true;}
 assert(failed && bytes(last)==previous && bytes(source)==unchanged);
 assert(&presentation.convert(source,MDFN_PixelFormat::RGB16_565)==&last);
 auto reject=[&](uint64 target) {
  bool rejected=false;try{presentation.convert(source,MDFN_PixelFormat(target));}catch(const std::exception&){rejected=true;}
  assert(rejected && bytes(last)==previous);
 };
 source.w=7;reject(MDFN_PixelFormat::RGB16_565);source.w=4;
 source.h=0;reject(MDFN_PixelFormat::RGB16_565);source.h=3;
 reject(0);assert(bytes(source)==unchanged);
 presentation.clear();assert(presentation.convert(source,source.format).format==source.format);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-presentation-surface-') as temp:
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
print('Presentation: 729 format pairs, two padded geometries, three updates, native conversion '
      'parity, unchanged source, storage reuse, allocation failure and invalid views pass ASan/UBSan')
