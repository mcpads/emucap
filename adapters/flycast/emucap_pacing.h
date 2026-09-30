#pragma once

// Pure request parsing, reply encoding and the sample pacing clock for agent pacing and batched
// reads.
//
// Flycast paces the guest only through its audio backend: each 512-sample AICA chunk is pushed
// with a blocking wait. The maintained hook replaces that wait with a host clock over generated
// samples (44.1 kHz of guest time), so every percent is paced the same way whether or not an audio
// device exists. Limited 100 percent with audible sound keeps the device wait, since the device
// then runs at the guest rate.

#include <cctype>
#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

const std::uint32_t EMUCAP_PACING_MIN_PERCENT = 1;
const std::uint32_t EMUCAP_PACING_MAX_PERCENT = 10000;
const std::size_t EMUCAP_BATCH_MAX_RANGES = 64;
const std::uint64_t EMUCAP_BATCH_MAX_BYTES = 65536;
const std::int64_t EMUCAP_AICA_RATE = 44100;
// A host that falls this far behind the clock (a slow interpreter or a stop) re-anchors instead
// of running fast to catch up.
const std::int64_t EMUCAP_PACING_CATCHUP_NS = 67000000;

enum class EmucapNumber { absent, valid, invalid };

// Non-negative integer value for key; fractions, signs and trailing text are invalid.
inline EmucapNumber emucap_json_uint(const std::string& text, const char* key, std::uint64_t& out) {
  const std::string pattern = std::string("\"") + key + "\"";
  const std::size_t key_pos = text.find(pattern);
  if (key_pos == std::string::npos) return EmucapNumber::absent;
  std::size_t pos = text.find(':', key_pos + pattern.size());
  if (pos == std::string::npos) return EmucapNumber::invalid;
  pos++;
  while (pos < text.size() && std::isspace(static_cast<unsigned char>(text[pos]))) pos++;
  if (pos >= text.size() || !std::isdigit(static_cast<unsigned char>(text[pos])))
    return EmucapNumber::invalid;
  const char* begin = text.c_str() + pos;
  char* end = nullptr;
  errno = 0;
  const unsigned long long parsed = std::strtoull(begin, &end, 10);
  if (end == begin || errno == ERANGE) return EmucapNumber::invalid;
  while (*end != '\0' && std::isspace(static_cast<unsigned char>(*end))) end++;
  if (*end != ',' && *end != '}' && *end != ']' && *end != '\0') return EmucapNumber::invalid;
  out = parsed;
  return EmucapNumber::valid;
}

// Plain string value for key inside one flat JSON object; escapes are rejected.
inline bool emucap_json_plain_string(const std::string& object, const char* key, std::string& out) {
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

struct EmucapBatchRange {
  std::string memory_type;
  std::uint64_t address = 0;
  std::uint64_t length = 0;
};

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
    const std::string index = std::to_string(out.size());
    if (!emucap_json_plain_string(object, "memory_type", range.memory_type)
        || range.memory_type.empty()) {
      error = "range " + index + " memory_type must be a string";
      return false;
    }
    if (emucap_json_uint(object, "address", range.address) != EmucapNumber::valid
        || emucap_json_uint(object, "length", range.length) != EmucapNumber::valid) {
      error = "range " + index + " needs integer address and length";
      return false;
    }
    if (range.length == 0) { error = "range " + index + " length must be positive"; return false; }
    out.push_back(range);
    if (out.size() > EMUCAP_BATCH_MAX_RANGES) { error = "ranges must contain 1..64 entries"; return false; }
    pos = end + 1;
  }
  if (out.empty()) { error = "ranges must contain 1..64 entries"; return false; }
  std::uint64_t total = 0;
  for (const EmucapBatchRange& range : out) {
    total += range.length;
    if (total > EMUCAP_BATCH_MAX_BYTES) { error = "ranges exceed 65536 bytes"; return false; }
  }
  return true;
}

struct EmucapPacingRequest {
  bool query = false;
  bool unlimited = false;
  std::uint32_t percent = 0;
};

inline bool emucap_parse_pacing_request(
    const std::string& line,
    EmucapPacingRequest& request,
    std::string& error) {
  std::string mode;
  const bool has_mode = emucap_json_plain_string(line, "mode", mode);
  std::uint64_t percent = 0;
  const EmucapNumber status = emucap_json_uint(line, "percent", percent);
  if (!has_mode && status == EmucapNumber::absent) {
    request.query = true;
    return true;
  }
  if (has_mode && mode == "unlimited" && status == EmucapNumber::absent) {
    request.unlimited = true;
    return true;
  }
  if (has_mode && mode == "limited" && status == EmucapNumber::valid
      && percent >= EMUCAP_PACING_MIN_PERCENT && percent <= EMUCAP_PACING_MAX_PERCENT) {
    request.percent = static_cast<std::uint32_t>(percent);
    return true;
  }
  error = "use limited with an integer percent in 1..10000, unlimited without percent, or omit both";
  return false;
}

// Native state that determines the effective policy.
struct EmucapPacingObservation {
  std::uint32_t percent = 100;  // agent target
  bool unlimited = false;       // agent unlimited
  bool fast_forward = false;    // Flycast fast-forward: no samples are pushed or paced
  bool mute_audio = false;      // netplay/test automation: samples are neither pushed nor paced
  bool audio = false;           // audible host audio
};

// The audio device paces only an audible limited 100 percent.
inline bool emucap_pacing_device_wait(const EmucapPacingObservation& o) {
  return o.audio && !o.unlimited && o.percent == 100;
}

inline std::string emucap_pacing_key(const EmucapPacingObservation& o) {
  char buf[96];
  std::snprintf(buf, sizeof(buf), "%u|%d|%d|%d|%d", o.percent, o.unlimited ? 1 : 0,
                o.fast_forward ? 1 : 0, o.mute_audio ? 1 : 0, o.audio ? 1 : 0);
  return buf;
}

inline std::string emucap_pacing_policy_json(const EmucapPacingObservation& o, std::uint64_t revision) {
  std::string mode = "custom";
  std::string percent = "null";
  if (!o.mute_audio) {
    if (o.fast_forward || o.unlimited) {
      mode = "unlimited";
    } else {
      mode = "limited";
      percent = std::to_string(o.percent);
    }
  }
  char diag[192];
  std::snprintf(diag, sizeof(diag),
                "{\"agent_percent\":%u,\"agent_unlimited\":%s,\"fast_forward\":%s,"
                "\"mute_audio\":%s,\"audio_device_wait\":%s}",
                o.percent, o.unlimited ? "true" : "false", o.fast_forward ? "true" : "false",
                o.mute_audio ? "true" : "false", emucap_pacing_device_wait(o) ? "true" : "false");
  return "{\"mode\":\"" + mode + "\",\"percent\":" + percent
      + ",\"source\":\"native\",\"policy_revision\":\"" + std::to_string(revision)
      + "\",\"host_constraints\":" + (o.audio ? "[\"audio_output\"]" : "[]")
      + ",\"diagnostics\":" + diag + "}";
}

inline std::string emucap_pacing_capability_json(bool audio) {
  return std::string("{\"modes\":[\"limited\",\"unlimited\"],")
      + "\"percent\":{\"min\":1,\"max\":10000,\"quantum\":1},"
      + "\"states\":[\"running\",\"frozen\"],\"scope\":\"host_pacing\",\"source\":\"native\","
      + "\"control_service_ms\":20,\"host_constraints\":" + (audio ? "[\"audio_output\"]" : "[]")
      + "}";
}

inline bool emucap_pacing_confirms(const EmucapPacingRequest& request, const EmucapPacingObservation& o) {
  if (o.fast_forward || o.mute_audio) return false;
  if (request.unlimited) return o.unlimited;
  return !o.unlimited && o.percent == request.percent;
}

// Host deadline for generated guest samples at a target percent.
class EmucapSamplePacer {
 public:
  void reanchor() { anchored_ = false; }

  // Host time at which a chunk of samples generated from now may be released.
  std::int64_t deadline_after(std::uint32_t samples, std::uint32_t percent, std::int64_t now_ns) {
    if (!anchored_ || now_ns - deadline_ > EMUCAP_PACING_CATCHUP_NS) {
      deadline_ = now_ns;
      anchored_ = true;
    }
    deadline_ += static_cast<std::int64_t>(samples) * 1000000000LL * 100
        / (EMUCAP_AICA_RATE * static_cast<std::int64_t>(percent));
    return deadline_;
  }

 private:
  bool anchored_ = false;
  std::int64_t deadline_ = 0;
};
