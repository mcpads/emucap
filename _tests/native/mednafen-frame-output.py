#!/usr/bin/env python3
"""Check reversible native in-flight output without replacing host policy."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[2]
source = root / 'adapters/mednafen/work/mednafen'
adapter = root / 'adapters/mednafen'
code = r'''
#include "emucap_frame_output.h"
#include <cassert>
#include <cstdlib>
#include <new>
static bool fail_alloc = false;
static std::size_t largest_alloc = 0;
void* operator new(std::size_t size) {
 largest_alloc=std::max(largest_alloc,size);
 if(fail_alloc) throw std::bad_alloc();
 if(auto p=std::malloc(size ? size : 1)) return p;
 throw std::bad_alloc();
}
void operator delete(void* p) noexcept { std::free(p); }
using namespace Mednafen;
struct Output {
 MDFN_Surface surface{nullptr,8,8,8,MDFN_PixelFormat(MDFN_PixelFormat::ABGR32_8888)};
 int32 widths[8] = {-1,8,8,8,8,8,8,8};
 int16 sound[32];
 EmulateSpecStruct spec;
 Output(unsigned channels,int count,int seed,bool enabled=true) {
  spec.surface=&surface; spec.LineWidths=widths;
  spec.DisplayRect={1,2,4,5};spec.InterlaceOn=true;spec.InterlaceField=bool(seed&1);
  spec.VideoFormatChanged=bool(seed&2);spec.SoundFormatChanged=bool(seed&4);
  spec.NeedSoundReverse=bool(seed&8);
  spec.SoundBuf=enabled ? sound : nullptr;
  spec.SoundRate=enabled ? 48000 : 0;spec.SoundBufMaxSize=enabled ? 16 : 0;
  spec.SoundBufSize=count;spec.SoundBufSize_InternalProcessed=count/2;
  spec.SoundBufSize_DriverProcessed=count/3;
  spec.MasterCycles=seed+100;spec.MasterCycles_InternalProcessed=seed+20;
  spec.MasterCycles_DriverProcessed=seed;
  for(unsigned i=0;i<32;++i)sound[i]=seed+i;
 }
};
bool equal_output(const EmulateSpecStruct& a,const EmulateSpecStruct& b,unsigned channels) {
 return a.DisplayRect.x==b.DisplayRect.x && a.DisplayRect.y==b.DisplayRect.y
  && a.DisplayRect.w==b.DisplayRect.w && a.DisplayRect.h==b.DisplayRect.h
  && a.InterlaceOn==b.InterlaceOn && a.InterlaceField==b.InterlaceField
  && a.VideoFormatChanged==b.VideoFormatChanged && a.SoundFormatChanged==b.SoundFormatChanged
  && a.NeedSoundReverse==b.NeedSoundReverse && a.SoundBufSize==b.SoundBufSize
  && a.SoundBufSize_InternalProcessed==b.SoundBufSize_InternalProcessed
  && a.SoundBufSize_DriverProcessed==b.SoundBufSize_DriverProcessed
  && a.MasterCycles==b.MasterCycles && a.MasterCycles_InternalProcessed==b.MasterCycles_InternalProcessed
  && a.MasterCycles_DriverProcessed==b.MasterCycles_DriverProcessed
  && (!a.SoundBufSize || std::equal(a.SoundBuf,a.SoundBuf+a.SoundBufSize*channels,b.SoundBuf));
}
int main() {
 for(unsigned channels : {1U,2U}) for(int saved_count : {0,1,8,16})
 for(int old_count : {0,2,12,16}) {
  Output origin(channels,saved_count,7),destination(channels,old_count,18),before(channels,old_count,18);
  destination.spec.SoundVolume=.3;destination.spec.soundmultiplier=4;
  destination.spec.skip=1;destination.spec.NeedRewind=true;
  origin.sound[0]=-32768;origin.sound[1]=32767;
  origin.spec.MasterCycles += int64(1)<<40;
  auto state=EmucapFrameOutput::capture(origin.spec,channels);
  const auto bytes=state.encode();
  assert(bytes.size()==84+saved_count*channels*2);
  assert(bytes[8]==channels && bytes[20]==8 && bytes[24]==8);
  const uint8_t rate_bytes[8]={0,0,0,0,0,0x70,0xe7,0x40};
  assert(std::equal(rate_bytes,rate_bytes+8,bytes.begin()+12));
  assert(bytes[28]==1 && bytes[32]==2 && bytes[36]==4 && bytes[40]==5);
  assert(bytes[48]==saved_count && bytes[65]==1);
  if(saved_count) assert(bytes[84]==0 && bytes[85]==128);
  if(saved_count*channels>1) assert(bytes[86]==255 && bytes[87]==127);
  state=EmucapFrameOutput::decode(bytes);assert(state.encode()==bytes);
  state.prepare(destination.spec,channels);
  fail_alloc=true;
  assert(state.swap_into(destination.spec,channels));
  assert(equal_output(destination.spec,origin.spec,channels));
  assert(destination.spec.surface==&destination.surface && destination.spec.LineWidths==destination.widths);
  assert(destination.spec.SoundBuf==destination.sound && destination.spec.SoundRate==48000);
  assert(destination.spec.SoundVolume==.3 && destination.spec.soundmultiplier==4);
  assert(destination.spec.skip==1 && destination.spec.NeedRewind);
  assert(state.swap_into(destination.spec,channels));
  assert(equal_output(destination.spec,before.spec,channels));
  fail_alloc=false;
 }
 Output muted(2,0,7,false),muted_dest(2,0,18,false);
 auto silent=EmucapFrameOutput::decode(EmucapFrameOutput::capture(muted.spec,2).encode());silent.prepare(muted_dest.spec,2);
 assert(silent.swap_into(muted_dest.spec,2));assert(equal_output(muted.spec,muted_dest.spec,2));
 Output origin(2,8,7),dest(2,12,18),before(2,12,18);
 auto state=EmucapFrameOutput::capture(origin.spec,2);
 fail_alloc=true;
 try {state.prepare(dest.spec,2);assert(false);}catch(const std::bad_alloc&){}
 fail_alloc=false;
 assert(equal_output(dest.spec,before.spec,2));
 state.prepare(dest.spec,2);
 for(int fault=0;fault<9;++fault) {
  auto bad=dest.spec;
  switch(fault) {
   case 0: bad.SoundRate=44100;break;
   case 1: bad.SoundBufSize=-1;break;
   case 2: bad.SoundBufSize=17;break;
   case 3: bad.SoundBufSize_DriverProcessed=bad.SoundBufSize_InternalProcessed+1;break;
   case 4: bad.MasterCycles_InternalProcessed=bad.MasterCycles+1;break;
   case 5: bad.DisplayRect.x=9;break;
   case 6: bad.SoundBuf=nullptr;break;
   case 7: bad.SoundRate=std::nan("");break;
   case 8: bad.SoundBufMaxSize=2147483647;break;
  }
  assert(!state.swap_into(bad,2));assert(equal_output(dest.spec,before.spec,2));
 }
 assert(!state.swap_into(dest.spec,1));
 auto moved=std::move(state);assert(!state.swap_into(dest.spec,2));
 assert(moved.swap_into(dest.spec,2));assert(equal_output(dest.spec,origin.spec,2));
 // Staging rejects complete malformed headers and lengths before sample allocation.
 auto bytes=EmucapFrameOutput::capture(origin.spec,2).encode();
 const auto reject=[&](const std::vector<uint8_t>& bad) {
  largest_alloc=0;
  try {auto unused=EmucapFrameOutput::decode(bad);assert(false);}
  catch(const std::runtime_error&) {}
  assert(largest_alloc<4096);
  assert(equal_output(dest.spec,origin.spec,2));
 };
 for(std::size_t n=0;n<bytes.size();++n)
  reject(std::vector<uint8_t>(bytes.begin(),bytes.begin()+n));
 auto set=[&](std::vector<uint8_t>& target,std::size_t offset,uint64_t value,unsigned count) {
  for(unsigned i=0;i<count;++i)target[offset+i]=uint8_t(value>>(i*8));
 };
 for(unsigned fault=0;fault<15;++fault) {
  auto bad=bytes;
  switch(fault) {
   case 0:bad[0]='?';break;
   case 1:set(bad,8,3,4);break;
   case 2:set(bad,12,0x7ff8000000000000ULL,8);break;
   case 3:set(bad,12,0x7ff0000000000000ULL,8);break;
   case 4:set(bad,12,0,8);break;
   case 5:set(bad,20,0,4);break;
   case 6:set(bad,28,0xffffffff,4);break;
   case 7:set(bad,36,9,4);break;
   case 8:set(bad,44,32,4);break;
   case 9:set(bad,48,0x7fffffff,4);break;
   case 10:set(bad,52,9,4);break;
   case 11:set(bad,56,9,4);break;
   case 12:set(bad,60,0xffffffffffffffffULL,8);break;
   case 13:set(bad,76,1000,8);break;
   case 14:bad.push_back(0);break;
  }
  reject(bad);
 }
 // A valid-sized declaration with missing samples must fail before a large allocation.
 auto short_payload=bytes;set(short_payload,48,1024*1024,4);reject(short_payload);
 fail_alloc=true;
 try {auto unused=EmucapFrameOutput::decode(bytes);assert(false);}catch(const std::bad_alloc&){}
 fail_alloc=false;
 assert(equal_output(dest.spec,origin.spec,2));
 // Maximum admitted payload and the first byte beyond its bound.
 std::vector<int16> maximum(2*1024*1024,-32768);
 auto large=origin.spec;large.SoundBuf=maximum.data();large.SoundBufSize=maximum.size();
 large.SoundBufMaxSize=maximum.size();
 auto maximum_bytes=EmucapFrameOutput::capture(large,1).encode();
 assert(maximum_bytes.size()==84+4*1024*1024);
 assert(EmucapFrameOutput::decode(maximum_bytes).encode()==maximum_bytes);
 maximum_bytes.push_back(0);reject(maximum_bytes);
 // Capture validates before allocating or reading any alleged initialized range.
 auto bad=origin.spec;bad.SoundBufSize=17;
 try {auto unused=EmucapFrameOutput::capture(bad,2);assert(false);}catch(const std::runtime_error&){}
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-frame-output-') as temp:
    cpp = Path(temp) / 'test.cpp'
    binary = Path(temp) / 'test'
    cpp.write_text(code)
    units = ['video/surface.cpp', 'video/convert.cpp', 'error.cpp']
    platform_libs = ['-liconv', '-framework', 'CoreFoundation'] if sys.platform == 'darwin' else []
    subprocess.run(['clang++', '-std=c++11', '-DHAVE_CONFIG_H', '-O1',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I' + str(source / 'include'), '-I' + str(source / 'intl'), '-I' + str(adapter),
        str(cpp), *[str(source / 'src' / unit) for unit in units],
        str(source / 'src/libtrio.a'), str(source / 'intl/libintl.a'), *platform_libs,
        '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True,
        env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1'))
print('Native frame output: mono/stereo/muted exchange, host policy and pointer preservation, '
      'portable codec, truncation/metadata/size rejection, maximum payload, allocation-free rollback and failures pass ASan/UBSan')
