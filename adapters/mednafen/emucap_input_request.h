#pragma once
#include "emucap_control_wire.h"
#include <cctype>
#include <cmath>

namespace EmucapControl {
struct InputRequest {
  std::uint16_t mask = 0;
  long frames = 1;
};

// Parse before recording an input obligation or touching native state. Lookup
// uses the active core's existing button table; no second mapping is maintained.
template <typename Lookup>
bool ReadInputRequest(const Json& params, bool timed, long max_frames, Lookup lookup,
                      const char* system, InputRequest& output, std::string& error) {
  error.clear();
  if (!params.is_object()) { error = "input params must be an object"; return false; }
  if (params.contains("port") && (!params["port"].is_number_integer() || params["port"] != 0)) {
    error = "Mednafen input requires integer controller port 0";
    return false;
  }
  InputRequest parsed;
  if (timed && params.contains("frames")) {
    if (!params["frames"].is_number()) {
      error = "frames must be a positive integer";
      return false;
    }
    const double count = params["frames"].get<double>();
    if (!std::isfinite(count) || count < 1 || count > max_frames || std::floor(count) != count) {
      error = "frames must be an integer from 1 to " + std::to_string(max_frames);
      return false;
    }
    parsed.frames = static_cast<long>(count);
  }
  if (params.contains("buttons")) {
    if (!params["buttons"].is_array()) { error = "buttons must be an array of strings"; return false; }
    for (const auto& button : params["buttons"]) {
      if (!button.is_string()) { error = "buttons must be an array of strings"; return false; }
      std::string name = button.get<std::string>();
      for (char& c : name) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
      std::uint16_t bit = 0;
      if (!lookup(name, bit)) {
        error = std::string("unsupported ") + system + " button: " + name;
        return false;
      }
      parsed.mask |= bit;
    }
  }
  output = parsed;
  return true;
}
} // namespace EmucapControl
