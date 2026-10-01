#pragma once

// Pure request parsing and reply encoding for batched memory reads and agent pacing.
//
// Mednafen paces guest time with CurGameSpeed: the driver's real-time syncer (sound off) or the
// audio queue (sound on) waits for host time scaled by it. The fast/slow-forward keys replace it
// while held; the agent's value is the base speed used when neither key is active.

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

#include "emucap_json_num.h"

struct EmucapBatchRange {
  std::string memory_type;
  std::uint64_t address = 0;
  std::uint64_t length = 0;
};

const std::size_t EMUCAP_BATCH_MAX_RANGES = 64;
const std::uint64_t EMUCAP_BATCH_MAX_BYTES = 65536;
const std::uint32_t EMUCAP_PACING_MIN_PERCENT = 1;
const std::uint32_t EMUCAP_PACING_MAX_PERCENT = 10000;

// Host playback uses the native slow-forward floor. The guest-time governor still uses the
// requested rate; its real-time syncer supplies the remaining wait below this audio ratio.
inline double emucap_audio_ratio(double speed) {
  return speed < 0.25 ? 0.25 : speed;
}

// Plain string value for key inside one flat JSON object; escapes are rejected.
inline bool emucap_flat_json_string(const std::string& object, const char* key, std::string& out) {
  const std::string pattern = std::string("\"") + key + "\"";
  std::size_t pos = object.find(pattern);
  if (pos == std::string::npos) return false;
  pos = object.find(':', pos + pattern.size());
  if (pos == std::string::npos) return false;
  pos++;
  while (pos < object.size() && object[pos] == ' ') pos++;
  if (pos >= object.size() || object[pos] != '"') return false;
  const std::size_t begin = ++pos;
  while (pos < object.size() && object[pos] != '"') {
    if (object[pos] == '\\') return false;
    pos++;
  }
  if (pos >= object.size()) return false;
  out = object.substr(begin, pos - begin);
  return true;
}

// "ranges":[{memory_type,address,length},...] with flat objects, in request order.
inline bool emucap_parse_batch_ranges(
    const std::string& line,
    std::vector<EmucapBatchRange>& out,
    std::string& error) {
  out.clear();
  std::size_t pos = line.find("\"ranges\"");
  if (pos == std::string::npos) { error = "ranges is required"; return false; }
  pos = line.find('[', pos);
  if (pos == std::string::npos) { error = "ranges must be an array"; return false; }
  pos++;
  for (;;) {
    while (pos < line.size() && (line[pos] == ' ' || line[pos] == ',')) pos++;
    if (pos >= line.size()) { error = "ranges array is unterminated"; return false; }
    if (line[pos] == ']') break;
    if (line[pos] != '{') { error = "ranges entries must be objects"; return false; }
    const std::size_t end = line.find('}', pos);
    if (end == std::string::npos) { error = "range object is unterminated"; return false; }
    const std::string object = line.substr(pos, end - pos + 1);
    EmucapBatchRange range;
    if (!emucap_flat_json_string(object, "memory_type", range.memory_type)
        || range.memory_type.empty()) {
      error = "range " + std::to_string(out.size()) + " memory_type must be a string";
      return false;
    }
    if (emucap_json_u64(object, "address", range.address) != EmucapJsonNumberStatus::valid
        || emucap_json_u64(object, "length", range.length) != EmucapJsonNumberStatus::valid) {
      error = "range " + std::to_string(out.size()) + " needs integer address and length";
      return false;
    }
    if (range.length == 0) {
      error = "range " + std::to_string(out.size()) + " length must be positive";
      return false;
    }
    out.push_back(range);
    if (out.size() > EMUCAP_BATCH_MAX_RANGES) { error = "ranges must contain 1..64 entries"; return false; }
    pos = end + 1;
  }
  if (out.empty()) { error = "ranges must contain 1..64 entries"; return false; }
  std::uint64_t total = 0;
  for (const EmucapBatchRange& range : out) {
    if (range.length > EMUCAP_BATCH_MAX_BYTES - total) {
      error = "ranges exceed 65536 bytes";
      return false;
    }
    total += range.length;
  }
  return true;
}

struct EmucapPacingRequest {
  bool query = false;
  bool unlimited = false;
  std::uint32_t percent = 0;
};

// mode/percent from the request; percent is an integer percent inside the domain.
inline bool emucap_parse_pacing_request(
    const std::string& line,
    EmucapPacingRequest& request,
    std::string& error) {
  std::string mode;
  const bool has_mode = emucap_flat_json_string(line, "mode", mode);
  std::uint64_t percent = 0;
  const EmucapJsonNumberStatus status = emucap_json_u64(line, "percent", percent);
  if (!has_mode && status == EmucapJsonNumberStatus::absent) {
    request.query = true;
    return true;
  }
  if (has_mode && mode == "unlimited" && status == EmucapJsonNumberStatus::absent) {
    request.unlimited = true;
    return true;
  }
  if (has_mode && mode == "limited" && status == EmucapJsonNumberStatus::valid
      && percent >= EMUCAP_PACING_MIN_PERCENT && percent <= EMUCAP_PACING_MAX_PERCENT) {
    request.percent = static_cast<std::uint32_t>(percent);
    return true;
  }
  error = "use limited with an integer percent in 1..10000, unlimited without percent, or omit both";
  return false;
}

// Native state that determines the effective policy.
struct EmucapPacingObservation {
  double base = 1.0;       // agent base speed (multiplier)
  bool unlimited = false;  // agent unlimited mode: no real-time wait
  int ffsf = 0;            // 1 fast-forward held, 2 slow-forward held
  double current = 1.0;    // CurGameSpeed
  bool netplay = false;
  bool unthrottled = false;  // sound off with the native nothrottle setting: no real-time wait
};

inline std::string emucap_pacing_key(const EmucapPacingObservation& o) {
  char buf[128];
  std::snprintf(buf, sizeof(buf), "%.17g|%d|%d|%.17g|%d|%d", o.base, o.unlimited ? 1 : 0, o.ffsf,
                o.current, o.netplay ? 1 : 0, o.unthrottled ? 1 : 0);
  return buf;
}

// Common policy object; a held key, netplay, nothrottle or an unexpected native speed is custom.
inline std::string emucap_pacing_policy_json(
    const EmucapPacingObservation& o,
    std::uint64_t revision,
    bool audio) {
  std::string mode = "custom";
  std::string percent = "null";
  if (o.ffsf == 0 && !o.netplay) {
    if (o.unlimited) {
      mode = "unlimited";
    } else if (!o.unthrottled && o.current == o.base) {
      const double scaled = o.base * 100.0;
      const double rounded = std::round(scaled);
      if (std::fabs(scaled - rounded) < 1e-9 && rounded >= EMUCAP_PACING_MIN_PERCENT
          && rounded <= EMUCAP_PACING_MAX_PERCENT) {
        mode = "limited";
        percent = std::to_string(static_cast<unsigned>(rounded));
      }
    }
  }
  char diag[256];
  std::snprintf(diag, sizeof(diag),
                "{\"base_speed\":%.17g,\"unlimited\":%s,\"fast_slow_forward\":%d,"
                "\"current_speed\":%.17g,\"netplay\":%s,\"nothrottle\":%s,"
                "\"host_audio_ratio\":%.17g}",
                o.base, o.unlimited ? "true" : "false", o.ffsf, o.current,
                o.netplay ? "true" : "false", o.unthrottled ? "true" : "false",
                emucap_audio_ratio(o.current));
  return "{\"mode\":\"" + mode + "\",\"percent\":" + percent
      + ",\"source\":\"native\",\"policy_revision\":\"" + std::to_string(revision)
      + "\",\"host_constraints\":" + (audio ? "[\"audio_output\"]" : "[]")
      + ",\"diagnostics\":" + diag + "}";
}

inline std::string emucap_pacing_capability_json(bool audio) {
  return std::string("{\"modes\":[\"limited\",\"unlimited\"],")
      + "\"percent\":{\"min\":1,\"max\":10000,\"quantum\":1},"
      + "\"states\":[\"running\",\"frozen\"],\"scope\":\"host_pacing\",\"source\":\"native\","
      + "\"control_service_ms\":20,\"host_constraints\":" + (audio ? "[\"audio_output\"]" : "[]")
      + "}";
}

// True when the base speed confirms the request with no held override.
inline bool emucap_pacing_confirms(const EmucapPacingRequest& request, const EmucapPacingObservation& o) {
  if (o.ffsf != 0 || o.netplay) return false;
  if (request.unlimited) return o.unlimited;
  const double scaled = o.base * 100.0;
  return !o.unlimited && !o.unthrottled
      && std::fabs(scaled - static_cast<double>(request.percent)) < 1e-9 && o.current == o.base;
}
