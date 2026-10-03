#!/usr/bin/env python3
"""Actual native CPU/DSP/FIFO park acquisition; controlled writers, no guest drain."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
native = root / 'adapters/dolphin/work/dolphin-src/Source/Core'
def function(file, signature):
    text = (native / file).read_text()
    start = text.index(signature); end = text.index('{', start) + 1; depth = 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}'); end += 1
    return text[start:end]
code = r'''
#include "Common/BlockingLoop.h"
#include <cassert>
#include <functional>
#include <future>
#include <queue>
#include <iostream>
#include <optional>
#include "picojson.h"
using u64=uint64_t;
namespace Common {template<class F> struct ScopeGuard {F f;ScopeGuard(F fn):f(fn){}~ScopeGuard(){f();}};}
namespace Core {thread_local bool cpu_thread=false;bool IsCPUThread(){return cpu_thread;}}
namespace CPU {
enum class State {Running,Stepping};
struct CPUManager {
 std::mutex m_stepping_lock,m_state_change_lock;
 State m_state=State::Stepping;
 bool m_state_cpu_thread_active=false,m_state_cpu_step_instruction=false;
 bool m_state_paused_and_locked=false,m_emucap_job_active=false;
 std::queue<int> m_pending_jobs;uint64_t m_emucap_memory_epoch=7;
 bool EmucapWithParkedCPU(const std::function<bool(uint64_t)>&, const char** = nullptr);
};
'''+function('Core/HW/CPU.cpp','bool CPUManager::EmucapWithParkedCPU(')+r'''
}
struct DSPLLE {
 std::mutex m_dsp_thread_mutex;std::atomic<int> m_cycle_count{0};
 bool EmucapTryLockIdle();void EmucapUnlockIdle();
};
'''+function('Core/HW/DSPLLE/DSPLLE.cpp','bool DSPLLE::EmucapTryLockIdle(')+function('Core/HW/DSPLLE/DSPLLE.cpp','void DSPLLE::EmucapUnlockIdle(')+r'''
struct FifoManager {
 Common::Flag m_emu_running_state;Common::BlockingLoop m_gpu_mainloop;
 bool EmucapMemoryIdle(const char** = nullptr) const;void EmulatorState(bool);
};
'''+function('VideoCommon/Fifo.cpp','bool FifoManager::EmucapMemoryIdle(')+function('VideoCommon/Fifo.cpp','void FifoManager::EmulatorState(')+r'''
namespace Core {
struct DSP {DSPLLE writer;DSPLLE*GetDSPEmulator(){return &writer;}};
struct System {
 CPU::CPUManager cpu;DSP dsp;FifoManager fifo;
 auto&GetCPU(){return cpu;}auto&GetDSP(){return dsp;}auto&GetFifo(){return fifo;}
};
u64 EmucapWithParkedMemory(System&, u64, const std::function<void()>&, const char** = nullptr);
'''+function('Core/Core.cpp','u64 EmucapWithParkedMemory(')+r'''
}
std::atomic<bool> s_stop{false},s_request_cancelled{false},s_control_retired{false};int errors=0;
std::mutex s_cancel_mutex;
std::optional<std::chrono::steady_clock::time_point> s_cancel_origin;
constexpr auto OWNED_STOP_BUDGET=std::chrono::seconds(5);
void Fail(const char*,const char*){++errors;}
'''+function(str(root/'adapters/dolphin/EmuCap.cpp'),'bool AwaitMemoryPark(')+function(str(root/'adapters/dolphin/EmuCap.cpp'),'bool VerifyFrozenPublication(')+r'''
int main(){
 Core::System system;int reads=0;
 picojson::object frozen{{"state",picojson::value(std::string("frozen"))}};
 assert(VerifyFrozenPublication(system,frozen,false));
 system.cpu.m_state_cpu_thread_active=true;
 assert(!VerifyFrozenPublication(system,frozen,false) && errors==1);
 s_stop=true;
 assert(!VerifyFrozenPublication(system,frozen,true) && errors==2 && s_control_retired);
 s_control_retired=false;
 s_stop=false;system.cpu.m_state_cpu_thread_active=false;
 auto read=[&](u64 epoch=0){return Core::EmucapWithParkedMemory(system,epoch,[&]{++reads;});};
 assert(read()==7 && reads==1);
 // Admission cannot complete a queued CPU instruction or host job.
 system.cpu.m_state_cpu_step_instruction=true;assert(!read() && reads==1);
 const char* reason=nullptr;
 assert(!Core::EmucapWithParkedMemory(system,0,[]{assert(false);},&reason));
 assert(std::string(reason)=="cpu_pending_step");
 system.cpu.m_state_cpu_step_instruction=false;system.cpu.m_pending_jobs.push(1);assert(!read());
 system.cpu.m_pending_jobs.pop();system.cpu.m_state_cpu_thread_active=true;assert(!read());
 system.cpu.m_state_cpu_thread_active=false;system.cpu.m_emucap_job_active=true;assert(!read());
 Core::cpu_thread=true;assert(read()==7);Core::cpu_thread=false;system.cpu.m_emucap_job_active=false;
 // Same clock does not admit a receipt from before a CPU mutation.
 auto receipt=read();++system.cpu.m_emucap_memory_epoch;auto before=reads;
 assert(!read(receipt) && reads==before);
 // Queued DSP cycles remain untouched by a rejected observation.
 system.dsp.writer.m_cycle_count=4;assert(!read() && system.dsp.writer.m_cycle_count==4);
 system.dsp.writer.m_cycle_count=0;
 Common::Event dsp_entered,dsp_release;std::atomic<int> stores{0};
 std::thread dsp([&]{std::lock_guard lock(system.dsp.writer.m_dsp_thread_mutex);dsp_entered.Set();dsp_release.Wait();++stores;});
 dsp_entered.Wait();before=reads;assert(!read() && reads==before && stores==0);
 dsp_release.Set();dsp.join();assert(read());
 Common::Event gpu_entered,gpu_release;
 system.fifo.m_emu_running_state.Set();system.fifo.m_gpu_mainloop.Prepare();
 std::thread gpu([&]{system.fifo.m_gpu_mainloop.Run([&]{if(!system.fifo.m_emu_running_state.IsSet())return;gpu_entered.Set();gpu_release.Wait();++stores;});});
 gpu_entered.Wait();system.fifo.EmulatorState(false);
 // Old CPU-only publication had returned here, before the actual writer store.
 before=reads;assert(stores==1 && !read() && reads==before && stores==1);
 gpu_release.Set();system.fifo.m_gpu_mainloop.Wait();assert(stores==2 && read());
 system.fifo.m_gpu_mainloop.Stop();gpu.join();
 std::cout<<"Dolphin actual CPU/DSP/FIFO authority: active/queued writers reject without drain; same-epoch acquisition and CPU-job ownership pass\n";
}
'''
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory)/'probe.cpp'; binary = Path(directory)/'probe'; source.write_text(code)
    subprocess.run(['clang++','-std=c++20','-pthread','-fsanitize=address,undefined','-I',str(native),'-I',str(next((native.parents[2]).rglob('picojson.h')).parent),str(source),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)
