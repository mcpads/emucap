#!/usr/bin/env python3
"""Actual AsyncIOManager queue/worker + Core park/batch latch, controlled filesystem.

Does not model OS file latency, native CPU/GPU scheduling or WebSocket delivery.
"""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, required=True, help='patched PPSSPP source root')
a = p.parse_args()
r = a.source

def function(s, signature):
    start = s.index(signature)
    brace = s.index('{', start)
    level = 1
    end = brace + 1
    while level:
        level += (s[end] == '{') - (s[end] == '}')
        end += 1
    return s[start:end]

header = (r / 'Core/HW/AsyncIOManager.h').read_text()
header = re.sub(r'^#(?:include|pragma).*$', '', header, flags=re.M)
io = (r / 'Core/HW/AsyncIOManager.cpp').read_text()
io = re.sub(r'^#include.*$', '', io, flags=re.M)
core = (r / 'Core/Core.cpp').read_text()
globals_ = core[core.index('static std::recursive_mutex g_stepMutex;'):core.index('struct CPUStepCommand')]
functions = '\n'.join(function(core, sig) for sig in [
    'bool Core_EmucapMemoryParked()',
    'bool Core_EmucapWithHaltedCPU(',
])
functions += '\n' + function(
    (r / 'Core/Debugger/WebSocket/SaveStateSubscriber.cpp').read_text(),
    'static bool WaitForLoadedMemoryPark(',
)
support = r'''
#include <algorithm>
#include <atomic>
#include <cassert>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <functional>
#include <future>
#include <map>
#include <mutex>
#include <set>
#include <string>
#include <thread>
#include <vector>
using u32=uint32_t; using u64=uint64_t; using s64=int64_t; using u8=uint8_t;
enum {CORE_RUNNING_CPU,CORE_STEPPING_CPU,CORE_RUNTIME_ERROR,CORE_POWERDOWN};
int coreState=CORE_STEPPING_CPU;
namespace CoreTiming { u64 GetTicks(){return 0;} }
u64 usToCycles(int n){return n;}
struct PointerWrap { int Section(const char*,int,int){return 2;} };
template<class T> void Do(PointerWrap&,T&){}
#define ERROR_LOG_REPORT(...) ((void)0)
struct Mips { void InvalidateICache(u32,int){assert(false && "fence must not consume results");} } mips;
auto currentMIPS=&mips;
struct FileSystem {
 std::mutex mutex; std::condition_variable cv; bool entered=false,released=false;
 int writes=0;
 s64 ReadFile(u32 handle,u8* buf,size_t bytes,int& usec){
  std::unique_lock lock(mutex);entered=true;cv.notify_all();cv.wait(lock,[&]{return released;});
  if(handle==99)return -1;
  std::fill(buf,buf+bytes,0xa5);++writes;usec=7;return bytes;
 }
 s64 WriteFile(u32,const u8*,size_t bytes,int&){return bytes;}
 void WaitEntered(){std::unique_lock lock(mutex);cv.wait(lock,[&]{return entered;});}
 void Release(){std::lock_guard lock(mutex);released=true;cv.notify_all();}
} pspFileSystem;
'''
compose = r'''
AsyncIOManager manager;
unsigned fenceCalls=0;
u64 __IoBeginMemoryFence(){++fenceCalls;return manager.BeginMemoryFence();}
bool __IoMemoryFenceComplete(u64 f){return manager.MemoryFenceComplete(f);}
struct StepCommand { bool pending=false; bool empty()const{return !pending;} } g_cpuStepCommand;
bool Core_IsActive(){return coreState==CORE_RUNNING_CPU;}
'''
tests = r'''
int main(){
 manager.SetThreadEnabled(true);
 std::vector<u8> ram(64,0);
 AsyncIOEvent ev=IO_EVENT_READ;ev.handle=1;ev.buf=ram.data();ev.bytes=ram.size();ev.invalidateAddr=0x08800000;
 manager.ScheduleOperation(ev);
 std::thread worker([]{manager.RunEventsUntil(0);});
 pspFileSystem.WaitEntered();
 assert(!manager.HasEvents()); // The last event has been popped, yet is still writing.
 assert(!Core_EmucapPollMemoryPark());assert(fenceCalls==1);
 unsigned copies=0;
 auto batch=[&]{return Core_EmucapWithHaltedCPU([&](u64){++copies;assert(ram[0]==0xa5 && ram.back()==0xa5);return true;});};
 for(int i=0;i<1000;++i){assert(!Core_EmucapMemoryParked());assert(!batch());}
 assert(copies==0 && fenceCalls==1); // observation never drains/issues extra work
 assert(!manager.HasResult(1));
 // A successful load callback is not a memory-park proof.  Exercise the actual
 // debugger-thread completion wait while the last native read remains in flight.
 assert(!WaitForLoadedMemoryPark(std::chrono::steady_clock::now()+std::chrono::milliseconds(5)));
 assert(!Core_EmucapMemoryParked() && fenceCalls==1 && !manager.HasResult(1));
 std::atomic<bool> loadReturned{false};
 bool loadSucceeded=false;
 std::thread loadWaiter([&]{
   loadSucceeded=WaitForLoadedMemoryPark(std::chrono::steady_clock::now()+std::chrono::seconds(2));
   loadReturned.store(true,std::memory_order_release);
 });
 Core_EmucapInvalidateMemoryPark(); // superseding generation before old fence completion
 pspFileSystem.Release();worker.join();
 assert(manager.HasResult(1));assert(manager.HasOperation(1));
 assert(!Core_EmucapMemoryParked());assert(!batch());
 assert(!loadReturned.load(std::memory_order_acquire));
 assert(!Core_EmucapPollMemoryPark());assert(fenceCalls==2);
 std::thread next([]{manager.RunEventsUntil(0);});next.join();
 assert(Core_EmucapPollMemoryPark());assert(Core_EmucapMemoryParked());
 loadWaiter.join();assert(loadSucceeded && loadReturned.load());
 const unsigned calls=fenceCalls;assert(batch());assert(copies==1 && fenceCalls==calls);
 // A frozen CPU continues host service under the step mutex. Contention is not
 // evidence of running guest work; the read must serialize with the same park.
 for(bool invalidate : {false,true}) {
   std::unique_lock<std::recursive_mutex> owner(g_stepMutex);
   std::promise<void> entering;
   auto entered=entering.get_future();
   auto reader=std::async(std::launch::async,[&]{entering.set_value();return batch();});
   entered.wait();
   assert(reader.wait_for(std::chrono::milliseconds(100))==std::future_status::timeout);
   if(invalidate) Core_EmucapInvalidateMemoryPark();
   owner.unlock();
   assert(reader.get()==!invalidate);
 }
 assert(copies==2 && fenceCalls==calls);
 assert(!Core_EmucapPollMemoryPark());
 std::thread repark([]{manager.RunEventsUntil(0);});repark.join();
 assert(Core_EmucapPollMemoryPark());

 assert(manager.HasResult(1) && manager.HasOperation(1)); // no result consumption or guest notification
 g_cpuStepCommand.pending=true;assert(!batch());g_cpuStepCommand.pending=false;
 // Multiple writes, including an I/O error, must publish results before their fence.
 Core_EmucapInvalidateMemoryPark();
 ev.handle=2;manager.ScheduleOperation(ev);ev.handle=99;manager.ScheduleOperation(ev);
 assert(!Core_EmucapPollMemoryPark());
 std::thread multiple([]{manager.RunEventsUntil(0);});multiple.join();
 assert(Core_EmucapPollMemoryPark());assert(manager.HasResult(2)&&manager.HasResult(99));
 assert(batch());assert(pspFileSystem.writes==2);
 // Lifecycle invalidation plus powerdown cannot be revived by an already completed fence.
 Core_EmucapInvalidateMemoryPark();coreState=CORE_POWERDOWN;assert(!batch());
 assert(!WaitForLoadedMemoryPark(std::chrono::steady_clock::now()+std::chrono::seconds(2)));
 manager.Shutdown();assert(!manager.HasResult(1));
}
'''
with tempfile.TemporaryDirectory(prefix='psp-memory-writers-') as td:
    td = Path(td)
    source = td/'probe.cpp'; source.write_text(support+header+io+compose+globals_+functions+tests)
    binary=td/'probe'
    subprocess.run(['c++','-std=c++17','-O1','-g','-pthread','-fsanitize=address,undefined',str(source),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True,timeout=10)
print('PSP actual I/O queue: popped writer exclusion, late fence, zero-drain batch, results retained, errors and lifecycle invalidation passed')
