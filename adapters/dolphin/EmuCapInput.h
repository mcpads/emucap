// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <cstdint>
#include <string>

#include <picojson.h>

namespace EmuCap::Input
{
// Wii Remote core-button bits from the pinned Dolphin Wiimote protocol model.
constexpr uint16_t WII_PAD_LEFT = 0x0001;
constexpr uint16_t WII_PAD_RIGHT = 0x0002;
constexpr uint16_t WII_PAD_DOWN = 0x0004;
constexpr uint16_t WII_PAD_UP = 0x0008;
constexpr uint16_t WII_BUTTON_PLUS = 0x0010;
constexpr uint16_t WII_BUTTON_TWO = 0x0100;
constexpr uint16_t WII_BUTTON_ONE = 0x0200;
constexpr uint16_t WII_BUTTON_B = 0x0400;
constexpr uint16_t WII_BUTTON_A = 0x0800;
constexpr uint16_t WII_BUTTON_MINUS = 0x1000;
constexpr uint16_t WII_BUTTON_HOME = 0x8000;

struct WiiOverride
{
  bool engaged = false;
  uint16_t buttons = 0;

  bool operator==(const WiiOverride&) const = default;
};

struct ApplyResult
{
  bool ok = false;
  bool engaged = false;
  std::string error;
};

// Validate the complete request before replacing state. A failed result leaves state unchanged.
ApplyResult ApplyWiiRequest(const picojson::object& request, WiiOverride& state);
}  // namespace EmuCap::Input
