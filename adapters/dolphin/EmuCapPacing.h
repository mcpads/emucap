// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <array>
#include <cstdint>

#include "Common/Config/ConfigInfo.h"

namespace EmuCap::Pacing
{
struct Observation
{
  float speed = 0;
  bool temp_disabled = false;
  std::array<uint64_t, 3> revision{};
  bool operator==(const Observation&) const = default;
};

enum class Outcome { Completed, Busy, Cancelled, Unverified };
struct Change
{
  Outcome outcome = Outcome::Unverified;
  Observation previous;
  Observation applied;
  Observation verified;
};

// Read and write run under the native settings gate. Notifications run at scope exit, before
// final verification. The caller publishes this result only after the queued job has completed.
template <typename Admit, typename Read, typename Write>
Change Apply(float target, Admit admit, Read read, Write write)
{
  Change result;
  {
    Config::SettingsTransaction transaction(std::try_to_lock);
    if (!transaction.OwnsLock())
    {
      result.outcome = Outcome::Busy;
      return result;
    }
    if (!admit())
    {
      result.outcome = Outcome::Cancelled;
      return result;
    }
    result.previous = read();
    write(target);
    result.applied = read();
  }
  {
    Config::SettingsTransaction transaction(std::try_to_lock);
    if (!transaction.OwnsLock())
      return result;
    result.verified = read();
    if (admit() && result.verified == result.applied && result.verified.speed == target &&
        !result.verified.temp_disabled)
      result.outcome = Outcome::Completed;
  }
  return result;
}
}  // namespace EmuCap::Pacing
