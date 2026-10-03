#!/usr/bin/env python3
"""Exercise the maintained state transaction across delayed native job finalization."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
text = (root / 'adapters/dolphin/EmuCap.cpp').read_text()
state = text[text.index('class StateCall :'):text.index('\nbool SendLine(', text.index('class StateCall :'))]
code = r'''
#include <picojson.h>
#include <atomic>
#include <cassert>
#include <chrono>
#include <condition_variable>
#include <functional>
#include <future>
#include <memory>
#include <optional>
#include <mutex>
#include <string>
#include <thread>
std::atomic<bool> s_stop{false},s_request_cancelled{false},s_control_retired{false};
std::string s_handler_error;
std::mutex s_cancel_mutex;
std::optional<std::chrono::steady_clock::time_point> s_cancel_origin;
namespace Temporal {constexpr auto OPERATION_BUDGET=std::chrono::seconds(10);}
namespace Core {
struct System{};enum class State{Paused};
thread_local bool cpu=false;
std::atomic<bool> active{false};
std::promise<void> finalizing,release;
std::thread worker;
State GetState(System&){return State::Paused;}
uint64_t EmucapWithParkedMemory(System&,uint64_t,const std::function<void()>& f){
 if(active&&!cpu)return 0;f();return 1;
}
void RunOnCPUThreadWithHostLock(System&,std::function<void()> f){
 worker=std::thread([f]{cpu=true;active=true;f();finalizing.set_value();release.get_future().wait();active=false;});
}
}
using Handler=picojson::object(*)(Core::System&,const picojson::object&);
picojson::object Fail(const char*,const char* message){s_handler_error=message;return {};}
bool VerifyFrozenPublication(Core::System&,const picojson::object&,bool){return true;}
bool AwaitMemoryPark(Core::System& system,std::chrono::steady_clock::time_point deadline){
 while(!s_stop&&std::chrono::steady_clock::now()<deadline){
  if(Core::EmucapWithParkedMemory(system,0,[]{}))return true;
  std::this_thread::yield();
 }return false;
}
''' + state + r'''
picojson::object Load(Core::System&,const picojson::object&){return {{"state",picojson::value(std::string("frozen"))}};}
int main(){
 for(bool interrupted:{false,true}){
  s_stop=false;s_control_retired=false;
  Core::finalizing=std::promise<void>();Core::release=std::promise<void>();
  Core::System system;auto call=std::make_shared<StateCall>();
  s_handler_error.clear();
  auto completion=std::async(std::launch::async,[&]{return call->Run(system,Load,{});});
  Core::finalizing.get_future().wait();
  // The state result cannot be published while the native job still owns memory.
  assert(Core::active);
  assert(completion.wait_for(std::chrono::milliseconds(20))==std::future_status::timeout);
  if(interrupted)s_stop=true;
  else Core::release.set_value();
  auto result=completion.get();
  assert(s_control_retired==interrupted);
  if(interrupted){assert(!s_handler_error.empty());assert(result.empty());Core::release.set_value();}
  else {assert(s_handler_error.empty());assert(result.at("state").get<std::string>()=="frozen");}
  Core::worker.join();
 }
}
'''
native = root / 'adapters/dolphin/work/dolphin-src'
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / 'probe.cpp'
    binary = Path(directory) / 'probe'
    source.write_text(code)
    subprocess.run(['clang++', '-std=c++20', '-pthread', '-fsanitize=address,undefined',
                    '-I', str(next(native.rglob('picojson.h')).parent), str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('State callback completion waits for native job release; failed cleanup retires control')
