#!/usr/bin/env python3
"""Exercise native video-processing transaction ownership and continuation."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
source = args.source.resolve()
adapter = Path(__file__).resolve().parents[2] / 'adapters/mednafen'
native = (source / 'src/mednafen.cpp').read_text()
owner = native[native.index('static bool PrevInterlaced;'):native.index('static bool FFDiscard')]
start = native.index(' if(espec->InterlaceOn)', native.index('void MDFNI_Emulate('))
process = native[start:native.index(' ProcessAudio(espec);', start)]
code = r'''
#include <mednafen/mednafen.h>
#include <mednafen/MemoryStream.h>
#include <mednafen/video/Deinterlacer.h>
#include <mednafen/video/tblur.h>
#include <mednafen/video/VideoHistoryIO.h>
#include <cassert>
namespace Mednafen {
void MDFN_Notify(MDFN_NoticeType, const char*, ...) noexcept { abort(); }
const std::vector<InputPortInfoStruct> ports;
const std::vector<CheatFormatStruct> cheat_formats;
const CheatInfoStruct cheats{nullptr,nullptr,nullptr,nullptr,cheat_formats,false};
MDFNGI game{nullptr,nullptr,nullptr,ModPrio(0),nullptr,ports,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,0,cheats};
MDFNGI other_game{nullptr,nullptr,nullptr,ModPrio(0),nullptr,ports,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,nullptr,0,cheats};
MDFNGI* MDFNGameInfo = &game;
''' + owner + r'''
void process_native(EmulateSpecStruct* espec) {
''' + process + r'''
 if(TBlur_IsOn()) TBlur_Run(espec);
}
}
using namespace Mednafen;
void initialize(unsigned mode, unsigned blur) {
 MDFNGameInfo = &game;
 game.fb_width = game.fb_height = 8;
 deint.reset(Deinterlacer::Create(mode)); deint_mode = mode; PrevInterlaced = false;
 TBlur_Kill(); if(blur) TBlur_Init(blur == 2, 50., 8, 8);
}
std::vector<uint8> save() {
 MemoryStream stream; MDFNI_SaveVideoHistory(&stream);
 return {stream.map(), stream.map() + stream.map_size()};
}
struct FaultingStream : MemoryStream {
 int reads=0, fail_at=-1;
 uint64 read(void* dest, uint64 count, bool error=true) override {
  if(reads++ == fail_at) throw std::runtime_error("injected read failure");
  return MemoryStream::read(dest, count, error);
 }
};
std::unique_ptr<MDFNVideoHistory> prepare(const std::vector<uint8>& bytes, int fail=-1) {
 FaultingStream stream; stream.write(bytes.data(), bytes.size()); stream.rewind();stream.fail_at=fail;
 return MDFNI_PrepareVideoHistory(&stream);
}
std::vector<uint32> frame(uint64 tag, unsigned n, bool interlaced=true) {
 MDFN_Surface surface(nullptr,8,8,10,MDFN_PixelFormat(tag));
 int32 widths[8]; for(auto& w:widths) w=8;
 EmulateSpecStruct spec; spec.surface=&surface;spec.LineWidths=widths;
 spec.DisplayRect={0,0,8,8};spec.InterlaceOn=interlaced;spec.InterlaceField=n%2;
 for(unsigned y=0;y<8;++y) for(unsigned x=0;x<8;++x) {
  auto pixel=surface.MakeColor((n*37+x*17)&255,(y*23+n*31)&255,(x*11+n*47)&255,255);
  if(surface.format.opp==2) surface.pixels16[y*10+x]=pixel;else surface.pixels[y*10+x]=pixel;
 }
 process_native(&spec);
 std::vector<uint32> result;
 for(unsigned y=0;y<8;++y) for(unsigned x=0;x<8;++x)
  result.push_back(surface.format.opp==2?surface.pixels16[y*10+x]:surface.pixels[y*10+x]);
 return result;
}
int main() {
 const uint64 tags[]={MDFN_PixelFormat::ABGR32_8888,MDFN_PixelFormat::IRGB16_1555,MDFN_PixelFormat::RGB16_565};
 unsigned sensitive=0;
 for(unsigned mode=0;mode<5;++mode) for(unsigned blur=0;blur<3;++blur)
  for(auto tag:tags) for(bool previous:{false,true}) {
   initialize(mode,blur);frame(tag,0);frame(tag,1,previous);
   const auto origin=save();
   std::vector<std::vector<uint32>> expected;
   for(unsigned n=2;n<10;++n) expected.push_back(frame(tag,n,n!=5));
   const auto destination=save();
   auto staged=prepare(origin);
   assert(save()==destination);
   assert(MDFNI_CommitVideoHistory(*staged)); assert(save()==origin);
   assert(PrevInterlaced==previous);
   assert(MDFNI_CommitVideoHistory(*staged)); assert(save()==destination);
   assert(MDFNI_CommitVideoHistory(*staged)); assert(save()==origin);
   for(unsigned n=2;n<10;++n) {
    assert(frame(tag,n,n!=5)==expected[n-2]);
    auto again=prepare(save()); assert(MDFNI_CommitVideoHistory(*again));
   }
   auto wrong=frame(tag,2); sensitive+=(wrong!=expected[0]);
   auto unchanged=save();
   auto reject=[&](const std::vector<uint8>& bytes, int fault=-1) {
    bool failed=false;try {auto p=prepare(bytes,fault);} catch(const std::exception&) {failed=true;}
    assert(failed && save()==unchanged);
   };
   for(size_t length=0;length<origin.size();++length) reject({origin.begin(),origin.begin()+length});
   for(int fault=0;fault<9;++fault) reject(origin,fault);
   auto bad=origin;bad.push_back(0);reject(bad);
   for(unsigned offset:{0u,8u,12u,16u,20u,31u,39u}) {
    bad=origin;bad[offset]=255;reject(bad);
   }
   // A failure in the second owner must not install the first owner's decoded state.
   bad=origin;bad.back()^=255;
   const auto deint_length=MDFN_de64lsb(bad.data()+24);
   bad[40+deint_length]='X'; reject(bad);
   staged=prepare(origin);
   game.fb_width=9;assert(!MDFNI_CommitVideoHistory(*staged));game.fb_width=8;
   deint_mode=(mode+1)%5;assert(!MDFNI_CommitVideoHistory(*staged));deint_mode=mode;
   MDFNGameInfo=&other_game;assert(!MDFNI_CommitVideoHistory(*staged));MDFNGameInfo=&game;
   assert(save()==unchanged);
   TBlur_Kill();if(blur!=1) TBlur_Init(false,50.,8,8);
   unchanged=save();assert(!MDFNI_CommitVideoHistory(*staged));assert(save()==unchanged);
   TBlur_Kill();if(blur) TBlur_Init(blur==2,50.,8,8);
   assert(MDFNI_CommitVideoHistory(*staged));assert(save()==origin);
  }
 assert(sensitive>0);
 TBlur_Kill();deint.reset();MDFNGameInfo=nullptr;
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-video-history-') as temp:
    cpp = Path(temp) / 'test.cpp'
    executable = Path(temp) / 'test'
    cpp.write_text(code)
    units = ['video/surface.cpp', 'video/convert.cpp', 'video/Deinterlacer.cpp',
             'video/Deinterlacer_Simple.cpp', 'video/Deinterlacer_Blend.cpp', 'video/tblur.cpp',
             'Stream.cpp', 'MemoryStream.cpp', 'error.cpp']
    platform_libs = ['-liconv', '-framework', 'CoreFoundation'] if sys.platform == 'darwin' else []
    subprocess.run(['clang++', '-std=c++11', '-DHAVE_CONFIG_H', '-O1',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I' + str(source / 'include'), '-I' + str(source / 'intl'), '-I' + str(adapter),
        str(cpp), *[str(source / 'src' / name) for name in units],
        str(source / 'src/libtrio.a'), str(source / 'intl/libintl.a'), *platform_libs,
        '-o', str(executable)], check=True)
    subprocess.run([str(executable)], check=True,
        env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1'))
print('Native video history: 90 configurations, eight continuations, interlace transitions, '
      'reversible commits, malformed blocks, read faults and configuration drift pass ASan/UBSan')
