#!/usr/bin/env python3
"""Exercise native policy observation and application under host overrides.

Native settings/key state and throttle effects are controlled boundaries; policy
classification, revision tracking, parsing and the request handler are production code.
This does not qualify SDL event delivery, VSync or physical output.
"""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
adapter = root / 'adapters/mednafen'
source = (adapter / 'emucap.cpp').read_text()

def function(signature):
    start = source.index(signature)
    return source[start:source.index('\n}', start) + 2]

code = r'''
#include <cassert>
#include "emucap_pacing.h"
#include "emucap_json.hpp"
using nlohmann::json;
using uint32_t = std::uint32_t;
bool audio=false, nothrottle=false;
int held=0, MDFNDnetplay=0;
double CurGameSpeed=1;
uint32_t g_base_percent=100;
bool g_speed_unlimited=false, g_pacing_released=false;
bool g_pacing_control_unverified=false, g_frozen=true;
std::uint64_t g_policy_revision=0;
std::string g_policy_key;
long g_frame=123;
bool ok=false;
std::string message;
json result;
bool MDFN_GetSettingB(const char* name) {
 return std::string(name)=="sound" ? audio : nothrottle;
}
double emucap_base_speed() { return g_base_percent/100.0; }
int emucap_ffsf_state() { return held; }
void RefreshThrottleFPS(double speed) { CurGameSpeed=speed; }
void emucap_ers_resync() {}
void reply_ok(long,const std::string& value) { ok=true;result=json::parse(value); }
void reply_err(long,const char*,const char* value) { ok=false;message=value; }
'''
for signature in ['bool host_audio_enabled()', 'EmucapPacingObservation native_pacing()',
                  'EmucapPacingObservation observe_pacing()', 'void apply_agent_speed()',
                  'void handle_execution_speed(']:
    # host_audio_enabled is a one-line definition.
    if signature == 'bool host_audio_enabled()':
        code += source[source.index(signature):].splitlines()[0] + '\n'
    else:
        code += function(signature) + '\n'
code += r'''
void query() { handle_execution_speed(1,"{}"); assert(ok); }
int main() {
 for(bool sound : {false,true}) for(bool frozen : {false,true}) {
  for(int override_kind=0;override_kind<5;override_kind++) {
   audio=sound;g_frozen=frozen;held=0;MDFNDnetplay=0;nothrottle=false;
   g_base_percent=100;g_speed_unlimited=false;CurGameSpeed=1;
   g_policy_key.clear();g_policy_revision=0;g_pacing_control_unverified=false;
   query();assert(result["mode"]=="limited" && result["percent"]==100);
   const auto original_revision=result["policy_revision"];
   query();assert(result["policy_revision"]==original_revision);
   if(override_kind==1) {held=1;CurGameSpeed=4;}
   if(override_kind==2) {held=2;CurGameSpeed=0.5;}
   if(override_kind==3) MDFNDnetplay=1;
   if(override_kind==4) nothrottle=true;
   const bool overridden=held || MDFNDnetplay || (nothrottle&&!audio);
   query();assert(result["mode"]==(overridden?"custom":"limited"));
   if(overridden) {
    assert(result["percent"].is_null());
    assert(result["policy_revision"]!=original_revision);
   }
   const std::string before=emucap_pacing_key(native_pacing());
   for(const char* request : {R"({"mode":"limited","percent":50})",
                             R"({"mode":"unlimited"})"}) {
    const bool unlimited=std::string(request).find("unlimited")!=std::string::npos;
    // Unlimited explicitly removes the silent nothrottle wait too; held keys
    // and netplay continue to own policy and must reject both requests.
    const bool rejected=held || MDFNDnetplay || (nothrottle&&!audio&&!unlimited);
    handle_execution_speed(1,request);
    assert(ok==!rejected && !g_pacing_control_unverified);
    assert(g_frame==123 && g_frozen==frozen);
    if(rejected) {
     assert(message.find("failed_restored")!=std::string::npos);
     assert(emucap_pacing_key(native_pacing())==before);
    } else {
     assert(result["execution_speed"]["mode"]==(unlimited?"unlimited":"limited"));
    }
   }
   held=0;MDFNDnetplay=0;nothrottle=false;apply_agent_speed();
   handle_execution_speed(1,R"({"mode":"limited","percent":200})");
   assert(ok && result["execution_speed"]["percent"]==200);
   query();const auto revision=result["policy_revision"];
   query();assert(result["policy_revision"]==revision);
  }
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-pacing-overrides-') as directory:
    cpp=Path(directory)/'test.cpp'; binary=Path(directory)/'test'
    cpp.write_text(code)
    subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined',
                    '-I'+str(adapter), '-I'+str(adapter/'work/mednafen/src/drivers'),
                    str(cpp),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)
print('PASS native pacing overrides: key/netplay/nothrottle, revisions, rollback and recovery (ASan/UBSan)')
