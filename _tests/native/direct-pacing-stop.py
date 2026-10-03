#!/usr/bin/env python3
"""Run actual pacing handlers inside actual native instruction-stop loops."""
import ast
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
# Share controlled policy dependencies without executing the separate fault suite.
module = ast.parse((ROOT / '_tests/native/direct-pacing-failure.py').read_text())
setup = next(ast.literal_eval(node.value) for node in module.body
             if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'setup' for t in node.targets))

support = r'''
#include <vector>
#include <utility>
#include <memory>
#include "emucap_native_control.h"
EmucapControl::NativeControl g_native_control;
bool check_native_context=false;
EmucapControl::NativeControl::Context expected_context=EmucapControl::NativeControl::Context::None;
// No parent operation in this legacy pacing-service witness. Owned cancellation
// and cleanup are exercised by the dedicated producer ownership tests.
void owned_poll() {}
int g_step_remaining=0, g_insn_remaining=0, g_fd=1;
bool g_frozen_via_cb=false, released_by_test=false;
int services=0, sleeps=0, connects=0;
long g_step_id=-1, g_boundary_reply_id=-1, g_test_adapter_exception_id=-1;
std::string g_boundary_reply;
bool g_emucap_park_pending=false, g_synthetic_fatal_pending=false;
std::atomic<bool> g_failure_shutdown_requested{false};
struct { uint32_t pc=0; } Sh4cntx;
void emucap_capture_fatal_sh4(const char*,uint32_t,uint32_t,int,int,int) {
 assert(false && "fatal path is outside this pacing-service fixture");
}
std::vector<std::pair<uint32_t,std::string>> g_bp_hits;
std::string emucap_capture_regs() { return "stopped-registers"; }
void emucap_connect() { if(++connects==2) g_fd=1; }
void emucap_ers_resync() {}
void contain_service_exception(const char*,const char*) { assert(false && "unexpected exception"); }
int usleep(unsigned) { assert(++sleeps<10); return 0; }
void serve_socket_once();
'''
service = r'''
void serve_socket_once() {
 assert(g_frozen && g_step_remaining==0 && g_insn_remaining==0);
 if(check_native_context) {
  assert(g_native_control.CurrentContext()==expected_context);
  const bool cpu_boundary=expected_context==EmucapControl::NativeControl::Context::Cpu
      || expected_context==EmucapControl::NativeControl::Context::Continuation;
  assert(g_native_control.Parked(g_frozen)==cpu_boundary);
 }
 ++services;
 if(services==1) {
  handle_execution_speed(1, "");
  assert(g_frozen && g_frame==123);
  assert(reply_success==(fault==0));
  assert(g_pacing_control_unverified==(fault==2));
  if(fault==0) assert(reply_payload.find("\"state\":\"frozen\"")!=std::string::npos);
 } else {
  // Reaching a second service turn proves pacing did not release the park.
  assert(services==2);
  released_by_test=true;
  g_frozen=false;  // Explicit harness teardown, not a producer resume request.
 }
}
'''
for adapter, owner in [('flycast', 'void emucap_bp_spin(uint32_t pc)'),
                       ('mednafen', 'void freeze_spin_until_resume()')]:
    source = (ROOT / f'adapters/{adapter}/emucap.cpp').read_text()
    def function(signature):
        start = source.index(signature + ' {')
        return source[start:source.index('\n}', start) + 2]
    call = 'emucap_bp_spin(0x1234)' if adapter == 'flycast' else 'freeze_spin_until_resume()'
    event_check = ('assert(g_bp_hits.size()==1 && g_bp_hits[0].first==0x1234 && '
                   'g_bp_hits[0].second=="stopped-registers");' if adapter == 'flycast'
                   else 'assert(g_frozen_via_cb);')
    main = r'''
int main() {
 for(int context=0;context<CONTEXT_COUNT;++context)
 for(int disconnected: {0,1}) for(fault=0; fault<=2; ++fault) {
  g_frozen=false; g_frozen_via_cb=false; g_frame=123; g_fd=disconnected?-1:1;
  services=sleeps=connects=reads=observations=0;
  g_pacing_control_unverified=false; released_by_test=false; g_bp_hits.clear();
  CONTEXT_SETUP
  CALL;
  assert(released_by_test && services==2 && g_frame==123);
  assert(connects==(disconnected?2:0));
  EVENTS
 }
}
'''.replace('CALL', call).replace('EVENTS', event_check)
    main=main.replace('CONTEXT_COUNT', '3' if adapter=='mednafen' else '1')
    context_setup = r'''
  check_native_context=true;
  using Context=EmucapControl::NativeControl::Context;
  const Context entry=context==0?Context::Cpu:(context==1?Context::Continuation:Context::None);
  expected_context=context==2?Context::Device:entry;
  EmucapControl::NativeControl::Scope scope(g_native_control,entry);
''' if adapter=='mednafen' else ''
    main=main.replace('CONTEXT_SETUP',context_setup)
    park = function('void emucap_park()') if adapter == 'flycast' else ''
    code = setup + support + function('void handle_execution_speed(long id, const std::string& line)') + service + park + function(owner) + main
    with tempfile.TemporaryDirectory(prefix='emucap-native-stop-') as temp:
        cpp, binary = Path(temp)/'check.cpp', Path(temp)/'check'
        cpp.write_text(code)
        subprocess.run(['clang++', '-std=c++17', '-fsanitize=address,undefined', '-I'+str(ROOT/'adapters/mednafen'), str(cpp), '-o', str(binary)], check=True)
        subprocess.run([str(binary)], check=True, timeout=10)
    print(f'PASS {adapter}: stop loop retained across success/restore/unverified pacing and reconnect', flush=True)
