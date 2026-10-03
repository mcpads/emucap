#!/usr/bin/env python3
"""Compile actual direct-adapter pacing handlers and dispatch admission with faults."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
setup = r'''
#include <cassert>
#include <atomic>
#include <cstdint>
#include <stdexcept>
#include <string>
using uint32_t=std::uint32_t;
struct EmucapPacingRequest { bool query=false, unlimited=false; uint32_t percent=200; };
struct EmucapPacingObservation { int key; };
// Renderer completion is a controlled dependency here. Dedicated renderer
// fixtures own actual writer exclusion/publication; this test owns pacing faults.
std::atomic<bool> g_renderer_failure_pending{false};
bool g_renderer_unverified=false;
bool exclude_renderer_writes(long) { return !g_renderer_unverified; }
bool g_input_control_unverified=false;
bool g_pacing_control_unverified=false, g_frozen=true, g_in_pacing_wait=false, g_pacing_released=false;
bool g_speed_unlimited=false, g_pace_unlimited=false;
uint32_t g_base_percent=100, g_pace_percent=100;
int g_policy_revision=0, g_frame=1, observations=0, reads=0, fault=0, dispatched=0;
bool reply_success=false;
std::string reply_message, reply_payload;
struct Pacer { void reanchor() {} } g_pacer;
bool host_audio_enabled() { return false; }
bool emucap_parse_pacing_request(const std::string&,EmucapPacingRequest&,std::string&) { return true; }
EmucapPacingObservation observe_pacing() { ++observations; return {100}; }
EmucapPacingObservation native_pacing() {
  ++reads;
  if(fault==3 && reads==1) throw std::runtime_error("post-mutation read failure");
  if(fault==4 && reads==2) throw std::runtime_error("rollback read failure");
  return {fault==2 && reads==2 ? 150 : 100};
}
bool emucap_pacing_confirms(const EmucapPacingRequest&,EmucapPacingObservation) { return fault==0; }
int emucap_pacing_key(EmucapPacingObservation p) { return p.key; }
std::string emucap_pacing_policy_json(EmucapPacingObservation,int,bool=false) { return "{}"; }
void apply_agent_speed() {}
void reply_ok(long,const std::string& payload) { reply_success=true; reply_payload=payload; }
void reply_err(long,const char*,const char* message) { reply_success=false; reply_message=message; }
std::string json_str(const std::string& value,const char*) { return value; }
void json_num(const std::string&,const char*,long& id) { id=1; }
'''
check = r'''
int main() {
 for(bool stopped: {false, true}) {
 for(fault=0; fault<=4; ++fault) {
  g_frozen=stopped; g_frame=123;
  g_pacing_control_unverified=false; observations=reads=dispatched=0;
  bool threw=false;
  try { handle_execution_speed(1, ""); } catch(...) { threw=true; }
  assert(g_frozen==stopped && g_frame==123);
  if(fault==0) {
   assert(reply_payload.find(stopped ? "\"state\":\"frozen\"" : "\"state\":\"running\"")!=std::string::npos);
   assert(reply_payload.find("\"frame\":123")!=std::string::npos);
  }
  const bool healthy=fault<=1;
  assert(g_pacing_control_unverified==!healthy);
  assert(threw==(fault>=3));
  if(!threw) assert(reply_success==(fault==0));
  for(const char* method: {"hello","status","resume","step","load_state","set_input","execution_speed"}) {
   const int before=dispatched;
   handle(method);
   assert(dispatched==before+(healthy?1:0));
   if(!healthy) assert(!reply_success && reply_message.find("unverified")!=std::string::npos);
  }
 }
 }
}
'''
for adapter in ['mednafen','flycast']:
    source=(root/f'adapters/{adapter}/emucap.cpp').read_text()
    start=source.index('void handle_execution_speed(')
    handler=source[start:source.index('\n}',start)+2]
    if adapter=='mednafen':
        # Owned dispatch now calls the legacy operation handler after decoding.
        # Exercise that handler's real input/pacing admission, not the wire parser.
        start=source.index('void handle_legacy(')
        start=source.index('{',start)+1
        end=source.index('  // 어떤 핸들러',start)
        admission='void handle(const std::string&) { long id=1;'+source[start:end]
    else:
        start=source.index('void handle(const std::string& line) {')
        end=source.index('\tif (g_failure_active',start)
        admission=source[start:end]
    admission+='\n ++dispatched;\n}\n'
    with tempfile.TemporaryDirectory(prefix='emucap-direct-policy-') as temp:
        cpp=Path(temp)/'test.cpp';binary=Path(temp)/'test'
        cpp.write_text(setup+handler+admission+check)
        subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(cpp),'-o',str(binary)],check=True)
        subprocess.run([str(binary)],check=True)
    print(f'PASS {adapter}: running/frozen state and frame preserved through application, restoration, mismatch and read exceptions',flush=True)
