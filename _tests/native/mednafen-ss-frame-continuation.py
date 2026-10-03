#!/usr/bin/env python3
"""Exercise native Saturn frame entry and master/slave dispatch continuation."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source',type=Path,default=Path('adapters/mednafen/work/mednafen'))
args=parser.parse_args()
source=(args.source/'src/ss/ss.cpp').read_text()
start=source.index(' if(!FrameActive)',source.index('static void Emulate('))
entry=source[start:source.index(' SOUND_StartFrame(',start)]
start=source.index('template<bool EmulateICache, bool DebugMode>\nstatic INLINE int32 RunLoop_INLINE')
loop=source[start:source.index('\ntemplate<bool EmulateICache>\nstatic NO_INLINE',start)]
code=r'''
#include <cassert>
#include <cstdint>
using uint32=uint32_t; using int32=int32_t; using sscpu_timestamp_t=int32;
#define INLINE
#define MDFN_LIKELY(v) (v)
struct EmulateSpecStruct {} spec;
static EmulateSpecStruct* espec=&spec;
static bool FrameActive,FrameDebugMode,ResumeDispatch,AllowMidSync,Running;
static uint32 DispatchCPU;
static int32 SchedulerTime,SH7095_mem_timestamp,next_event_ts,cur_clock_div;
static int begins,inputs,starts,bindings,slave_changes,forced,events,steps[2],hooks[2];
static int interrupt_cpu=-1;
struct RestoredDispatch {};
static bool DBG_NeedCPUHooks() {return true;}
static int SMPC_StartFrame(EmulateSpecStruct*) {++begins; return 61;}
static void UpdateSMPCInput(int) {++inputs;}
namespace VDP2 {
 void StartFrame(EmulateSpecStruct*,bool) {++starts;}
 void ResumeFrame(EmulateSpecStruct*) {++bindings;}
}
static void SMPC_ProcessSlaveOffOn() {++slave_changes;}
static void ForceEventUpdates(int) {++forced;}
static bool EventHandler(int) {++events; Running=false; return false;}
static void DBG_SetEffTS(int ts) {assert(ts==SchedulerTime);}
template<unsigned which> void DBG_CPUHandler() {
 ++hooks[which]; assert(DispatchCPU==which);
 if(interrupt_cpu==(int)which) throw RestoredDispatch{};
}
struct Core {
 int timestamp=0;
 void SetDebugMode(bool) {}
 template<unsigned which,bool,bool> void Step() {++steps[which]; timestamp+=2;}
 void DMA_BusTimingKludge() {}
 void RunSlaveUntil(int bound) {while(timestamp<bound) Step<1,true,false>();}
 void RunSlaveUntil_Debug(int bound) {
  while(timestamp<bound) {DBG_CPUHandler<1>(); Step<1,true,true>();}
 }
} CPU[2];
void enter_frame() {
''' + entry + '\n}\n' + loop + r'''
static void clear() {
 FrameActive=FrameDebugMode=ResumeDispatch=AllowMidSync=Running=false;
 DispatchCPU=0; SchedulerTime=SH7095_mem_timestamp=cur_clock_div=0; next_event_ts=1;
 begins=inputs=starts=bindings=slave_changes=forced=events=0;
 steps[0]=steps[1]=hooks[0]=hooks[1]=0;
 CPU[0].timestamp=CPU[1].timestamp=0; interrupt_cpu=-1;
}
int main() {
 clear(); enter_frame(); enter_frame();
 assert(begins==1 && inputs==1 && starts==1 && bindings==1 && FrameActive && AllowMidSync && FrameDebugMode);
 clear(); FrameActive=true; DispatchCPU=1; SchedulerTime=7; AllowMidSync=false; cur_clock_div=65;
 enter_frame(); assert(begins==0 && inputs==0 && starts==0 && bindings==1);
 assert(DispatchCPU==1 && SchedulerTime==7 && !AllowMidSync && cur_clock_div==65 && !FrameDebugMode);
 clear(); RunLoop_INLINE<false,true>(&spec);
 assert(slave_changes==1 && forced==1 && steps[0]==1 && steps[1]==1);
 for(bool cached : {false,true}) {
  clear(); ResumeDispatch=true; Running=true; DispatchCPU=1;
  CPU[0].timestamp=6; CPU[1].timestamp=2; SchedulerTime=4;
  if(cached) RunLoop_INLINE<true,true>(&spec); else RunLoop_INLINE<false,true>(&spec);
  assert(slave_changes==0 && forced==0 && steps[0]==0 && steps[1]==2);
  clear(); ResumeDispatch=true; Running=true; DispatchCPU=1; interrupt_cpu=1;
  CPU[0].timestamp=6; CPU[1].timestamp=2;
  try {
   if(cached) RunLoop_INLINE<true,true>(&spec); else RunLoop_INLINE<false,true>(&spec);
   assert(false);
  } catch(const RestoredDispatch&) {}
  assert(steps[0]==0 && steps[1]==0 && events==0 && forced==0);
 }
 clear(); ResumeDispatch=true; Running=true; DispatchCPU=0; interrupt_cpu=0;
 try {RunLoop_INLINE<false,true>(&spec); assert(false);} catch(const RestoredDispatch&) {}
 assert(steps[0]==0 && steps[1]==0 && forced==0);
}
'''
# Range-for initializer-list portability for the tiny seam executable.
code='#include <initializer_list>\n'+code
with tempfile.TemporaryDirectory(prefix='mednafen-ss-continuation-') as tmp:
    cpp=Path(tmp)/'test.cpp';binary=Path(tmp)/'test';cpp.write_text(code)
    subprocess.run(['clang++','-std=c++11','-O1','-fsanitize=address,undefined',str(cpp),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)
print('Native Saturn continuation: frame start/rebind, master/slave dispatch and both slave unwind paths pass ASan/UBSan')

debug=(args.source/'src/ss/debug.inc').read_text()
start=debug.index(' if(SkipRestoredCPUHook',debug.index('static MDFN_COLD void DBG_CPUHandler'))
gate=debug[start:debug.index('#if 0',start)]
code=r'''
#include <cassert>
#include <cstdint>
#include <initializer_list>
using uint32=uint32_t;
static bool SkipRestoredCPUHook;
static unsigned DispatchCPU, callbacks;
struct Core { uint32 Pipe_ID; } CPU[2];
template<unsigned which> void enter() {
''' + gate + r'''
 ++callbacks;
}
int main() {
 DispatchCPU=0; callbacks=0; CPU[0].Pipe_ID=0;
 SkipRestoredCPUHook=true; enter<0>();
 assert(!SkipRestoredCPUHook && callbacks==0);
 enter<0>(); assert(callbacks==1);
 for(unsigned op : {0x7e,0xfe,0x7f,0xff}) {
  CPU[0].Pipe_ID=op<<24; enter<0>(); assert(callbacks==1);
 }
 DispatchCPU=1; SkipRestoredCPUHook=true; CPU[1].Pipe_ID=0;
 enter<1>(); assert(!SkipRestoredCPUHook && callbacks==1);
 enter<1>(); assert(callbacks==2);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-ss-callback-') as tmp:
    cpp=Path(tmp)/'test.cpp';binary=Path(tmp)/'test';cpp.write_text(code)
    subprocess.run(['clang++','-std=c++11','-O1','-fsanitize=address,undefined',str(cpp),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)
print('Native Saturn callback gate: one restored entry and four pseudo dispatch classes pass ASan/UBSan')
