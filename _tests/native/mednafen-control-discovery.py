#!/usr/bin/env python3
"""Check the actual producer discovery guard for native callback availability."""
from pathlib import Path
import subprocess
import tempfile

source = Path('adapters/mednafen/emucap.cpp').read_text()
start = source.index('bool owned_native_callbacks_available() {')
end = source.index('\nbool preflight_input_request(', start)
functions = source[start:end]
header = Path('adapters/mednafen/work/mednafen/src/drivers/emucap_json.hpp').resolve()
code = r'''
#include <cassert>
#include <cstdlib>
#include <cstring>
#include <string>
#include "emucap_json.hpp"
struct DebuggerStub { void (*SetCPUCallback)(); };
struct GameStub { DebuggerStub* Debugger; };
GameStub* CurGame;
''' + functions + r'''
void callback() {}
int main() {
 setenv("EMUCAP_LAUNCH_ID","test-runtime",1);
 const std::string original=R"({"state":"frozen","nested":{"preserved":true}})";
 GameStub game{}; DebuggerStub debugger{};
 for(int missing=0;missing<3;missing++) {
  CurGame=missing==0 ? nullptr : &game;
  game.Debugger=missing==1 ? nullptr : &debugger;
  debugger.SetCPUCallback=nullptr;
  std::string result=original;
  assert(!owned_native_callbacks_available()); append_owned_control_capabilities(result);
  assert(result==original);
 }
 CurGame=&game;game.Debugger=&debugger;debugger.SetCPUCallback=callback;
 assert(owned_native_callbacks_available());
 std::string result=original;append_owned_control_capabilities(result);
 const auto value=nlohmann::json::parse(result);
 assert(value["state"]=="frozen" && value["nested"]["preserved"]==true);
 assert(value["control_session_lifecycle"]==true);
 const auto& capability=value["temporal_cancellation_capability"];
 assert(capability["methods"]==nlohmann::json({"step","step_instructions"}));
 assert(capability["control_service_ms"]==50 && capability["stop_host_ms"]==5000);
 for(const auto& id : {std::string(),std::string(129,'x')}) {
  setenv("EMUCAP_LAUNCH_ID",id.c_str(),1);
  std::string unchanged=original;append_owned_control_capabilities(unchanged);
  assert(!owned_control_discovery_available() && unchanged==original);
 }
 setenv("EMUCAP_LAUNCH_ID",std::string(128,'x').c_str(),1);
 assert(owned_control_discovery_available());
 unsetenv("EMUCAP_LAUNCH_ID");
 std::string unchanged=original;append_owned_control_capabilities(unchanged);
 assert(!owned_control_discovery_available() && unchanged==original);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-control-discovery-') as temp:
    cpp = Path(temp) / 'test.cpp'; binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    '-I' + str(header.parent), str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Native callback guard and shared hello/status capability decoration pass ASan/UBSan')
