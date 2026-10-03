#!/usr/bin/env python3
"""Exercise Mesen pause/lock release owners with the real limiter and controlled time."""
import argparse
from pathlib import Path
import subprocess
import tempfile
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,required=True,help='patched Core/Shared directory')
a=p.parse_args();source=(a.source/'Emulator.cpp').read_text()
def body(sig):
 start=source.index(sig);i=source.index('{',start)+1;depth=1
 while depth:
  depth+=(source[i]=='{')-(source[i]=='}');i+=1
 return source[start:i].replace('std::this_thread::sleep_for','fake_sleep')
header=(a.source/'FrameLimiter.h').read_text().replace('#pragma once','').replace('#include "Utilities/Timer.h"','')
code=r'''
#include <memory>
#include <chrono>
#include <cassert>
#include <cmath>
#include <cstdio>
using std::shared_ptr;
static double now=0, waited=0, park=0;
static int resets=0;
class Timer {double origin=0;public:double GetElapsedMS(){return now-origin;}void Reset(){origin=now;++resets;}void WaitUntil(double t){double d=t-GetElapsedMS();if(d>0){now+=d;waited+=d;}}};
'''+header+r'''
enum class ConsoleNotificationType {GamePaused,GameResumed};
struct Notify {void SendNotification(ConsoleNotificationType){}};
struct Rewind {bool IsRewinding(){return false;}};
struct Actions {bool IsResetPending(){return false;}};
struct Debugger {bool HasBreakRequest(){return false;}};
struct Weak {shared_ptr<Debugger> lock(){return {};}explicit operator bool(){return false;}};
struct Lock {void Release(){now+=park;}void Acquire(){now+=5;}bool TryAcquire(int){Acquire();return true;}};
struct Counter {int reads=0;operator int(){return reads-->0?1:0;}};
namespace PlatformUtilities {void EnableScreensaver(){}void RestoreTimerResolution(){}void DisableScreensaver(){}void EnableHighResolutionTimer(){}}
struct Emulator {
 FrameLimiter *_frameLimiter;Notify notify;Rewind rewind;Actions actions;
 Notify *_notificationManager=&notify;Rewind *_rewindManager=&rewind;Actions *_systemActionManager=&actions;
 Lock _runLock;Weak _debugger;Counter _lockCounter;
 bool _paused=false,_stopFlag=false,_threadPaused=false;
 void OnBeforePause(bool){} void OnAfterPause();void WaitForPauseEnd();void WaitForLock();
};
static Emulator *active;
void fake_sleep(std::chrono::duration<int,std::milli>){now+=30;active->_paused=false;}
'''+ '\n'.join(body(sig) for sig in ['void Emulator::OnAfterPause()','void Emulator::WaitForPauseEnd()','void Emulator::WaitForLock()'])+r'''
int main(){
 for(int rate:{1,100,1000})for(double duration:{1.,50.,10000.})for(bool locked:{false,true}){
  now=0;park=duration;double delay=1000./60*100/rate;FrameLimiter limiter(delay);Emulator e;e._frameLimiter=&limiter;active=&e;
  for(int i=0;i<3;++i){limiter.ProcessFrame();while(limiter.WaitForNextFrame()){}}
  resets=0;
  if(locked){e._lockCounter.reads=2;e.WaitForLock();assert(!e._threadPaused);}
  else {e._paused=true;e.WaitForPauseEnd();}
  assert(resets==1);now+=delay/4;waited=0;limiter.ProcessFrame();while(limiter.WaitForNextFrame()){}
  assert(std::abs(waited-delay*.75)<1e-7);
  // No acquired lock means no anchor change.
  resets=0;e._lockCounter.reads=0;e.WaitForLock();assert(resets==0);
  // Shutdown must not advertise a resume by reanchoring.
  e._stopFlag=true;e._lockCounter.reads=2;e.WaitForLock();e.WaitForPauseEnd();assert(resets==0);
 }
 Emulator absent;absent._frameLimiter=nullptr;absent.OnAfterPause();
 puts("Mesen pause/lock owners: release-time anchor, active time, no-op and shutdown passed");
}
'''
with tempfile.TemporaryDirectory() as t:
 f=Path(t)/'probe.cpp';f.write_text(code)
 subprocess.run(['c++','-std=c++17','-fsanitize=address,undefined',str(f),'-o',t+'/probe'],check=True)
 subprocess.run([t+'/probe'],check=True)
