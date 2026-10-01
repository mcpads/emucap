// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include "EventDistributor.hh"
namespace openmsx {
struct EventListener {
  unsigned delivered = 0;
  bool signalEvent(const Event&) { ++delivered; return false; }
};
}
