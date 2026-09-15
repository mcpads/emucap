// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <chrono>
#include <cstdint>
#include <future>
#include <utility>

namespace EmuCap::Temporal
{
inline constexpr uint64_t MAX_ADVANCE_COUNT = 5000;
inline constexpr auto OPERATION_BUDGET = std::chrono::seconds(250);

inline bool ValidAdvanceCount(uint64_t count)
{
  return count > 0 && count <= MAX_ADVANCE_COUNT;
}

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
