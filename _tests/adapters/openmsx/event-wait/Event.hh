// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
namespace openmsx {
enum class EventType { Command, Unobserved, NUM_EVENT_TYPES };
struct Event { EventType type; };
inline EventType getType(const Event& event) { return event.type; }
}
