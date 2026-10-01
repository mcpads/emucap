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

  // A timeout before dispatch must not overwrite a later human setting when the queue drains.
  int speed = 100;
  HostJob delayed(HostJob::Clock::now());
  assert(delayed.Wait() == HostJobOutcome::Cancelled);
  speed = 200;
  delayed.Execute([] { return true; }, [&] { speed = 400; return true; });
  assert(speed == 200);

  // Expiration is also enforced by dispatch when the waiting caller has not run yet.
  HostJob expired(HostJob::Clock::now());
  expired.Execute([] { return true; }, [&] { speed = 400; return true; });
  assert(expired.Wait() == HostJobOutcome::Cancelled && speed == 200);

  HostJob stale(HostJob::Clock::now() + 1s);
  stale.Execute([] { return false; }, [&] { speed = 400; return true; });
  assert(stale.Wait() == HostJobOutcome::Cancelled && speed == 200);

  HostJob completed_job(HostJob::Clock::now() + 1s);
  completed_job.Execute([] { return true; }, [&] { speed = 400; return true; });
  assert(completed_job.Wait() == HostJobOutcome::Completed && speed == 400);
  completed_job.Execute([] { return true; }, [&] { speed = 800; return true; });
  assert(speed == 400);  // duplicate dispatch cannot apply the request twice

  // Hold the work beyond the deadline without blocking the caller's synchronized decision.
  auto active = std::make_shared<HostJob>(HostJob::Clock::now() + 1s);
  std::promise<void> entered;
  std::promise<void> release;
  auto released = release.get_future();
  std::thread host([active, &entered, &released] {
    active->Execute([] { return true; }, [&] {
      entered.set_value();
      released.wait();
      return true;
    });
  });
  assert(entered.get_future().wait_for(1s) == std::future_status::ready);
  assert(active->Wait() == HostJobOutcome::Unverified);
  release.set_value();
  host.join();
  assert(active->Wait() == HostJobOutcome::Unverified); // late completion cannot restore health

  HostJob finished_before_deadline(HostJob::Clock::now() + 10ms);
  finished_before_deadline.Execute([] { return true; }, [] { return true; });
  std::this_thread::sleep_for(15ms);
  assert(finished_before_deadline.Wait() == HostJobOutcome::Completed);

  HostJob failed(HostJob::Clock::now() + 1s);
  failed.Execute([] { return true; }, [] { return false; });
  assert(failed.Wait() == HostJobOutcome::Unverified);
}
