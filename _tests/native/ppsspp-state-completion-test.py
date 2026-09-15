#!/usr/bin/env python3
"""Exercise the native WebSocket state handler's completion postcondition.

Pass SaveStateSubscriber.cpp from the pinned, extracted tree. The fake queue
completes on invocation; this tests completion policy, not EmuThread scheduling.
"""

import argparse
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    source = args.source.read_text()
    source = source[source.index("namespace {"):source.index("}  // namespace") + len("}  // namespace")]
    support = r'''
#include <cassert>
#include <chrono>
#include <condition_variable>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
enum { CORE_RUNNING_CPU, CORE_STEPPING_CPU, CORE_STEPPING_GE };
static int coreState, resumes, operations;
enum class BreakReason { SavestateLoad, SavestateSave };
enum class BootState { Complete };
static BootState PSP_GetBootState() { return BootState::Complete; }
static void Core_Break(BreakReason, int) { if (coreState == CORE_RUNNING_CPU) coreState = CORE_STEPPING_CPU; }
static void Core_Resume() { ++resumes; coreState = CORE_RUNNING_CPU; }
struct Path { explicit Path(const std::string &) {} };
struct JsonWriter {
  std::map<std::string, std::string> fields;
  void writeString(const char *key, const std::string &value) { fields[key] = value; }
};
struct DebuggerRequest {
  bool failed = false, responded = false;
  JsonWriter json;
  void Fail(const std::string &) { failed = true; }
  bool ParamString(const char *, std::string *out) { *out = "checkpoint.ppst"; return true; }
  JsonWriter &Respond() { responded = true; return json; }
};
namespace SaveState {
enum class Status { FAILURE, SUCCESS };
using Callback = std::function<void(Status, std::string_view, std::string_view)>;
static Status result = Status::SUCCESS;
static void Save(const Path &, int, Callback callback) { ++operations; callback(result, "", ""); }
static void Load(const Path &, int, Callback callback) { ++operations; callback(result, "", ""); }
}
'''
    checks = r'''
int main() {
  for (int initial : {CORE_RUNNING_CPU, CORE_STEPPING_CPU}) {
    coreState = initial; resumes = 0;
    DebuggerRequest request;
    RunSaveStateOperation(request, false, BreakReason::SavestateLoad);
    assert(request.responded && !request.failed);
    assert(coreState == CORE_STEPPING_CPU && resumes == 0);
    assert(request.json.fields["state"] == "frozen");
  }
  coreState = CORE_RUNNING_CPU; resumes = 0;
  DebuggerRequest save;
  RunSaveStateOperation(save, true, BreakReason::SavestateSave);
  assert(save.responded && coreState == CORE_RUNNING_CPU && resumes == 1);
  SaveState::result = SaveState::Status::FAILURE;
  coreState = CORE_RUNNING_CPU; resumes = 0;
  DebuggerRequest failure;
  RunSaveStateOperation(failure, false, BreakReason::SavestateLoad);
  assert(failure.failed && !failure.responded && resumes == 0);
  coreState = CORE_STEPPING_GE; operations = 0;
  DebuggerRequest unsupported;
  RunSaveStateOperation(unsupported, false, BreakReason::SavestateLoad);
  assert(unsupported.failed && operations == 0);
}
'''
    with tempfile.TemporaryDirectory(prefix="ppsspp-state-completion-") as directory:
        path = Path(directory)
        (path / "test.cpp").write_text(support + source + checks)
        subprocess.run(["c++", "-std=c++17", "-pthread", str(path / "test.cpp"), "-o", str(path / "test")], check=True)
        subprocess.run([str(path / "test")], check=True)
    print("PPSSPP state completion cases passed")


if __name__ == "__main__":
    main()
