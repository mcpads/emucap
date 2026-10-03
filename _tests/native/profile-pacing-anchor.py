#!/usr/bin/env python3
"""Execute actual Dolphin/openMSX clock owners against controlled host and guest clocks."""
from pathlib import Path
import subprocess, tempfile
ROOT = Path(__file__).resolve().parents[2]

def function(path, signature):
    text = path.read_text()
    start = text.index(signature)
    opening = text.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]

common = r'''
#include <algorithm>
#include <atomic>
#include <cassert>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
using s64 = int64_t; using u32 = uint32_t;
'''
dolphin = ROOT/'adapters/dolphin/work/dolphin-src/Source/Core/Core/CoreTiming.cpp'
dolphin_code = common + r'''
struct Clock {
 using duration=std::chrono::nanoseconds;
 using time_point=std::chrono::time_point<Clock>;
 static time_point current;
 static time_point now(){return current;}
};
Clock::time_point Clock::current{};
using TimePoint=Clock::time_point; using DT=Clock::duration;
using DT_us=std::chrono::microseconds;
namespace Core { bool GetIsThrottlerTempDisabled(){return false;} }
namespace Common { template<class F> struct ScopeGuard { F f; ~ScopeGuard(){f();} }; template<class F> ScopeGuard(F)->ScopeGuard<F>; }
#define DEBUG_LOG_FMT(...) ((void)0)
struct Timers {s64 GetTicksPerSecond(){return 1000000;}};
struct Metrics {void CountPerformanceMarker(s64,s64){}};
struct System {Timers t; Metrics m; Timers& GetSystemTimers(){return t;} Metrics& GetPerfMetrics(){return m;}};
struct CoreTimingManager {
 System m_system;
 s64 m_throttle_reference_cycle=0, ticks=0;
 TimePoint m_throttle_reference_time{};
 s64 m_throttle_adj_clock_per_sec=1000000;
 double m_emulation_speed=1;
 bool m_config_rush_frame_presentation=false, m_correct_time_drift=false;
 bool m_throttle_disable_vi_int=false;
 std::atomic<bool> m_throttled_after_presentation{false};
 DT m_max_throttle_skip_time=std::chrono::milliseconds(1), m_max_fallback=std::chrono::milliseconds(50);
 TimePoint slept_until{};
 s64 GetTicks(){return ticks;}
 void SleepUntil(TimePoint target){slept_until=target; if(target>Clock::current)Clock::current=target;}
 void UpdateVISkip(TimePoint,TimePoint){}
 TimePoint CalculateTargetHostTimeInternal(s64);
 TimePoint GetTargetHostTime(s64);
 bool IsSpeedUnlimited() const;
 void Throttle(s64); void UpdateSpeedLimit(s64,double); void RestartThrottle(); void ResetThrottle(s64);
};
'''
for signature in ['TimePoint CoreTimingManager::CalculateTargetHostTimeInternal(', 'bool CoreTimingManager::IsSpeedUnlimited()', 'TimePoint CoreTimingManager::GetTargetHostTime(', 'void CoreTimingManager::Throttle(', 'void CoreTimingManager::UpdateSpeedLimit(', 'void CoreTimingManager::RestartThrottle()', 'void CoreTimingManager::ResetThrottle(']:
    dolphin_code += function(dolphin, signature)+'\n'
dolphin_code += r'''
int main(){
 for(int pause_ms:{1,50,10000}) {
  CoreTimingManager c; c.ticks=4000000; c.ResetThrottle(c.ticks);
  Clock::current+=std::chrono::milliseconds(pause_ms);
  c.RestartThrottle(); auto resumed=Clock::now();
  c.Throttle(c.ticks+16000);
  assert(c.slept_until==resumed+std::chrono::milliseconds(16));
  // Load moves guest time backwards. The production load caller resets at that cycle.
  c.ticks=100; Clock::current+=std::chrono::milliseconds(pause_ms);c.ResetThrottle(c.ticks);
  auto loaded=Clock::now();c.Throttle(c.ticks+16000);
  assert(c.slept_until==loaded+std::chrono::milliseconds(16));
 }
 for(double speed:{0.5,1.0,100.0}) {
  CoreTimingManager c;c.ResetThrottle(0);
  const s64 change=10000;auto due=c.GetTargetHostTime(change);
  c.UpdateSpeedLimit(change,speed);
  // Rate change preserves the deadline at the transition and changes only its slope.
  assert(c.GetTargetHostTime(change)==due);
  assert(c.GetTargetHostTime(change+1000000)-due==Clock::duration(s64(1000000000/speed)));
 }
 CoreTimingManager c;c.UpdateSpeedLimit(0,0);Clock::current+=std::chrono::seconds(10);c.Throttle(300);
 assert(c.m_throttle_reference_cycle==300 && c.m_throttle_reference_time==Clock::now());
 puts("Dolphin actual clock owner: pause, backward guest clock, rate transition and unlimited passed");
}
'''
msx = ROOT/'adapters/openmsx/work/openmsx-21.0/src/RealTime.cc'
msx_code = common+r'''
using EmuTime=double;
namespace Timer {static uint64_t now=1000000; uint64_t getTime(){return now;}}
template<class T,class U>T narrow(U v){return static_cast<T>(v);}
template<class T,class U>T narrow_cast(U v){return static_cast<T>(v);}
constexpr double SYNC_INTERVAL=0.08;
constexpr int64_t MAX_LAG=200000;
struct Setting{};struct SpeedManager{};struct ThrottleManager{};
struct RealTime {
 bool enabled=true; uint64_t idealRealTime=0;double sleepAdjust=0,emuTime=0,guest=0,speed=1;
 struct Throttle {bool isThrottled(){return true;}} throttleManager;
 struct Distributor {bool interrupt=false;unsigned requested=0;bool sleep(unsigned n){requested=n;Timer::now+=interrupt?1:n;return !interrupt;}} eventDistributor;
 struct Delay {void sync(EmuTime){}} eventDelay;
 EmuTime getCurrentTime(){return guest;}
 double getRealDuration(EmuTime a,EmuTime b){return (b-a)/speed;}
 double getEmuDuration(double d){return d*speed;}
 void removeSyncPoint(){} void setSyncPoint(double){}
 void internalSync(EmuTime,bool);void resync();
 void update(const Setting&) noexcept;void update(const SpeedManager&) noexcept;void update(const ThrottleManager&) noexcept;
};
'''
for signature in ['void RealTime::internalSync(', 'void RealTime::resync()', 'void RealTime::update(const Setting&', 'void RealTime::update(const SpeedManager&', 'void RealTime::update(const ThrottleManager&']:
    msx_code+=function(msx,signature)+'\n'
msx_code+=r'''
int main(){
 for(int pause_ms:{1,50,10000}) {
  RealTime r;r.guest=4;r.resync();Timer::now+=pause_ms*1000;r.update(Setting{});
  auto resumed=Timer::now;r.internalSync(r.guest+0.125,true);
  assert(Timer::now==resumed+125000);
  r.guest=1;Timer::now+=pause_ms*1000;r.resync();auto loaded=Timer::now;
  r.internalSync(r.guest+0.125,true);assert(Timer::now==loaded+125000);
 }
 for(double speed:{0.5,1.0,100.0}) {
  RealTime r;r.resync();r.internalSync(0.125,true);r.guest=0.125;r.speed=speed;
  r.update(SpeedManager{});auto changed=Timer::now;
  r.internalSync(0.25,true);assert(Timer::now==changed+uint64_t(125000/speed));
 }
 // A control event interrupts the sleep: remaining deadline is still owed.
 RealTime r;r.resync();r.eventDistributor.interrupt=true;auto start=Timer::now;
 r.internalSync(0.125,true);assert(Timer::now==start+1 && r.sleepAdjust==0);
 r.eventDistributor.interrupt=false;r.internalSync(0.125,true);assert(Timer::now==start+125000);
 puts("openMSX actual clock owner: pause, backward guest clock, rate transition and interrupted sleep passed");
}
'''
with tempfile.TemporaryDirectory(prefix='profile-pacing-anchor-') as work:
    for name,code in [('dolphin',dolphin_code),('openmsx',msx_code)]:
        path=Path(work)/f'{name}.cpp'; exe=Path(work)/name;path.write_text(code)
        subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined',str(path),'-o',str(exe)],check=True)
        subprocess.run([str(exe)],check=True)
