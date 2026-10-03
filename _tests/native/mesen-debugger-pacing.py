#!/usr/bin/env python3
"""Check the actual Mesen debugger park owner and limiter with controlled callbacks."""
import argparse
from pathlib import Path
import subprocess
import tempfile
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,required=True,help='patched Core directory')
a=p.parse_args()
def body(path,sig):
 s=path.read_text();start=s.index(sig);i=s.index('{',start)+1;depth=1
 while depth:
  depth+=(s[i]=='{')-(s[i]=='}');i+=1
 return s[start:i].replace('std::this_thread::sleep_for','fake_sleep')
header=(a.source/'Shared/FrameLimiter.h').read_text().replace('#pragma once','').replace('#include "Utilities/Timer.h"','')
code=r'''
#include <cassert>
#include <cmath>
#include <chrono>
#include <cstdio>
#include <initializer_list>
static double now,waited,park;static int resets;
class Timer {double origin=0;public:double GetElapsedMS(){return now-origin;}void Reset(){origin=now;++resets;}void WaitUntil(double t){double d=t-GetElapsedMS();if(d>0){now+=d;waited+=d;}}};
'''+header+r'''
enum class CpuType {Main,Other};enum class BreakSource {Unspecified,Pause,CpuStep};
enum class MemoryOperationType {ExecOpCode};
struct MemoryOperationInfo {MemoryOperationType Type=MemoryOperationType::ExecOpCode;};
struct BreakEvent {CpuType SourceCpu;int BreakpointId;BreakSource Source;MemoryOperationInfo Operation;};
enum class EventType {CodeBreak,CodeBreakIdleSavestate,CodeBreakIdle};
enum class ConsoleNotificationType {CodeBreak,DebuggerResumed};
struct Notify {void SendNotification(ConsoleNotificationType,void* =nullptr){}};
struct Emulator {FrameLimiter *_frameLimiter;Notify notify;void OnBeforePause(bool){}void OnAfterPause();Notify* GetNotificationManager(){return &notify;}};
struct CpuDebug {bool AllowChangeProgramCounter=true,IgnoreBreakpoints=false;void OnBeforeBreak(CpuType){}void DrawPartialFrame(){}};
struct Entry {CpuDebug* Debugger;};
struct Config {bool SingleBreakpointPerInstruction=false,DrawPartialFrame=false;};
struct Settings {Config config;Config& GetDebugConfig(){return config;}};
namespace PlatformUtilities {void EnableScreensaver(){}void DisableScreensaver(){}}
struct Debugger {
 int _suspendRequestCount=0,_breakRequestCount=0;
 bool _executionStopped=false,_waitForBreakResume=false,forbidden=false,immediate=false;
 CpuType _mainCpuType=CpuType::Main;Emulator *_emu;CpuDebug cpu;Entry _debuggers[2]{{&cpu},{&cpu}};Settings settings;Settings *_settings=&settings;
 CpuDebug* GetMainDebugger(){return &cpu;}
 bool IsBreakpointForbidden(BreakSource,CpuType,MemoryOperationInfo*){return forbidden;}
 void ClearPendingBreakExceptions(){}
 void ProcessEvent(EventType type,CpuType){if(type!=EventType::CodeBreak || immediate){now+=park;_waitForBreakResume=false;}}
 void SleepUntilResume(CpuType,BreakSource,MemoryOperationInfo*,int);
};
static Debugger* active;
void fake_sleep(std::chrono::duration<int,std::milli>){now+=park;active->_breakRequestCount=0;active->_waitForBreakResume=false;}
'''+body(a.source/'Shared/Emulator.cpp','void Emulator::OnAfterPause()')+'\n'+body(a.source/'Debugger/Debugger.cpp','void Debugger::SleepUntilResume(')+r'''
int main(){
 for(int rate:{1,100,1000})for(double duration:{1.,50.,10000.})for(int mode:{0,1,2}){
  now=0;park=duration;double delay=1000./60*100/rate;FrameLimiter limiter(delay);Emulator e{&limiter};Debugger d;d._emu=&e;active=&d;
  for(int i=0;i<3;++i){limiter.ProcessFrame();while(limiter.WaitForNextFrame()){}}
  resets=0;d.immediate=mode==1;d._breakRequestCount=mode==2?1:0;MemoryOperationInfo op;
  d.SleepUntilResume(CpuType::Main,mode==2?BreakSource::Unspecified:BreakSource::Pause,&op,0);
  assert(resets==1 && !d._executionStopped);now+=delay/4;waited=0;
  limiter.ProcessFrame();while(limiter.WaitForNextFrame()){}assert(std::abs(waited-delay*.75)<1e-7);
 }
 for(int mode:{0,1,2,3,4}){
  FrameLimiter limiter(16);Emulator e{&limiter};Debugger d;d._emu=&e;active=&d;resets=0;
  d._suspendRequestCount=mode==0;d._executionStopped=mode==1;d._breakRequestCount=(mode==2||mode==3)?1:0;d.cpu.AllowChangeProgramCounter=mode!=3;d.forbidden=mode==4;
  d.SleepUntilResume(mode==2?CpuType::Other:CpuType::Main,BreakSource::Pause,nullptr,0);assert(resets==0);
 }
 puts("Mesen debugger: idle, immediate callback, internal lock and rejected stops passed");
}
'''
with tempfile.TemporaryDirectory() as t:
 f=Path(t)/'probe.cpp';f.write_text(code)
 subprocess.run(['c++','-std=c++17','-fsanitize=address,undefined',str(f),'-o',t+'/probe'],check=True)
 subprocess.run([t+'/probe'],check=True)
