#!/usr/bin/env python3
"""Check native park release excludes short frozen host time from pacing credit."""
from pathlib import Path
import argparse,os,subprocess,tempfile
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--baseline-ref',help='git revision of the pre-fix adapter source');a=p.parse_args()
root=Path(__file__).resolve().parents[2]
def function(path,signature):
 s=(subprocess.check_output(['git','show',a.baseline_ref+':'+str(path.relative_to(root))],cwd=root,text=True) if a.baseline_ref else path.read_text());start=s.index(signature);end=s.index('{',start);depth=1;i=end+1
 while depth:
  depth+=(s[i]=='{')-(s[i]=='}');i+=1
 return s[start:i]
def park_loop(path,signature):
 s=function(path,signature)
 start=s.index('bool pacing_parked = false;')
 end=s.index('if (pacing_parked) emucap_ers_resync();',start)+len('if (pacing_parked) emucap_ers_resync();')
 return s[start:end]
common=r'''
#include <algorithm>
#include <memory>
#include <atomic>
#include <vector>
#include <string>
#include <cassert>
#include <cstdint>
#include <exception>
#include <stdexcept>
#include <cstdio>
#include <cstdlib>
using int64=int64_t;
static int64_t now=1000,waited=0,pause_ms=50;
static bool g_frozen=false,g_frozen_via_cb=false,g_emucap_park_pending=true;
static int g_step_remaining=0,g_insn_remaining=0,g_fd=1,g_probe_id=-1;
static void serve_socket_once(){now+=pause_ms;g_frozen=false;}
static void emucap_connect(){g_fd=1;}
static void usleep(unsigned us){now+=us/1000;}
static void contain_service_exception(const char*,const char*){std::exit(2);}
static void check(bool pass){if(!pass){std::puts("short frozen interval was counted as execution credit");std::exit(1);}}
'''
med=root/'adapters/mednafen';src=med/'work/mednafen/src/drivers'
ers='\n'.join(l for l in (src/'ers.cpp').read_text().splitlines() if not l.startswith('#include'))
code=common+r'''
#include "emucap_native_control.h"
static EmucapControl::NativeControl g_native_control;
static void owned_poll() {}
namespace Time {
int64_t MonoMS(){return now;}
void SleepMS(int64_t ms){ms=std::max<int64_t>(ms,1);now+=ms;waited+=ms;}
}
extern "C" bool emucap_pacing_idle(){return false;}
'''+(src/'ers.h').read_text()+ers+r'''
static EmuRealSyncher ers;
extern "C" void emucap_ers_resync(){ers.SetETtoRT();}
'''+function(med/'emucap.cpp','void freeze_spin_until_resume() {')+('' if a.baseline_ref else '\nvoid frame_park(){'+park_loop(med/'emucap.cpp','void emucap_service(uint64_t frame) {')+'}\nvoid startup_park(){'+park_loop(med/'emucap.cpp','void emucap_pre_first_frame() {')+'}\n')+r'''
int main(){
 for(auto park : {freeze_spin_until_resume})
 for(auto duration : {1,50,10000}) {
 pause_ms=duration;g_step_remaining=0;g_frozen=true;
 ers.SetEmuClock(1000);ers.AddEmuTime(16);ers.Sync();
 park();waited=0;
 ers.AddEmuTime(16);ers.Sync();check(waited>=15 && waited<=17);
 // No actual park means the pending pacing debt is retained.
 ers.AddEmuTime(10);g_step_remaining=1;park();waited=0;ers.Sync();
 check(waited>=9 && waited<=11);
 }
}
'''
if not a.baseline_ref:code=code.replace('{freeze_spin_until_resume}', '{freeze_spin_until_resume,frame_park,startup_park}')
fly=root/'adapters/flycast'
flycode=common+r'''
#include "emucap_pacing.h"
static EmucapSamplePacer g_pacer;
static long g_step_id=-1,g_boundary_reply_id=-1,g_test_adapter_exception_id=-1;
static uint64_t g_frame=0;
static bool g_renderer_unverified=false,g_synthetic_fatal_pending=false;
static std::atomic<bool> g_failure_shutdown_requested{false};
static std::string g_boundary_reply;
static struct { uint32_t pc=0; } Sh4cntx;
static bool exclude_renderer_writes(long){now+=pause_ms;return true;}
static void reply_ok(long,const std::string&) {assert(false);}
static void reply_err(long,const char*,const char*) {assert(false);}
static void emucap_capture_fatal_sh4(const char*,uint32_t,uint32_t,int,int,int){assert(false);}

struct Hit {uint32_t pc;std::string registers;};
static std::vector<Hit> g_bp_hits;
static std::string emucap_capture_regs(){return "{}";}
'''+function(fly/'emucap.cpp','void emucap_park() {')+function(fly/'emucap.cpp','void emucap_bp_spin(uint32_t pc) {')+r'''
static void bp_park(){emucap_bp_spin(1234);}
int main(){
 for(auto park : {emucap_park,bp_park})
 for(auto duration : {1,50,10000}) {
 pause_ms=duration;g_step_remaining=0;g_pacer.reanchor();
 const int64_t chunk=512LL*1000000000/EMUCAP_AICA_RATE;
 auto prior=g_pacer.deadline_after(512,100,now*1000000);
 now=prior/1000000;g_frozen=true;park();
 auto next=g_pacer.deadline_after(512,100,now*1000000);
 check(next==now*1000000+chunk);
 if(park==emucap_park) {
  pause_ms=1;g_frozen=true;g_step_remaining=1;park();
  check(g_pacer.deadline_after(512,100,now*1000000)==next+chunk);
 }
 }
}
'''
with tempfile.TemporaryDirectory(prefix='pacing-park-') as d:
 for name,code in [('mednafen',code),('flycast',flycode)]:
  cpp=Path(d)/(name+'.cpp');exe=Path(d)/name;cpp.write_text(code)
  subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined','-I'+str(root/'adapters'/name),str(cpp),'-o',str(exe)],check=True)
  r=subprocess.run([str(exe)],capture_output=True,text=True,env=dict(os.environ,UBSAN_OPTIONS="halt_on_error=1"))
  if a.baseline_ref:assert r.returncode==1,(name,r.returncode,r.stdout,r.stderr)
  else:assert r.returncode==0,(name,r.returncode,r.stdout,r.stderr)
  print(name, 'counterexample reproduced' if a.baseline_ref else 'short-park release and no-park pacing debt passed')
