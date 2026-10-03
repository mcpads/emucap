#!/usr/bin/env python3
"""Exercise the maintained Dolphin frame cleanup closure with delayed host dispatch."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/dolphin/EmuCap.cpp').read_text()
start = source.index('  auto stop_deadline =', source.index('struct FrameStart'))
end = source.index('\n\n  const auto run =', start)
closure = source[start:end]
code = r'''
#include "EmuCapTemporal.h"
#include <algorithm>
#include <atomic>
#include <cassert>
#include <functional>
#include <optional>
#include <vector>
using namespace std::chrono_literals;
namespace Temporal = EmuCap::Temporal;
std::atomic<bool> s_control_retired{false};
std::mutex s_cancel_mutex;
std::optional<std::chrono::steady_clock::time_point> s_cancel_origin;
constexpr auto STEP_WAIT_SLICE = 2ms;
bool dispatch = true, freeze = true;
int mutations = 0;
namespace Core {
struct System {};
enum class State { Running, Paused };
State state = State::Running;
System system;
std::vector<std::function<void(System&)>> jobs;
void QueueHostJob(std::function<void(System&)> work) {
 if (dispatch) work(system); else jobs.push_back(std::move(work));
}
void CancelFrameStep(System&) { ++mutations; if (freeze) state = State::Paused; }
State GetState(System&) { return state; }
}
struct SafeAccess { explicit SafeAccess(Core::System&) {} };
int main() {
 const auto operation_deadline = Temporal::HostJob::Clock::now() + std::chrono::hours(1);
''' + closure + r'''
 assert(cancel_frame_step());
 assert(mutations == 1 && !s_control_retired);
 const auto original_deadline = stop_deadline;
 assert(cancel_frame_step());
 assert(stop_deadline == original_deadline);
 dispatch = false;
 assert(!cancel_frame_step() && s_control_retired);
 const int before = mutations;
 Core::state = Core::State::Running;
 for (auto& work : Core::jobs) work(Core::system);
 assert(mutations == before && Core::state == Core::State::Running);
 assert(!cancel_frame_step() && mutations == before);
 // Even a callback that ran cannot certify cleanup from an unsuccessful pause.
 s_control_retired = false; dispatch = true; freeze = false;
 assert(!cancel_frame_step() && s_control_retired);
 s_control_retired = false; freeze = true;
 s_cancel_origin = Temporal::HostJob::Clock::now() - std::chrono::seconds(6);
 const int before_expired_cancel = mutations;
 assert(!cancel_frame_step() && s_control_retired);
 assert(mutations == before_expired_cancel);
}
'''
with tempfile.TemporaryDirectory(prefix='dolphin-cleanup-') as directory:
    path = Path(directory)
    (path / 'test.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++20', '-fno-exceptions', '-pthread', '-Wall',
                    '-Wextra', '-Werror', '-I' + str(root / 'adapters/dolphin'),
                    str(path / 'test.cpp'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
print('Dolphin native cleanup: deadline retention, late dispatch revocation and retirement passed')
