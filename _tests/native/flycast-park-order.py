#!/usr/bin/env python3
"""Actual adapter terminal paths wait for a controlled renderer completion."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
s=(root/'adapters/flycast/emucap.cpp').read_text()
def function(name):
 start=s.index(name);return s[start:s.index('\n}',start)+2]
code=r'''
#include "emucap_render_fence.h"
#include <atomic>
#include <cassert>
#include <cstdio>
#include <future>
#include <stdexcept>
#include <string>
#include <unistd.h>
using namespace std::chrono_literals;
bool g_frozen=false,g_renderer_unverified=false,g_emucap_park_pending=false;
bool g_pacing_released=false,g_synthetic_fatal_pending=false;
long g_step_id=-1,g_step_remaining=0,g_step_requested=0,g_boundary_reply_id=-1,g_test_adapter_exception_id=-1;
int g_fd=1;uint64_t g_frame=0,g_step_progress_ms=0;
std::string g_tx,g_boundary_reply;
std::atomic<uint64_t> g_observed_frame{0};
std::atomic<bool> g_failure_shutdown_requested{false};
std::atomic<bool> g_renderer_failure_pending{false};
std::atomic<bool> g_capture_disabled{false};
int renderer_records=0;
const int PROGRESS_INTERVAL_MS=1000;
struct {void set(unsigned){}} g_input_override;
struct {void reanchor(){}} g_pacer;
struct {unsigned pc=0;} Sh4cntx;
std::atomic<int> ok{0},errors{0};int failure_records=0;
std::string terminal;
std::promise<void>* entered=nullptr;std::shared_future<void> released;
EmucapRenderFence::Result result=EmucapRenderFence::Result::complete;
EmucapRenderFence::Result rend_emucap_wait_writers(unsigned){
 if(entered){auto p=entered;entered=nullptr;p->set_value();released.wait();}return result;
}
void rend_emucap_fail_writers(){result=EmucapRenderFence::Result::failed;}
bool publish_native_failure(const char* operation,const char* reason,bool active,const char* state){
 assert(std::string(operation)=="renderer" && reason && active && std::string(state)=="unknown");
 ++renderer_records;return true;
}
void remember_active_native_failure(const char*,const char*){++failure_records;}
void reply_ok(long,const std::string& value){terminal=value;++ok;}
void reply_err(long,const char*,const char*){++errors;}
void reply_working(long){}
uint64_t monotonic_ms(){return 0;}
void emucap_connect(){}
void flush_tx_once(){}
bool expired=false;
bool advance_expired(){return expired;}
void contain_service_exception(const char*,const char*){assert(false);}
void emucap_capture_fatal_sh4(const char*,unsigned,unsigned,int,int,int){}
void serve_socket_once(){
 // Model an authorized release, or teardown of an unverified session.
 if(g_renderer_unverified)g_failure_shutdown_requested=true;else g_frozen=false;
}
'''+function('bool exclude_renderer_writes(')+'\n'+function('void emucap_service()')+'\n'+function('void emucap_park()')+'\n'+function('void emucap_renderer_failed(')+r'''
int main(){
 for(bool timeout : {false,true}) for(bool deadline : {false,true}) {
  ok=0;errors=0;g_renderer_unverified=false;g_failure_shutdown_requested=false;
  g_step_id=7;g_step_remaining=deadline?3:1;g_step_requested=g_step_remaining;
  g_frozen=true;expired=deadline;g_emucap_park_pending=false;
  result=timeout?EmucapRenderFence::Result::timed_out:EmucapRenderFence::Result::complete;
  emucap_service();
  assert(ok==0 && errors==0 && g_emucap_park_pending && g_boundary_reply_id==7);
  std::promise<void> pending,release;entered=&pending;released=release.get_future().share();
  auto park=std::async(std::launch::async,[]{emucap_park();});
  pending.get_future().wait();assert(ok==0 && errors==0);
  assert(park.wait_for(10ms)==std::future_status::timeout);
  release.set_value();park.get();
  assert(ok==(timeout?0:1) && errors==(timeout?1:0));
  assert(g_renderer_unverified==timeout);
  if(!timeout)assert(terminal.find(deadline?"interrupted":"completed")!=std::string::npos);
  if(timeout){
   result=EmucapRenderFence::Result::complete;
   assert(!exclude_renderer_writes(9)); // A later apparent ack cannot heal uncertainty.
  }
 }
 g_renderer_unverified=false;
 const int prior_generic_records=failure_records;
 emucap_renderer_failed("controlled render exception");
 assert(g_renderer_failure_pending && g_capture_disabled && renderer_records==1);
 assert(!exclude_renderer_writes(10));
 assert(failure_records==prior_generic_records); // Preserve the original renderer reason.
}
'''
with tempfile.TemporaryDirectory(prefix='flycast-park-') as d:
 d=Path(d);(d/'check.cpp').write_text(code)
 subprocess.run(['clang++','-std=c++17','-O1','-g','-pthread','-fsanitize=address,undefined','-I'+str(root/'adapters/flycast'),str(d/'check.cpp'),'-o',str(d/'check')],check=True)
 subprocess.run([str(d/'check')],check=True,timeout=20)
print('PASS actual service/park: deferred completed/interrupted replies, pending writer, failed barrier, retained uncertainty')
