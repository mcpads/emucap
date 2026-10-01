// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <atomic>
namespace openmsx {
struct Pump { void poll() {} void execute() {} };
struct Reactor {
  Pump pump;
  std::atomic<unsigned> wakes{0};
  Pump& getInputEventGenerator() { return pump; }
  Pump& getInterpreter() { return pump; }
  Pump& getRTScheduler() { return pump; }
  void enterMainLoop() { ++wakes; }
};
}
