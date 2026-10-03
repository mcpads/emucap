#!/usr/bin/env python3
"""Run PCSX2 state transitions and limiter with controlled host ticks."""
import argparse
from pathlib import Path
import subprocess
import tempfile
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, required=True, help='patched VMManager.cpp')
a = p.parse_args()
s = a.source.read_text()
guard = (a.source.parent.parent / 'common/ScopedGuard.h').read_text()
guard = guard.replace('#pragma once', '').replace('#include "Pcsx2Defs.h"', '#define __fi inline')
def body(signature):
    start = s.index(signature)
    brace = s.index('{', start)
    i, depth = brace + 1, 1
    while depth:
        depth += (s[i] == '{') - (s[i] == '}')
        i += 1
    return s[start:i]
code = guard + r'''
#include <algorithm>
#include <atomic>
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <functional>
#include <stdexcept>
#include <utility>
using u64=uint64_t; using s64=int64_t;
static u64 ticks=10000000, s_limiter_frame_start;
static s64 s_limiter_ticks_per_frame;
static float s_target_speed=1;
static bool s_use_vsync_for_timing=false, s_limiter_restarted=false;
static unsigned s_emucap_memory_epoch;
static int interrupt=0, pumps=0;
static unsigned s_emucap_execution_depth=0, s_emucap_state_transition_depth=0;
static bool s_cpu_implementation_changed=false;
static std::function<void()> cpu_action, pause_action, switch_action, exit_action;
void UpdateCPUImplementations(){if(switch_action)switch_action();}
void vtlb_ResetFastmem(){}
u64 GetCPUTicks(){return ticks++;} // One microsecond per read also advances the native spin loop.
u64 GetTickFrequency(){return 1000000;}
enum class VMState {Initializing,Shutdown,Running,Paused,Stopping};
std::atomic<VMState> s_state{VMState::Paused};
#define pxAssert(x) assert(x)
#define THREAD_VU1 false
struct {void WaitVU(){}} vu1Thread;
struct {void ExitExecution(){if(exit_action)exit_action();} void Execute(){if(cpu_action)cpu_action();}} cpu;
auto Cpu=&cpu;
struct {bool InhibitScreensaver=false;} EmuConfig;
void SetTimerResolutionIncreased(bool){}
void UpdateInhibitScreensaver(bool){}
void AccumulateSessionPlaytime(){}
void ResetResumeTimestamp(){}
namespace MTGS {void WaitGS(bool){auto action=pause_action;if(action)action();}}
namespace InputManager {void PauseVibration(){}}
namespace PerformanceMetrics {void Reset(){}}
namespace SPU2 {void SetOutputPaused(bool){}}
namespace Achievements {void OnVMPaused(bool){}}
namespace Threading {void Sleep(int ms){assert(ms<=10);ticks+=ms*1000;}}
namespace Host {
 void OnVMPaused(){} void OnVMResumed(){}
 void PumpMessagesOnCPUThread(){++pumps;if(interrupt==1)s_state=VMState::Paused;if(interrupt==2)s_limiter_restarted=true;}
}
namespace PINEServer {void ServiceEmuCapFrameOnCPUThread(){}}
namespace VMManager {
 VMState GetState(){return s_state.load();}
 bool IsEmucapExecutionParkedOnCPUThread();
 void Execute(); void ResetFrameLimiter(); void SetState(VMState);
 namespace Internal {void ClearCPUExecutionCaches(){} void Throttle();bool IsExecutionInterrupted(){return s_state!=VMState::Running;}}
}
''' + '\n'.join(body(x) for x in ['void VMManager::ResetFrameLimiter()\n{','bool VMManager::IsEmucapExecutionParkedOnCPUThread()', 'void VMManager::SetState(VMState state)', 'void VMManager::Execute()', 'void VMManager::Internal::Throttle()']) + r'''
int main(){
 using VMManager::IsEmucapExecutionParkedOnCPUThread;
 // A paused state flag is visible while GS still drains. It is not a terminal.
 s_state=VMState::Running;
 int drains=0;
 pause_action=[&]{
  ++drains;
  assert(VMManager::GetState()==VMState::Paused);
  assert(!IsEmucapExecutionParkedOnCPUThread());
 };
 VMManager::SetState(VMState::Paused);
 assert(drains==1 && IsEmucapExecutionParkedOnCPUThread());
 // A callback pumped inside guest execution must remain pending after pause.
 cpu_action=[&]{
  VMManager::SetState(VMState::Paused);
  assert(!IsEmucapExecutionParkedOnCPUThread());
 };
 VMManager::SetState(VMState::Running);
 VMManager::Execute();
 assert(drains==2 && IsEmucapExecutionParkedOnCPUThread());
 // A stale Qt Running branch cannot execute after its event dispatch paused.
 int stale_entries=0;
 cpu_action=[&]{++stale_entries;};
 VMManager::Execute();assert(stale_entries==0 && IsEmucapExecutionParkedOnCPUThread());
 // Include setup in the active scope, and recheck pause before entering the CPU.
 switch_action=[]{VMManager::SetState(VMState::Paused);assert(!IsEmucapExecutionParkedOnCPUThread());};
 s_cpu_implementation_changed=true;s_state=VMState::Running;
 VMManager::Execute();
 assert(stale_entries==0 && IsEmucapExecutionParkedOnCPUThread());
 // Nested transition completion cannot release an outer transition.
 pause_action=[&]{
  pause_action=nullptr;
  VMManager::SetState(VMState::Paused);
  assert(!IsEmucapExecutionParkedOnCPUThread());
 };
 VMManager::SetState(VMState::Paused);
 assert(IsEmucapExecutionParkedOnCPUThread());
 // C++ unwinding releases the execution scope; this does not assert VM health.
 cpu_action=[]{VMManager::SetState(VMState::Paused);throw std::runtime_error("injected CPU failure");};
 s_state=VMState::Running;
 try {VMManager::Execute();assert(false);} catch(const std::runtime_error&){}
 assert(IsEmucapExecutionParkedOnCPUThread());
 // Stopping releases transition ownership before the CPU's non-local exit.
 exit_action=[]{assert(s_emucap_state_transition_depth==0);throw std::runtime_error("CPU exit");};
 cpu_action=[]{VMManager::SetState(VMState::Stopping);};
 s_state=VMState::Running;
 try {VMManager::Execute();assert(false);} catch(const std::runtime_error&){}
 assert(!IsEmucapExecutionParkedOnCPUThread());
 s_state=VMState::Paused;assert(IsEmucapExecutionParkedOnCPUThread());
 cpu_action=nullptr;exit_action=nullptr;
 for(VMState state:{VMState::Initializing,VMState::Shutdown,VMState::Running,VMState::Stopping}){
  s_state=state;assert(!IsEmucapExecutionParkedOnCPUThread());
 }
 for(int rate:{1,50,100,400,1000})for(u64 park:{1000,50000,10000000}){
  s_target_speed=rate/100.f;s_limiter_ticks_per_frame=100000000LL/(60*rate);
  s_state=VMState::Paused;ticks+=park;
  VMManager::SetState(VMState::Running);u64 anchor=s_limiter_frame_start;
  ticks+=s_limiter_ticks_per_frame/4;
  VMManager::Internal::Throttle();
  assert(s_limiter_frame_start==anchor+s_limiter_ticks_per_frame);
  assert(ticks>=s_limiter_frame_start && ticks<=s_limiter_frame_start+2);
  // A redundant running transition preserves the existing deadline.
  anchor=s_limiter_frame_start;ticks+=s_limiter_ticks_per_frame/4;
  VMManager::SetState(VMState::Running);assert(s_limiter_frame_start==anchor);
  VMManager::Internal::Throttle();assert(s_limiter_frame_start==anchor+s_limiter_ticks_per_frame);
 }
 for(int reason:{1,2}){
  s_state=VMState::Paused;VMManager::SetState(VMState::Running);
  s_limiter_ticks_per_frame=2000000;u64 begin=ticks;interrupt=reason;pumps=0;
  VMManager::Internal::Throttle();assert(pumps==1 && ticks-begin<=10010);
 }
 interrupt=0;s_state=VMState::Running;
 for(bool vsync:{false,true}){
  s_use_vsync_for_timing=vsync;s_target_speed=vsync?1:0;u64 begin=ticks;pumps=0;
  VMManager::Internal::Throttle();assert(ticks==begin && pumps==0);
 }
 puts("PS2 native resume/limiter: fresh interval, interruption and unwound pause boundary passed");
}
'''
with tempfile.TemporaryDirectory() as t:
    f=Path(t)/'probe.cpp';f.write_text(code)
    subprocess.run(['c++','-std=c++17','-fsanitize=address,undefined',str(f),'-o',t+'/probe'],check=True)
    subprocess.run([t+'/probe'],check=True)
