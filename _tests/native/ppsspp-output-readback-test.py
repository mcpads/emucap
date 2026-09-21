#!/usr/bin/env python3
"""Compile the applied native output mailbox with a controllable GPU.

Tests request ownership, cancellation, stop replacement, and failed readback.
This does not model graphics-driver servicing; real renderer checks are separate.
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
    source = source[source.index("// Output readback has request-owned storage:"):source.index("bool ProcessStepping() {")]
    support = r'''
#include <cassert>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <future>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>
using u32 = unsigned int;
static constexpr int GPU_DBG_FRAMEBUF_DISPLAY = 1;
#define _dbg_assert_(x) assert(x)
enum CoreState { CORE_RUNNING_CPU, CORE_STEPPING_CPU, CORE_STEPPING_GE, CORE_POWERDOWN };
static std::atomic<CoreState> coreState{CORE_STEPPING_CPU};
static std::atomic<int> cpuStop{1}, gpuStop{1};
static int Core_GetSteppingCounter() { return cpuStop; }
static int GetSteppingCounter() { return gpuStop; }
static const char *GetCurrentThreadName() { return "Test"; }
struct GPUDebugBuffer {
 std::vector<unsigned char> data;
 u32 stride = 0, height = 0;
 bool flipped = false, backbuffer = false;
 void Allocate(u32 w, u32 h, int, bool flip = false) { stride=w; height=h; flipped=flip; data.resize(w*h); }
 unsigned char *GetData() { return data.empty() ? nullptr : data.data(); }
 u32 GetStride() { return stride; }
 u32 GetHeight() { return height; }
 bool GetFlipped() { return flipped; }
 bool IsBackBuffer() { return backbuffer; }
 int GetFormat() { return 0; }
 u32 PixelSize() { return 1; }
};
struct GPU {
 bool succeed = true, empty = false, backbuffer = false;
 int reads = 0;
 std::mutex lock;
 std::condition_variable cv;
 bool block = false, entered = false, release = false;
 bool GetCurrentFramebuffer(GPUDebugBuffer &buffer, int type, int scale) {
  assert(type == GPU_DBG_FRAMEBUF_DISPLAY && scale == 1);
  std::unique_lock<std::mutex> guard(lock);
  ++reads; entered = true; cv.notify_all();
  cv.wait(guard, [&] { return !block || release; });
  buffer.Allocate(empty ? 0 : 512, empty ? 0 : 300, 0);
  std::fill(buffer.data.begin(), buffer.data.end(), 255);
  if (!empty) for (u32 y = 0; y < 272; ++y)
   std::fill(buffer.data.begin() + y * 512, buffer.data.begin() + y * 512 + 480, reads);
  buffer.backbuffer = backbuffer;
  return succeed;
 }
};
static GPU device;
static GPU *gpu = &device;
'''
    checks = r'''
static auto Submit() {
 auto future = std::async(std::launch::async, GPU_ReadDisplayFramebuffer);
 const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
 for (;;) {
  { std::lock_guard<std::mutex> guard(outputReadLock); if (outputRead) break; }
  assert(std::chrono::steady_clock::now() < deadline);
  std::this_thread::yield();
 }
 return future;
}
int main() {
 // Both halt types are read-only. Each result owns distinct storage.
 auto first = Submit(); ProcessOutputRead(); auto retained = first.get();
 assert(retained && retained->GetData()[0] == 1 && coreState == CORE_STEPPING_CPU);
 for (int i = 0; i < 20; ++i) {
  coreState = i % 2 ? CORE_STEPPING_GE : CORE_STEPPING_CPU;
  auto f = Submit(); ProcessOutputRead(); auto result = f.get();
  assert(result && result->GetData()[0] == i + 2 && retained->GetData()[0] == 1);
  assert(result->GetStride() == 480 && result->GetHeight() == 272);
  for (auto pixel : result->data) assert(pixel == i + 2);
 }
 // Failed GPU reads cannot return the previous successful buffer.
 device.succeed = false;
 auto failed = Submit(); ProcessOutputRead(); assert(!failed.get());
 device.succeed = true;
 device.empty = true;
 auto empty = Submit(); ProcessOutputRead(); assert(!empty.get());
 device.empty = false;
 device.backbuffer = true;
 auto swapchain = Submit(); ProcessOutputRead(); assert(!swapchain.get());
 device.backbuffer = false;
 gpu = nullptr;
 auto absent = Submit(); ProcessOutputRead(); assert(!absent.get());
 gpu = &device;
 // State replacement, including halt -> run -> a new halt, invalidates a request.
 coreState = CORE_STEPPING_CPU;
 int reads = device.reads;
 auto replaced = Submit(); ++cpuStop; ProcessOutputRead(); assert(!replaced.get());
 assert(device.reads == reads);
 auto shutdown = Submit(); coreState = CORE_POWERDOWN; ProcessOutputRead(); assert(!shutdown.get());
 assert(device.reads == reads);
 assert(!GPU_ReadDisplayFramebuffer());
 coreState = CORE_STEPPING_CPU;
 auto reset = Submit(); CancelOutputRead(); assert(!reset.get());
 ProcessOutputRead(); assert(device.reads == reads);
 // A stop change while the host read is executing also invalidates success.
 device.block = true; device.entered = false; device.release = false;
 auto changed = Submit(); std::thread changingWorker(ProcessOutputRead);
 { std::unique_lock<std::mutex> guard(device.lock); device.cv.wait(guard, [] { return device.entered; }); }
 ++cpuStop;
 { std::lock_guard<std::mutex> guard(device.lock); device.release = true; device.cv.notify_all(); }
 changingWorker.join(); assert(!changed.get());
 device.block = false; reads = device.reads;
 // Spurious notifications do not mean completion. Queued timeouts are discarded.
 auto queued = Submit(); outputReadWait.notify_all();
 assert(queued.wait_for(std::chrono::milliseconds(20)) == std::future_status::timeout);
 assert(!GPU_ReadDisplayFramebuffer());  // Busy admission cannot overwrite the first request.
 assert(!queued.get()); ProcessOutputRead(); assert(device.reads == reads);
 // A timed-out in-flight host read retains its own storage. Its late completion
 // cannot complete or overwrite a subsequent request.
 device.block = true; device.entered = false; device.release = false;
 auto slow = Submit();
 std::thread worker(ProcessOutputRead);
 { std::unique_lock<std::mutex> guard(device.lock); device.cv.wait(guard, [] { return device.entered; }); }
 assert(!slow.get());
 auto next = Submit();
 { std::lock_guard<std::mutex> guard(device.lock); device.release = true; device.cv.notify_all(); }
 worker.join();
 assert(next.wait_for(std::chrono::milliseconds(20)) == std::future_status::timeout);
 ProcessOutputRead(); auto recovered = next.get();
 assert(recovered && recovered->GetData()[0] == reads + 2 && retained->GetData()[0] == 1);
 assert(coreState == CORE_STEPPING_CPU);
}
'''
    with tempfile.TemporaryDirectory(prefix="ppsspp-output-readback-") as directory:
        path = Path(directory)
        (path / "test.cpp").write_text(support + source + checks)
        subprocess.run(["c++", "-std=c++17", "-pthread", "-fsanitize=address,undefined", str(path / "test.cpp"), "-o", str(path / "test")], check=True)
        subprocess.run([str(path / "test")], check=True, timeout=30)
    print("PPSSPP output ownership, stop replacement, read failure, busy, spurious wakeup, queued/in-flight timeout and recovery passed")


if __name__ == "__main__":
    main()
