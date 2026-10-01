#include "emucap_pacing.h"

#include <cstdio>
#include <cstdlib>

static void check(bool condition, const char* message) {
  if (!condition) {
    std::fprintf(stderr, "FAIL %s\n", message);
    std::exit(1);
  }
}

int main() {
  std::vector<EmucapBatchRange> ranges;
  std::string error;
  check(emucap_parse_batch_ranges(
            "{\"id\":1,\"method\":\"read_memory_batch\",\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":16,\"length\":2},{\"memory_type\":\"vram\",\"address\":0,\"length\":1}]}}",
            ranges, error),
        "valid ranges");
  check(ranges.size() == 2 && ranges[1].memory_type == "vram" && ranges[0].address == 16, "range order");
  const char* bad[] = {
      "{\"params\":{}}",
      "{\"params\":{\"ranges\":[]}}",
      "{\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":1}]}}",
      "{\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":1,\"length\":0}]}}",
      "{\"params\":{\"ranges\":[{\"address\":1,\"length\":1}]}}",
      "{\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":0,\"length\":65537}]}}",
  };
  for (const char* line : bad) check(!emucap_parse_batch_ranges(line, ranges, error), line);

  check(!emucap_parse_batch_ranges(
      "{\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":0,\"length\":1},{\"memory_type\":\"ram\",\"address\":0,\"length\":18446744073709551615}]}}",
      ranges, error), "aggregate length cannot wrap below the byte limit");

  check(emucap_parse_batch_ranges(
      "{\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":0,\"length\":65535},{\"memory_type\":\"ram\",\"address\":65535,\"length\":1}]}}",
      ranges, error), "aggregate at byte limit remains accepted");
  check(!emucap_parse_batch_ranges(
      "{\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":0,\"length\":65536},{\"memory_type\":\"ram\",\"address\":0,\"length\":1}]}}",
      ranges, error), "aggregate one byte beyond limit is rejected");

  EmucapPacingRequest request;
  check(emucap_parse_pacing_request("{\"params\":{}}", request, error) && request.query, "query");
  request = {};
  check(emucap_parse_pacing_request("{\"params\":{\"mode\":\"limited\",\"percent\":50}}", request, error)
            && request.percent == 50,
        "limited");
  request = {};
  check(emucap_parse_pacing_request("{\"params\":{\"mode\":\"unlimited\"}}", request, error)
            && request.unlimited,
        "unlimited");
  const char* bad_requests[] = {
      "{\"params\":{\"mode\":\"limited\",\"percent\":33.5}}",
      "{\"params\":{\"mode\":\"limited\",\"percent\":0}}",
      "{\"params\":{\"mode\":\"limited\",\"percent\":10001}}",
      "{\"params\":{\"mode\":\"limited\"}}",
      "{\"params\":{\"mode\":\"unlimited\",\"percent\":100}}",
      "{\"params\":{\"percent\":50}}",
  };
  for (const char* line : bad_requests) {
    request = {};
    check(!emucap_parse_pacing_request(line, request, error), line);
  }

  EmucapPacingObservation o;
  o.base = 0.07;
  o.current = 0.07;
  check(emucap_pacing_policy_json(o, 3, false).find("\"mode\":\"limited\",\"percent\":7") != std::string::npos,
        "limited policy");
  EmucapPacingRequest seven;
  seven.percent = 7;
  check(emucap_pacing_confirms(seven, o), "confirm 7 percent");
  check(emucap_pacing_policy_json(o, 3, true).find("\"host_audio_ratio\":0.25") != std::string::npos,
        "low-speed host audio floor is disclosed without changing the guest policy");
  for (unsigned percent = 1; percent <= EMUCAP_PACING_MAX_PERCENT; ++percent) {
    const double speed = percent / 100.0;
    check(emucap_audio_ratio(speed) >= 0.25, "host audio stays within native resampler domain");
    if (percent >= 25)
      check(emucap_audio_ratio(speed) == speed, "native-supported audio rates stay unchanged");
  }
  o.ffsf = 1;
  check(emucap_pacing_policy_json(o, 3, false).find("\"mode\":\"custom\",\"percent\":null") != std::string::npos,
        "held fast-forward is custom");
  check(!emucap_pacing_confirms(seven, o), "override is not confirmed");
  o.ffsf = 0;
  o.unthrottled = true;
  check(emucap_pacing_policy_json(o, 3, false).find("\"mode\":\"custom\",\"percent\":null") != std::string::npos,
        "nothrottle without sound is custom");
  check(!emucap_pacing_confirms(seven, o), "nothrottle is not confirmed");
  o.unthrottled = false;
  o.unlimited = true;
  check(emucap_pacing_policy_json(o, 4, true).find("\"mode\":\"unlimited\",\"percent\":null") != std::string::npos,
        "unlimited policy");
  check(emucap_pacing_policy_json(o, 4, true).find("[\"audio_output\"]") != std::string::npos,
        "audio constraint");
  std::puts("ALL MEDNAFEN PACING TESTS PASSED");
  return 0;
}
