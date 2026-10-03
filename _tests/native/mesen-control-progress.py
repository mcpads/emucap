#!/usr/bin/env python3
"""Check maintained native control observation with deterministic host time.

This tests clock/stop provenance, generation fencing and service admission. Actual
CPU/PPU tick attribution and live scheduling require the separate runtime witness.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True, help='patched Mesen Core directory')
args = parser.parse_args()
header = (args.source / 'Debugger/Debugger.h').read_text()
source = (args.source / 'Debugger/Debugger.cpp').read_text()
state = header[header.index('struct AgentControlState'):header.index('\nclass Debugger\n')]
fields = header[header.index('\tstatic atomic<uint64_t> _nextAgentGeneration;'):header.index('\n\tvoid Reset();')]
methods = source[source.index('atomic<uint64_t> Debugger::_nextAgentGeneration'):source.index('bool Debugger::IsPaused()')]
code = r'''
#include <atomic>
#include <cassert>
#include <chrono>
#include <cstdint>
#include <cstdio>
using std::atomic;
struct TestClock {
 using rep = int64_t;
 using period = std::milli;
 using duration = std::chrono::milliseconds;
 using time_point = std::chrono::time_point<TestClock>;
 static constexpr bool is_steady = true;
 static int64_t ms;
 static time_point now() { return time_point(duration(ms)); }
};
int64_t TestClock::ms = 0;
enum class StepType { Step, PpuFrame };
enum class EventType { ControlIdle };
struct StepRequest { int32_t StepCount = 7, PpuStepCount = 123; StepType Type = StepType::PpuFrame; };
struct MainDebugger { StepRequest request; StepRequest* GetStepRequest() { return &request; } };
struct Scripts { bool enabled = true; bool HasEventCallback(EventType) { return enabled; } };
''' + state + r'''
class Debugger {
public:
''' + fields + r'''
 Scripts scripts;
 Scripts* _scriptManager = &scripts;
 MainDebugger main;
 int _mainCpuType = 0;
 bool _executionStopped = false, _waitForBreakResume = false;
 uint32_t _suspendRequestCount = 0;
 int callbacks = 0;
 MainDebugger* GetMainDebugger() { return &main; }
 void ProcessEvent(EventType, int) { callbacks++; }
 AgentControlState GetAgentControlState();
};
''' + methods + r'''
int main() {
 Debugger a, b;
 auto first = a.GetAgentControlState();
 assert(first.Generation != 0 && b.GetAgentControlState().Generation != first.Generation);
 assert(!first.Halted);
 a._executionStopped = true;
 assert(!a.GetAgentControlState().Halted); // stop flag alone is insufficient
 a._waitForBreakResume = true;
 assert(a.GetAgentControlState().Halted);
 a._waitForBreakResume = false;
 assert(!a.GetAgentControlState().Halted); // scheduled resume revokes proof
 a._waitForBreakResume = true;
 a._suspendRequestCount = 1;
 assert(!a.GetAgentControlState().Halted);
 a._suspendRequestCount = 0;
 a._agentInstructionBoundaries = 19; a._agentHaltedCpuSteps = 4; a._agentPpuCycles = 91;
 TestClock::ms = 205;
 auto s = a.GetAgentControlState();
 assert(s.InstructionBoundaries == 19 && s.HaltedCpuSteps == 4 && s.PpuCycles == 91 && s.MonotonicMs == 205);
 assert(s.InstructionRemaining == 7 && s.PpuRemaining == 123 && s.Type == StepType::PpuFrame);
 a.main.request.StepCount = 0;
 assert(s.InstructionRemaining == 7); // observation is a value, not mutable proof
 assert(a.GetAgentControlState().Generation == s.Generation);
 assert(a.GetAgentControlState().InstructionBoundaries == 19); // reads do not advance
 for(int i = 0; i < 256; i++) a.ServiceAgentControl();
 assert(a.callbacks == 1);
 TestClock::ms += 9;
 for(int i = 0; i < 256; i++) a.ServiceAgentControl();
 assert(a.callbacks == 1);
 TestClock::ms += 1;
 for(int i = 0; i < 256; i++) a.ServiceAgentControl();
 assert(a.callbacks == 2);
 a.scripts.enabled = false;
 TestClock::ms += 100;
 for(int i = 0; i < 256; i++) a.ServiceAgentControl();
 assert(a.callbacks == 2);
 assert(a.GetAgentControlState().PpuCycles == 91); // service does not invent progress
 puts("Mesen native control clock, halt and service tests passed");
}
'''
code = code.replace('std::chrono::steady_clock', 'TestClock')
with tempfile.TemporaryDirectory(prefix='mesen-control-test-') as tmp:
    directory = Path(tmp)
    (directory / 'test.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++17', '-Wall', '-Wextra', '-Werror',
                    '-fsanitize=address,undefined', str(directory / 'test.cpp'),
                    '-o', str(directory / 'test')], check=True)
    subprocess.run([str(directory / 'test')], check=True)
