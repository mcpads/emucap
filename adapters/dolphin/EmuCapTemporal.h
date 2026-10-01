// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <future>
#include <mutex>
#include <utility>

namespace EmuCap::Temporal
{
inline constexpr uint64_t MAX_ADVANCE_COUNT = 5000;
inline constexpr auto OPERATION_BUDGET = std::chrono::seconds(250);

inline bool ValidAdvanceCount(uint64_t count)
{
  return count > 0 && count <= MAX_ADVANCE_COUNT;
}

enum class HostJobOutcome { Completed, Cancelled, Unverified };

// One queued mutation and its deadline share this state. A caller may leave while Execute is
// running, so both the caller and the queued closure retain a shared_ptr to this object.
class HostJob
{
public:
  using Clock = std::chrono::steady_clock;
  explicit HostJob(Clock::time_point deadline) : m_deadline(deadline) {}

  template <typename Admit, typename Work>
  void Execute(Admit admit, Work work)
  {
    {
      std::lock_guard lock(m_mutex);
      if (m_phase != Phase::Queued)
        return;
      if (Clock::now() >= m_deadline || !admit())
      {
        m_phase = Phase::Cancelled;
        m_changed.notify_all();
        return;
      }
      m_phase = Phase::Applying;
    }
    const bool completed = work();
    std::lock_guard lock(m_mutex);
    if (m_phase == Phase::Applying)
      m_phase = completed ? Phase::Completed : Phase::Unverified;
    m_changed.notify_all();
  }

  HostJobOutcome Wait()
  {
    std::unique_lock lock(m_mutex);
    m_changed.wait_until(lock, m_deadline, [&] {
      return m_phase != Phase::Queued && m_phase != Phase::Applying;
    });
    // Completion wins if it became visible before this synchronized deadline decision.
    if (m_phase == Phase::Completed)
      return HostJobOutcome::Completed;
    if (m_phase == Phase::Queued)
      m_phase = Phase::Cancelled;
    if (m_phase == Phase::Cancelled)
      return HostJobOutcome::Cancelled;
    m_phase = Phase::Unverified;
    return HostJobOutcome::Unverified;
  }

private:
  enum class Phase { Queued, Applying, Completed, Cancelled, Unverified };
  const Clock::time_point m_deadline;
  std::mutex m_mutex;
  std::condition_variable m_changed;
  Phase m_phase = Phase::Queued;
};

// Only the caller writes responses. The worker owns guest execution and cleanup; even after
// transport failure its terminal result is joined before another session can start.
template <typename Work, typename Progress, typename Cancel>
auto RunWithProgress(Work work, Progress progress, Cancel cancel,
                     std::chrono::milliseconds interval = std::chrono::seconds(1))
{
  auto result = std::async(std::launch::async, std::move(work));
  bool connected = true;
  while (result.wait_for(interval) != std::future_status::ready)
  {
    if (connected && !progress())
    {
      connected = false;
      cancel();
    }
  }
  return result.get();
}
}  // namespace EmuCap::Temporal
