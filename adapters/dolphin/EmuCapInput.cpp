// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later

#include "EmuCapInput.h"

#include <cctype>
#include <cmath>
#include <string_view>

namespace EmuCap::Input
{
namespace
{
ApplyResult Reject(std::string message)
{
  return {.ok = false, .engaged = false, .error = std::move(message)};
}

bool WiiButtonBit(std::string_view name, uint16_t& bit)
{
  std::string normalized;
  normalized.reserve(name.size());
  for (const unsigned char character : name)
    normalized.push_back(static_cast<char>(std::tolower(character)));

  if (normalized == "a")
    bit = WII_BUTTON_A;
  else if (normalized == "b")
    bit = WII_BUTTON_B;
  else if (normalized == "one")
    bit = WII_BUTTON_ONE;
  else if (normalized == "two")
    bit = WII_BUTTON_TWO;
  else if (normalized == "minus")
    bit = WII_BUTTON_MINUS;
  else if (normalized == "plus")
    bit = WII_BUTTON_PLUS;
  else if (normalized == "home")
    bit = WII_BUTTON_HOME;
  else if (normalized == "up")
    bit = WII_PAD_UP;
  else if (normalized == "down")
    bit = WII_PAD_DOWN;
  else if (normalized == "left")
    bit = WII_PAD_LEFT;
  else if (normalized == "right")
    bit = WII_PAD_RIGHT;
  else
    return false;
  return true;
}

ApplyResult ValidatePort(const picojson::object& request)
{
  const auto port = request.find("port");
  const auto pad = request.find("pad");
  if (port != request.end() && pad != request.end())
    return Reject("use either port or pad, not both");
  const auto field = port != request.end() ? port : pad;
  if (field == request.end())
    return {.ok = true, .engaged = false, .error = {}};
  if (!field->second.is<double>())
    return Reject("Wii Remote port must be an integer");
  const double value = field->second.get<double>();
  if (!std::isfinite(value) || std::floor(value) != value)
    return Reject("Wii Remote port must be an integer");
  if (value != 0)
    return Reject("only Wii Remote port 0 is supported");
  return {.ok = true, .engaged = false, .error = {}};
}
}  // namespace

ApplyResult ApplyWiiRequest(const picojson::object& request, WiiOverride& state)
{
  const ApplyResult port = ValidatePort(request);
  if (!port.ok)
    return port;

  for (const char* field :
       {"stickX", "stickY", "substickX", "substickY", "triggerL", "triggerR"})
  {
    if (request.contains(field))
      return Reject(std::string(field) + " is not part of the Wii Remote core-button surface");
  }

  bool release = false;
  if (const auto engaged = request.find("engaged"); engaged != request.end())
  {
    if (!engaged->second.is<bool>())
      return Reject("engaged must be a boolean");
    release = !engaged->second.get<bool>();
  }

  const auto buttons = request.find("buttons");
  if (buttons == request.end())
  {
    if (!release)
      return Reject("Wii set_input requires buttons or engaged=false");
  }
  else
  {
    if (!buttons->second.is<picojson::array>())
      return Reject("buttons must be an array");
    release = release || buttons->second.get<picojson::array>().empty();
  }

  WiiOverride next;
  if (!release)
  {
    for (const picojson::value& button : buttons->second.get<picojson::array>())
    {
      if (!button.is<std::string>())
        return Reject("buttons must contain strings");
      uint16_t bit = 0;
      const std::string& name = button.get<std::string>();
      if (!WiiButtonBit(name, bit))
        return Reject("unsupported Wii Remote core button: " + name);
      next.buttons |= bit;
    }
    next.engaged = true;
  }

  state = next;
  return {.ok = true, .engaged = state.engaged, .error = {}};
}
}  // namespace EmuCap::Input
