// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#include "EmuCapTemporal.h"
#include <atomic>
#include <cassert>
#include <condition_variable>
#include <mutex>
#include <thread>

int main()
{
  using namespace EmuCap::Temporal;
  using namespace std::chrono_literals;
  assert(ValidAdvanceCount(16));
  assert(ValidAdvanceCount(5000));
  assert(!ValidAdvanceCount(0));
  assert(!ValidAdvanceCount(5001));
  std::mutex mutex;
  std::condition_variable cv;
  bool progress_seen = false;
  bool cleaned = false;
  const int completed = RunWithProgress([&] {
    std::unique_lock lock(mutex);
    cv.wait(lock, [&] { return progress_seen; });
    cleaned = true;
    return 5000;
  }, [&] {
    std::lock_guard lock(mutex);
    assert(!cleaned);
    progress_seen = true;
    cv.notify_all();
    return true;
  }, [] { assert(false); }, 1ms);
  assert(completed == 5000 && cleaned);

  std::atomic<bool> cancelled{false};
  std::atomic<bool> terminal_cleanup{false};
  int attempts = 0;
  int cancellations = 0;
  const int partial = RunWithProgress([&] {
    while (!cancelled.load())
      std::this_thread::yield();
    std::this_thread::sleep_for(5ms);
    terminal_cleanup.store(true);
    return 17;
  }, [&] { ++attempts; return false; }, [&] { ++cancellations; cancelled.store(true); }, 1ms);
  assert(partial == 17 && terminal_cleanup.load());
  assert(attempts == 1 && cancellations == 1);
}
