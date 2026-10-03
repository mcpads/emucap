#!/usr/bin/env python3
"""Exercise the native NDS limiter and shared CPU resume with a controlled host clock."""
import argparse
import pathlib
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=pathlib.Path, required=True, help='patched gdbstub.cpp')
args = parser.parse_args()
p=args.source.read_text()
def body(sig):
 a=p.index(sig);b=p.index('{',a);depth=1;i=b+1
 while depth:
  depth+=(p[i]=='{')-(p[i]=='}');i+=1
 return p[a:i]
pace=p[p.index('static EmucapPacingOwner emucap_pacing'):p.index('// Requests the stub answers while the guest runs:')]
pace=pace.replace('std::chrono::steady_clock','Clock').replace('std::this_thread::sleep_for','sleep_for')
code=r'''
#include <chrono>
#include <atomic>
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <cstdio>
#include "EmucapPacing.h"
struct Clock {using duration=std::chrono::nanoseconds;using time_point=std::chrono::time_point<Clock>;static time_point now(){return current;}static time_point current;};
Clock::time_point Clock::current{};
static int64_t waited=0;
void sleep_for(Clock::duration d){Clock::current+=d;waited+=d.count();}
struct Ctrl {void *data=nullptr;void unstall(void*){}};
struct gdb_stub_state {enum {RUNNING_EMU_GDB_STATE,START_RUN_GDB_STATE};int emu_stub_state=0,ctl_stub_state=0,main_stop_flag=0;Ctrl *cpu_ctrl;};
gdb_stub_state* emucap_gdb_stubs[2];
void NDS_debug_continue(){}
struct Cpu {bool stalled=false;};
Cpu NDS_ARM9,NDS_ARM7;
'''+pace+body('static void emucap_start_gdb_stubs()')+r'''
int main(){
 Ctrl ctrl;gdb_stub_state one,two;one.cpu_ctrl=&ctrl;two.cpu_ctrl=&ctrl;
 emucap_gdb_stubs[0]=&one;emucap_gdb_stubs[1]=&two;
 // A requested stall alone is insufficient; both cores must be parked.
 for(bool parked:{false,true})for(bool arm9:{false,true})for(bool arm7:{false,true}){
  emucap_gdbstub_scheduler_parked(parked);NDS_ARM9.stalled=arm9;NDS_ARM7.stalled=arm7;
  assert(emucap_shared_halt_verified()==(parked && arm9 && arm7));
 }
 emucap_gdbstub_scheduler_parked(true);NDS_ARM9.stalled=NDS_ARM7.stalled=true;
 emucap_gdb_stubs[1]=nullptr;assert(!emucap_shared_halt_verified());emucap_gdb_stubs[1]=&two;
 assert(emucap_shared_halt_verified());emucap_start_gdb_stubs();assert(!emucap_shared_halt_verified());
 for(int rate:{1,100,400,10000})for(int ms:{1,50,10000})for(int cpu:{0,1}){
  emucap_pacing.Apply(rate);emucap_pace_wake=true;emucap_pace_frames(1,0);
  for(int i=0;i<10;++i)emucap_pace_frames(1,0);
  emucap_gdb_stubs[cpu]->main_stop_flag=1;Clock::current+=std::chrono::milliseconds(ms);emucap_start_gdb_stubs();
  auto revision=emucap_pacing.Read().revision;auto frame=emucap_vblank_clock.load();
  waited=0;emucap_pace_frames(1,0);
  assert(emucap_pacing.Read().percent==rate && emucap_pacing.Read().revision==revision && emucap_vblank_clock==frame);
  int64_t expected=560190LL*1000000000LL/33513982LL*100/rate;
  if(waited!=expected){fprintf(stderr,"rate=%d park_ms=%d waited=%lld expected=%lld\n",rate,ms,(long long)waited,(long long)expected);return 1;}
  // A redundant resume must preserve the pending deadline and elapsed active time.
  auto elapsed=std::chrono::nanoseconds(std::min<int64_t>(1000000,expected/4));Clock::current+=elapsed;
  emucap_start_gdb_stubs();waited=0;emucap_pace_frames(1,0);assert(waited==expected-elapsed.count());
 }
 emucap_pacing.Apply(0);one.main_stop_flag=1;emucap_start_gdb_stubs();
 waited=0;emucap_pace_frames(1,0);assert(waited==0);
 puts("NDS resume: full fresh interval, unchanged policy and no-op deadline preservation passed");
}
'''
with tempfile.TemporaryDirectory() as d:
 (pathlib.Path(d)/'EmucapPacing.h').write_text((args.source.parent/'EmucapPacing.h').read_text())
 f=pathlib.Path(d)/'probe.cpp';f.write_text(code)
 subprocess.run(['c++','-std=c++17','-fsanitize=address,undefined',str(f),'-o',d+'/probe'],check=True)
 subprocess.run([d+'/probe'],check=True)
