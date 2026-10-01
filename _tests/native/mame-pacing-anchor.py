#!/usr/bin/env python3
"""Run native limiter/setter bodies against controlled host and guest clocks.

The clock double implements attotime's one-second scalar saturation; native MAME
build and real low-rate runs separately verify the actual attotime implementation.
"""
import argparse,re,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,default=ROOT/'adapters/mame-neogeo/work/mame-src/src/emu')
a=p.parse_args();cpp=(a.source/'video.cpp').read_text();h=(a.source/'video.h').read_text()
def body(text,signature):
 start=text.index(signature);end=text.index('{',start);depth=1;i=end+1
 while depth:
  depth+=(text[i]=='{')-(text[i]=='}');i+=1
 return text[start:i]
setters='\n'.join(body(h,'void set_'+n+'(') for n in ['throttled','throttle_rate','fastforward','speed_factor'])
methods=body(cpp,'void video_manager::update_throttle(')+'\n'+body(cpp,'void video_manager::postload(')+'\n'+body(cpp,'void video_manager::resume(')
resume=body((a.source/'machine.cpp').read_text(),'void running_machine::resume(').replace('running_machine::','Machine::')
registration=next(line for line in cpp.splitlines() if 'machine.add_notifier(MACHINE_NOTIFY_RESUME,' in line)
preamble=r'''
#include <cassert>
#include <functional>
#include <cmath>
#include <cstdint>
#include <limits>
#include <string>
#include <vector>
using u32=uint32_t;using attoseconds_t=int64_t;using osd_ticks_t=uint64_t;
constexpr int64_t ATTOSECONDS_PER_SECOND=1000000000000000000LL;
constexpr bool LOG_THROTTLE=false;constexpr unsigned PAUSED_REFRESH_RATE=60;
struct attotime {
 long double seconds=0;
 attotime()=default;attotime(int s,int64_t a):seconds(s+(long double)a/ATTOSECONDS_PER_SECOND){}
 static const attotime zero;
 static attotime from_double(long double s){attotime a;a.seconds=s;return a;}
 double as_double()const{return seconds;}
 attoseconds_t as_attoseconds()const {return seconds>=1?ATTOSECONDS_PER_SECOND:seconds<=-1?-ATTOSECONDS_PER_SECOND:(int64_t)(seconds*ATTOSECONDS_PER_SECOND);}
 std::string as_string(int)const{return "clock";}
 attotime &operator+=(attotime a){seconds+=a.seconds;return *this;}
};
const attotime attotime::zero{};
static attotime operator+(attotime a,attotime b){return attotime::from_double(a.seconds+b.seconds);}
static attotime operator-(attotime a,attotime b){return attotime::from_double(a.seconds-b.seconds);}
static attotime operator*(attotime a,unsigned b){return attotime::from_double(a.seconds*b);}
static attotime operator/(attotime a,unsigned b){return attotime::from_double(a.seconds/b);}
[[maybe_unused]] static bool operator<(attotime a,attotime b){return a.seconds<b.seconds;}
[[maybe_unused]] static bool operator>=(attotime a,attotime b){return a.seconds>=b.seconds;}
static unsigned population_count_32(u32 x){return __builtin_popcount(x);}
static uint64_t ticks=10000000,waited;
static uint64_t osd_ticks(){return ticks;}
static uint64_t osd_ticks_per_second(){return 1000000;}
enum {MACHINE_NOTIFY_RESUME};
template<class C>std::function<void()> machine_notify_delegate(void(C::*fn)(),C* object){
 return [=](){(object->*fn)();};
}
struct Machine {
 bool m_paused=false;unsigned notifications=0;std::function<void()> on_resume;
 void add_notifier(int kind,std::function<void()> f){assert(kind==MACHINE_NOTIFY_RESUME);on_resume=f;}
 void call_notifiers(int kind){assert(kind==MACHINE_NOTIFY_RESUME);++notifications;on_resume();}
 void resume();
 bool paused()const{return m_paused;}attotime time()const{return attotime::from_double(100);}
 template<class... T>void logerror(T...){}
};
struct Recording {void set_next_frame_time(attotime){}};
class video_manager {
public:
 Machine m_machine;Machine &machine(){return m_machine;}
 std::vector<Recording*>m_movie_recordings;
 osd_ticks_t m_speed_last_realtime=0,m_throttle_last_ticks=0;
 attotime m_speed_last_emutime,m_throttle_emutime,m_throttle_realtime;
 u32 m_speed=1000,m_throttle_history=0;
 bool m_throttled=true,m_fastforward=false,m_throttle_resync=true;
 float m_throttle_rate=1;
 uint64_t throttle_until_ticks(uint64_t target){assert(target>=ticks);waited+=target-ticks;ticks=target;return ticks;}
 void update_throttle(attotime);void postload();void resume();
 video_manager(){auto& machine=m_machine;EMUCAP_RESUME_REGISTRATION}
'''
preamble=preamble.replace('EMUCAP_RESUME_REGISTRATION',registration)
cases=r'''
int main(){
 const unsigned rates[]={1000,10,1,11,500,4000,100000};
 video_manager v;attotime guest=attotime::from_double(100);
 for(unsigned rate:rates){
  v.set_speed_factor(rate);waited=0;v.update_throttle(guest);assert(waited==0);
  guest=guest+attotime::from_double(1.0L/60);v.update_throttle(guest);
  long double expected=1000000.0L/60*1000/rate;
  assert(fabsl((long double)waited-expected)<3);
  v.set_speed_factor(rate);waited=0;guest=guest+attotime::from_double(1.0L/60);v.update_throttle(guest);
  assert(fabsl((long double)waited-expected)<3);
 }
 // Long forward jumps must be compared as durations, including at the minimum speed.
 v.set_speed_factor(1);v.update_throttle(guest);waited=0;
 guest=guest+attotime::from_double(20);v.update_throttle(guest);assert(waited==0);
 // Policy transitions and load reset host history but retain the selected policy.
 auto reanchor=[&](){guest=guest+attotime::from_double(1.0L/60);waited=0;v.update_throttle(guest);assert(!waited);};
 v.set_fastforward(true);reanchor();
 v.set_fastforward(false);reanchor();
 v.set_throttled(false);reanchor();
 v.set_throttled(true);reanchor();
 v.set_throttle_rate(0.5f);reanchor();
 v.postload();assert(v.m_speed==1&&v.m_throttle_rate==0.5f);
 waited=0;v.update_throttle(attotime::from_double(2));assert(!waited);
 // Actual machine resume notification invalidates the video owner's anchor.
 for(unsigned rate:rates)for(unsigned pause_ticks:{1000u,50000u,10000000u}) {
  v.set_speed_factor(rate);v.set_throttle_rate(1);v.update_throttle(guest);
  for(unsigned i=0;i<10;++i){guest=guest+attotime::from_double(1.0L/60);v.update_throttle(guest);}
  v.machine().m_paused=true;ticks+=pause_ticks;
  auto count=v.machine().notifications;v.machine().resume();
  assert(v.machine().notifications==count+1 && v.m_throttle_resync);
  assert(v.m_speed==rate && v.m_throttled && !v.m_fastforward);
  waited=0;guest=guest+attotime::from_double(1.0L/60);v.update_throttle(guest);assert(!waited);
  // Repeating resume while running must not grant another free interval.
  v.machine().resume();assert(v.machine().notifications==count+1 && !v.m_throttle_resync);
  waited=0;guest=guest+attotime::from_double(1.0L/60);v.update_throttle(guest);
  long double expected=1000000.0L/60*1000/rate;
  assert(fabsl((long double)waited-expected)<3);
 }
 return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='mame-pacing-anchor-') as d:
 d=Path(d);(d/'test.cpp').write_text(preamble+setters+'\n};\n'+methods+'\n'+resume+cases)
 subprocess.run(['c++','-std=c++17','-O1','-g','-fsanitize=address,undefined','-Wall','-Wextra','-Werror','-Wno-sign-compare',str(d/'test.cpp'),'-o',str(d/'test')],check=True)
 subprocess.run([str(d/'test')],check=True)
print('native limiter: actual resume notification, no-op resume, policy/load reanchor, no-op preservation, full-duration jumps and low-rate frame waits passed')
