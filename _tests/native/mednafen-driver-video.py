#!/usr/bin/env python3
"""Exercise the native driver video transaction with instrumented mutex ownership."""
import ast
import os
from pathlib import Path
import subprocess
import tempfile
import sys

root = Path(__file__).resolve().parents[2]
source = root / 'adapters/mednafen/work/mednafen'
adapter = root / 'adapters/mednafen'
fixture = ast.parse((Path(__file__).with_name('mednafen-driver-frames.py')).read_text())
code = next(ast.literal_eval(n.value) for n in fixture.body
            if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'code' for t in n.targets))
code = code[:code.index('int main()')]
code += r'''
#include "emucap_driver_video.h"
#include <atomic>
#include <mutex>
#include <thread>
#include <exception>
namespace MThreading {
 struct Mutex { std::mutex value; };
 struct Sem {};
 static bool fail_lock=false,fail_unlock=false;
 static std::atomic<bool> contended{false};
 bool Mutex_Lock(Mutex* p) noexcept {
  if(fail_lock)return false;
  if(!p->value.try_lock()) {contended=true;p->value.lock();}
  return true;
 }
 bool Mutex_Unlock(Mutex* p) noexcept {p->value.unlock();return !fail_unlock;}
 bool Sem_Post(Sem*) noexcept {return true;}
}
static struct {
 std::unique_ptr<MDFN_Surface> surface;
 MDFN_Rect rect;
 std::unique_ptr<int32[]> lw;
 int field;
} SoftFB[2];
static bool SoftFB_BackBuffer=false;
static std::atomic<int> VTReady{-1};
static unsigned VTRotated=0;
static bool VTSSnapshot=false,pending_ssnapshot=false;
static MThreading::Mutex mutex;
static MThreading::Mutex* VTMutex=&mutex;
static MThreading::Sem* VTWakeupSem=nullptr;
static struct Game {unsigned rotated=3;} game;
static Game* CurGame=&game;
static const MDFN_Surface* displayed=nullptr;
static int displayed_field=-99;
static unsigned displayed_rotation=99;
static bool displayed_snapshot=false;
void BlitScreen(const MDFN_Surface* s,const MDFN_Rect*,const int32*,int r,int f,bool snap) {
 displayed=s;displayed_field=f;displayed_rotation=r;displayed_snapshot=snap;
}
#include "emucap_driver_video.inc"
int main(int argc,char**) {
 if(argc>1) {
  std::set_terminate([]{std::_Exit(77);});
  MThreading::fail_unlock=true;
  {EmucapDriverVideo guard;}
  return 1;
 }
 for(unsigned i=0;i<2;++i) {
  SoftFB[i].surface.reset(new MDFN_Surface(nullptr,4,4,4,MDFN_PixelFormat(MDFN_PixelFormat::ABGR32_8888)));
  SoftFB[i].rect={0,0,4,4};SoftFB[i].field=-1;
  SoftFB[i].lw.reset(new int32[4]{-1,4,4,4});
  for(unsigned j=0;j<16;++j)SoftFB[i].surface->pixels[j]=200+i;
 }
 Buffers saved(MDFN_PixelFormat::ABGR32_8888,100);
 saved.fields[0]=saved.fields[1]=-1; // Completed image field is independently owned.
 auto staged=EmucapDriverFrames::capture(saved.views,1);
 auto original=EmucapDriverFrames::capture(EmucapDriverViews(),0).encode();
 auto saved_bytes=staged.encode();
 EmucapCompletedFrame image;
 image.capture(saved.a,saved.rect[0],saved.widths[0],1);
 const auto image_bytes=image.encode();
 VTReady=1;VTSSnapshot=true;
 std::thread consumer;
 std::atomic<bool> consumed{false};
 {
  EmucapDriverVideo guard;
  assert(guard.capture().encode()==original && guard.can_restore(staged));
  consumer=std::thread([&]{
   MThreading::Mutex_Lock(VTMutex);
   EmucapBlitReadyFrame(VTReady.load());VTReady=-1;consumed=true;
   MThreading::Mutex_Unlock(VTMutex);
  });
  while(!MThreading::contended.load())std::this_thread::yield();
  assert(!consumed.load());
  SoftFB[1].field=2;assert(!guard.can_restore(staged));assert(!guard.restore(staged));
  SoftFB[1].field=-1;
  assert(guard.capture().encode()==original && staged.encode()==saved_bytes && VTReady==1);
  assert(guard.restore(staged));assert(!consumed.load() && VTReady==1);
  assert(guard.restore(staged));assert(guard.capture().encode()==original && VTReady==1);
  CurGame=nullptr;assert(!guard.publish(image) && image.encode()==image_bytes && VTReady==1);CurGame=&game;
  assert(guard.restore(staged));
  fail_after=0;assert(guard.publish(image));assert(fail_after==0);fail_after=-1;
  assert(!consumed.load() && VTReady==2 && !image.surface());
 }
 consumer.join();
 assert(consumed && displayed==EmucapPublishedFrame.surface());
 assert(displayed_field==1 && displayed_rotation==3 && displayed_snapshot);
 assert(EmucapPublishedFrame.encode()==image_bytes);
 {
  EmucapDriverVideo guard;
  // Consumed screenshot flags must not be resurrected.
  auto again=EmucapCompletedFrame::decode(image_bytes);
  assert(guard.publish(again));assert(!VTSSnapshot && VTReady==2);
  VTSSnapshot=true;
  EmucapCompletedFrame absent;
  assert(guard.publish(absent));assert(VTReady==-1 && pending_ssnapshot && !VTSSnapshot);
  auto next=EmucapCompletedFrame::decode(image_bytes);
  assert(guard.publish(next));assert(VTReady==2 && VTSSnapshot && !pending_ssnapshot);
  for(int slot:{0,1}) {EmucapBlitReadyFrame(slot);assert(displayed==SoftFB[slot].surface.get());}
  bool caught=false;try{EmucapBlitReadyFrame(3);}catch(const std::exception&){caught=true;}assert(caught);
 }
 auto prior=EmucapDriverFrames::capture(EmucapDriverViews(),0).encode();
 for(bool missing:{false,true}) {
  MThreading::fail_lock=!missing;VTMutex=missing?nullptr:&mutex;
  bool caught=false;try{EmucapDriverVideo guard;}catch(const std::exception&){caught=true;}
  assert(caught && VTReady==2 && EmucapDriverFrames::capture(EmucapDriverViews(),0).encode()==prior);
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-driver-video-') as temp:
    cpp = Path(temp) / 'test.cpp'
    exe = Path(temp) / 'test'
    cpp.write_text(code)
    libs = ['-liconv', '-framework', 'CoreFoundation'] if sys.platform == 'darwin' else []
    subprocess.run(['clang++', '-std=c++11', '-DHAVE_CONFIG_H', '-O1', '-pthread',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I'+str(source/'include'), '-I'+str(source/'intl'), '-I'+str(adapter), str(cpp),
        *[str(source/'src'/n) for n in ['video/surface.cpp','video/convert.cpp','error.cpp']],
        str(source/'src/libtrio.a'), str(source/'intl/libintl.a'), *libs, '-o',str(exe)], check=True)
    env = dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1')
    subprocess.run([str(exe)], check=True, env=env)
    assert subprocess.run([str(exe), 'unlock-failure'], env=env).returncode == 77
print('Driver display transaction: competing reader exclusion, reversible raster, publication, '
      'screenshot ownership, admission and terminal unlock failure pass ASan/UBSan')
