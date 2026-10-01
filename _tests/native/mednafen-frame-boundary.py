#!/usr/bin/env python3
"""Exercise the actual driver GameLoop with instrumented emulator/output services."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen/src/drivers/main.cpp'))
parser.add_argument('--baseline', action='store_true', help='expect old boundary/startup/render counterexamples')
args = parser.parse_args()
source = args.source.read_text()
start = source.index('static int GameLoop(void *arg)\n{')
body = source[start:source.index('\nstd::string GetBaseDirectory', start)]
prefix = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <algorithm>
#include <memory>
#include <string>
using uint8 = uint8_t; using int16 = int16_t; using int32 = int32_t;
using uint32 = uint32_t; using uint64 = uint64_t;
#define MDFN_UNLIKELY(x) (x)
#define MDFN_MASTERCLOCK_FIXED(x) (x)
const int MDFNSS_FUZZ_RANDOM = 1;
struct Rect { int32 x=0,y=0,w=0,h=0; };
struct Surface {};
struct FrameBuffer {
  std::unique_ptr<Surface> surface{new Surface};
  Rect rect;
  std::unique_ptr<int32[]> lw{new int32[2]()};
  int field=-1;
} SoftFB[2];
bool SoftFB_BackBuffer=false;
struct EmulateSpecStruct {
  Surface* surface=nullptr;
  Rect DisplayRect;
  int32* LineWidths=nullptr;
  bool skip=false,NeedRewind=false,InterlaceOn=false,InterlaceField=false;
  double soundmultiplier=0,SoundRate=0,SoundVolume=0;
  int16* SoundBuf=nullptr;
  int32 SoundBufMaxSize=0,SoundBufSize=0,SoundBufSize_DriverProcessed=0;
  uint32 MasterCycles=0,MasterCycles_DriverProcessed=0;
};
struct Game { int soundchan=2; uint64 MasterClock=60000; } game;
Game* CurGame=&game;
bool GameThreadRun=true,NeedVideoSync=false,MDFNDnetplay=false;
bool pending_ssnapshot=false,pending_snapshot=false,pending_save_state=false,pending_save_movie=false;
bool NeedFrameAdvance=false,DNeedRewind=false,StateFuzzTest=false,StateRCTest=false,StateSLSTest=false;
bool InFrameAdvance=false,GameLoopPaused=false;
unsigned NoWaiting=0;
double CurGameSpeed=1,ratio=1;
std::string scenario;
unsigned emulated=0,captured=0,accounted=0,submitted=0,serviced=0;
bool replaced=false;
struct Syncher {
  void SetETtoRT() {}
  bool NeedFrameSkip() { return false; }
  void AddEmuTime(uint32) { assert(!replaced); ++accounted; }
} ers;
namespace Time { void SleepMS(int) { abort(); } }
bool Sound_NeedReInit() { return false; }
void GT_ReinitSound() {}
bool MDFN_GetSettingB(const char*) { return true; }
unsigned MDFN_GetSettingUI(const char*) { return 100; }
int Sound_GetRate() { return 48000; }
int16* Sound_GetEmuModBuffer(int32* size) { static int16 samples[8]={}; *size=8; return samples; }
double emucap_host_audio_ratio() { return ratio; }
void emucap_pre_first_frame() {
  if (!emulated && scenario=="startup_policy") ratio=0.25;
}
bool emucap_frame_consumer_active() { return scenario!="no_consumer"; }
void MDFNI_Emulate(EmulateSpecStruct* spec) {
  if (scenario=="startup_policy") assert(spec->soundmultiplier==0.25);
  if (scenario=="render_consumer") assert(!spec->skip);
  if (scenario=="no_consumer") assert(spec->skip);
  ++emulated;
  spec->DisplayRect.w=100+emulated;
  spec->DisplayRect.h=2;
  spec->InterlaceOn=true;
  spec->InterlaceField=true;
  spec->SoundBufSize=2;
  spec->MasterCycles=60;
}
void emucap_capture(const void*,const void* rect,const void*,int field=1) {
  assert(field==1);
  assert(static_cast<const Rect*>(rect)->w==int(100+emulated));
  ++captured;
}
bool MDFND_Update(int index,int16*,int32 size) {
  assert(!replaced && size==2 && accounted==emulated);
  if (index>=0) {
    assert(SoftFB[index].rect.w==int(100+emulated));
    assert(SoftFB[index].field==1);
  }
  ++submitted;
  return index>=0;
}
void FPS_IncVirtual(uint32) {}
void FPS_IncDrawn() {}
void FPS_UpdateCalc() {}
void Netplay_GT_CheckPendingLine() {}
void emucap_service(uint64 frame) {
  ++serviced;
  assert(frame==emulated && serviced==emulated);
  if (scenario=="frame_boundary") {
    assert(captured==emulated && submitted==emulated && accounted==emulated);
    assert(SoftFB_BackBuffer);
    // Model a successful load replacing raster metadata and buffer roles.
    // No destination-frame output may follow this serviced boundary.
    replaced=true;
    SoftFB_BackBuffer=false;
    for (auto& buffer:SoftFB) { buffer.rect.w=777; buffer.field=0; }
  }
  if (scenario!="count" || frame==3) GameThreadRun=false;
}
// Upstream diagnostic branches remain present in the actual GameLoop but inactive.
struct MemoryStream {
  MemoryStream(unsigned) {}
  void rewind() {}
  uint8* map() { static uint8 data[32]={}; return data; }
  unsigned map_size() { return 32; }
};
struct FileStream {
  enum { MODE_WRITE=1 };
  FileStream(const char*,int) {}
  void write(const void*,unsigned) {}
  void close() {}
};
void MDFNSS_SaveSM(MemoryStream*) {}
void MDFNSS_LoadSM(MemoryStream*,bool=false,int=0) {}
'''
suffix = r'''
int main(int argc,char** argv) {
  assert(argc==2);
  scenario=argv[1];
  if (scenario=="render_consumer" || scenario=="no_consumer") NoWaiting=1;
  assert(GameLoop(nullptr)==1);
  assert(emulated==(scenario=="count" ? 3U : 1U));
  assert(captured==emulated && submitted==emulated && serviced==emulated);
  if (scenario=="frame_boundary") {
    assert(!SoftFB_BackBuffer && SoftFB[0].rect.w==777 && SoftFB[1].rect.w==777);
    assert(SoftFB[0].field==0 && SoftFB[1].field==0);
  }
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-frame-boundary-') as temp:
    cpp = Path(temp) / 'test.cpp'
    executable = Path(temp) / 'test'
    cpp.write_text(prefix + body + suffix)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    '-fno-omit-frame-pointer', str(cpp), '-o', str(executable)], check=True)
    for scenario in ['frame_boundary', 'startup_policy', 'render_consumer', 'no_consumer', 'count']:
        result = subprocess.run([str(executable), scenario], stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True,
                                env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1'))
        expect_failure = args.baseline and scenario in ['frame_boundary', 'startup_policy', 'render_consumer']
        assert (result.returncode != 0) == expect_failure, (scenario, result.returncode, result.stdout)
        print(scenario, 'counterexample reproduced' if expect_failure else 'passed')
