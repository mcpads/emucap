#!/usr/bin/env python3
"""Actual load handler excludes old writers before replacing guest state."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
s=(root/'adapters/flycast/emucap.cpp').read_text()
start=s.index('void handle_load_state(');body=s[start:s.index('\n}',start)+2]
code=r'''
#include <atomic>
#include <cassert>
#include <cstdio>
#include <future>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>
using namespace std::chrono_literals;
using u8=unsigned char;using u32=unsigned;
constexpr int EMUCAP_FLYCAST_STATE_HEADER=0;
bool g_renderer_unverified=false,g_frozen=true,g_fb_fresh=true;
std::mutex g_fb_mtx;
int vblank_schid=0;
std::atomic<int> loads{0},replies{0},errors{0};int generations=0;
bool allow=true,throw_load=false;
std::promise<void>* pending=nullptr;std::shared_future<void> released;
std::string json_str(const std::string& path,const char*){return path;}
void reply_ok(long,const char*){++replies;}
void reply_err(long,const char*,const char*){++errors;}
void remember_active_native_failure(const char*,const char*){}
bool exclude_renderer_writes(long){
 if(pending){auto p=pending;pending=nullptr;p->set_value();released.wait();}
 if(!allow)++errors;return allow;
}
bool emucap_state_parse(const std::vector<u8>&,u32& a,u32& b){a=1;b=2;return true;}
struct Sh4Interpreter{
 static Sh4Interpreter* Instance;
 void restoreTiming(u32 a,u32 b){assert(a==1 && b==2);}
} interpreter;
Sh4Interpreter* Sh4Interpreter::Instance=&interpreter;
struct Deserializer{Deserializer(u8*,size_t){}};
struct {void loadstate(Deserializer&){++loads;if(throw_load)throw std::runtime_error("partial restore");}} emu;
void rend_emucap_replace_generation(){assert(loads==1);++generations;}
bool sh4_sched_is_scheduled(int){return true;}
void rescheduleSPG(){}
'''+body+r'''
int main(int argc,char** argv){
 assert(argc==2);
 for(bool fail : {false,true}) {
  loads=0;replies=0;errors=0;generations=0;allow=!fail;g_fb_fresh=true;
  std::promise<void> entered,release;pending=&entered;released=release.get_future().share();
  auto load=std::async(std::launch::async,[&]{handle_load_state(7,argv[1]);});
  entered.get_future().wait();assert(loads==0 && replies==0);
  assert(load.wait_for(10ms)==std::future_status::timeout);
  release.set_value();load.get();
  assert(loads==(fail?0:1) && replies==(fail?0:1) && errors==(fail?1:0));
  assert(generations==(fail?0:1));assert(g_fb_fresh==fail);
 }
 allow=true;throw_load=true;loads=0;replies=0;errors=0;generations=0;
 handle_load_state(8,argv[1]);
 assert(loads==1 && replies==0 && errors==1 && generations==0);
 assert(g_renderer_unverified && g_frozen);
}
'''
with tempfile.TemporaryDirectory(prefix='flycast-restore-') as d:
 d=Path(d);(d/'check.cpp').write_text(code);(d/'state').write_bytes(b'fixture')
 subprocess.run(['clang++','-std=c++17','-O1','-g','-pthread','-fsanitize=address,undefined',str(d/'check.cpp'),'-o',str(d/'check')],check=True)
 subprocess.run([str(d/'check'),str(d/'state')],check=True,timeout=20)
print('PASS actual restore handler: pending old writer, barrier failure prevents replacement, generation follows replacement, partial restore quarantined')
