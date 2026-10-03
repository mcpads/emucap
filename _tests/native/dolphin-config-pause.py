#!/usr/bin/env python3
"""A native graphics-config refresh must preserve a halted CPU/GPU boundary."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
s=(root/'adapters/dolphin/work/dolphin-src/Source/Core/VideoCommon/VideoConfig.cpp').read_text()
a=s.index('const auto config_changed_callback = [] {');b=s.index('\n    };',a)+7
code=r'''
#include <cassert>
struct Fifo{bool running;int locks=0;void PauseAndLock(){running=false;++locks;}void RestoreState(bool run){if(run)running=true;}};
namespace Core {
 enum class State {Running,Paused,Uninitialized};
 struct System {Fifo fifo{};State state;bool initialized;static System&GetInstance(){static System s;return s;}Fifo&GetFifo(){return fifo;}};
 bool IsRunning(System&s){return s.initialized;}State GetState(System&s){return s.state;}
}
struct {int refreshed=0,verified=0;void Refresh(){++refreshed;}void VerifyValidity(){++verified;}}g_Config;
int main(){
''' + s[a:b] + r'''
 auto&system=Core::System::GetInstance();
 for(auto state:{Core::State::Running,Core::State::Paused,Core::State::Uninitialized}){
  system.state=state;system.initialized=state!=Core::State::Uninitialized;
  system.fifo.running=state==Core::State::Running;system.fifo.locks=0;
  for(int i=0;i<3;i++)config_changed_callback();
  assert(system.fifo.running==(state==Core::State::Running));
  assert(system.fifo.locks==(system.initialized?3:0));
 }
 assert(g_Config.refreshed==9&&g_Config.verified==9);
}
'''
with tempfile.TemporaryDirectory()as d:
 p=Path(d)/'probe.cpp';p.write_text('#include <initializer_list>\n'+code);b=Path(d)/'probe'
 subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(p),'-o',str(b)],check=True);subprocess.run([str(b)],check=True)
print('Graphics refresh preserves running, paused and uninitialized states')
