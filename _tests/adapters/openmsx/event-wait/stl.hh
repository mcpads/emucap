// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <algorithm>
#include <functional>
namespace openmsx {
template<class Range, class Value, class Projection>
bool contains(Range& range, const Value& value, Projection projection) {
  return std::ranges::find(range, value, projection) != range.end();
}
template<class Range, class Value, class Projection>
auto rfind_unguarded(Range& range, const Value& value, Projection projection) {
  return std::ranges::find(range, value, projection);
}
}
