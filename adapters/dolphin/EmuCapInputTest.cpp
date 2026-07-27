// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later

#include "EmuCapInput.h"

#include <cassert>
#include <string>

namespace
{
picojson::object Request(std::initializer_list<const char*> buttons)
{
  picojson::array values;
  for (const char* button : buttons)
    values.emplace_back(std::string(button));
  return {{"buttons", picojson::value(values)}};
}

void AssertRejectedWithoutMutation(picojson::object request)
{
  EmuCap::Input::WiiOverride state{true, EmuCap::Input::WII_BUTTON_A};
  const auto before = state;
  const auto result = EmuCap::Input::ApplyWiiRequest(request, state);
  assert(!result.ok);
  assert(state == before);
}
}  // namespace

int main()
{
  using namespace EmuCap::Input;

  WiiOverride state;
  const auto all = ApplyWiiRequest(
      Request({"a", "b", "one", "two", "minus", "plus", "home", "up", "down", "left",
               "right"}),
      state);
  assert(all.ok);
  assert(all.engaged);
  assert(state.engaged);
  assert(state.buttons == static_cast<uint16_t>(
                              WII_BUTTON_A | WII_BUTTON_B | WII_BUTTON_ONE | WII_BUTTON_TWO |
                              WII_BUTTON_MINUS | WII_BUTTON_PLUS | WII_BUTTON_HOME | WII_PAD_UP |
                              WII_PAD_DOWN | WII_PAD_LEFT | WII_PAD_RIGHT));

  const auto replace = ApplyWiiRequest(Request({"two", "right"}), state);
  assert(replace.ok);
  assert(state.engaged);
  assert(state.buttons == static_cast<uint16_t>(WII_BUTTON_TWO | WII_PAD_RIGHT));

  const auto release = ApplyWiiRequest(Request({}), state);
  assert(release.ok);
  assert(!release.engaged);
  assert(!state.engaged);
  assert(state.buttons == 0);

  state = {true, WII_BUTTON_B};
  picojson::object explicit_release{{"engaged", picojson::value(false)}};
  const auto released = ApplyWiiRequest(explicit_release, state);
  assert(released.ok);
  assert(!state.engaged);

  picojson::object bad_type{{"buttons", picojson::value(std::string("a"))}};
  AssertRejectedWithoutMutation(bad_type);

  picojson::array non_string;
  non_string.emplace_back(1.0);
  AssertRejectedWithoutMutation({{"buttons", picojson::value(non_string)}});
  AssertRejectedWithoutMutation(Request({"x"}));
  AssertRejectedWithoutMutation({{"buttons", picojson::value(picojson::array{})},
                                 {"port", picojson::value(1.0)}});
  AssertRejectedWithoutMutation({{"buttons", picojson::value(picojson::array{})},
                                 {"port", picojson::value(0.5)}});
  AssertRejectedWithoutMutation({{"buttons", picojson::value(picojson::array{})},
                                 {"pad", picojson::value(std::string("0"))}});
  AssertRejectedWithoutMutation({{"buttons", picojson::value(picojson::array{})},
                                 {"port", picojson::value(0.0)},
                                 {"pad", picojson::value(0.0)}});
  AssertRejectedWithoutMutation({{"buttons", picojson::value(picojson::array{})},
                                 {"stickX", picojson::value(128.0)}});
  AssertRejectedWithoutMutation({{"engaged", picojson::value(true)}});
  AssertRejectedWithoutMutation({{"engaged", picojson::value(std::string("false"))}});

  return 0;
}
