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
  check(!emucap_parse_batch_ranges("{\"params\":{\"ranges\":[]}}", ranges, error), "empty ranges");
  check(!emucap_parse_batch_ranges(
            "{\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":1.5,\"length\":2}]}}", ranges, error),
        "fractional address");
  check(!emucap_parse_batch_ranges(
            "{\"params\":{\"ranges\":[{\"memory_type\":\"ram\",\"address\":0,\"length\":65537}]}}", ranges, error),
        "oversized batch");

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
  check(emucap_parse_pacing_request("{\"params\":{\"mode\":\"limited\",\"percent\":37}}", request, error)
            && request.percent == 37,
        "limited");
  request = {};
  check(emucap_parse_pacing_request("{\"params\":{\"mode\":\"unlimited\"}}", request, error)
            && request.unlimited,
        "unlimited");
  const char* invalid[] = {
      "{\"params\":{\"mode\":\"limited\",\"percent\":0}}",
      "{\"params\":{\"mode\":\"limited\",\"percent\":33.333}}",
      "{\"params\":{\"mode\":\"limited\",\"percent\":10001}}",
      "{\"params\":{\"mode\":\"unlimited\",\"percent\":100}}",
      "{\"params\":{\"percent\":50}}",
  };
  for (const char* line : invalid) {
    request = {};
    check(!emucap_parse_pacing_request(line, request, error), line);
  }

  EmucapPacingObservation o;
  o.percent = 37;
  check(emucap_pacing_policy_json(o, 2).find("\"mode\":\"limited\",\"percent\":37") != std::string::npos,
        "limited policy");
  EmucapPacingRequest thirty_seven;
  thirty_seven.percent = 37;
  check(emucap_pacing_confirms(thirty_seven, o), "confirm 37 percent");
  o.fast_forward = true;
  check(emucap_pacing_policy_json(o, 2).find("\"mode\":\"unlimited\",\"percent\":null") != std::string::npos,
        "fast-forward reads back as unlimited");
  check(!emucap_pacing_confirms(thirty_seven, o), "fast-forward is not confirmed");
  o.fast_forward = false;
  o.mute_audio = true;
  check(emucap_pacing_policy_json(o, 2).find("\"mode\":\"custom\",\"percent\":null") != std::string::npos,
        "unpaced native mute is custom");
  o.mute_audio = false;
  o.audio = true;
  check(!emucap_pacing_device_wait(o), "37 percent uses the host clock");
  o.percent = 100;
  check(emucap_pacing_device_wait(o), "audible 100 percent uses the device");
  check(emucap_pacing_policy_json(o, 3).find("[\"audio_output\"]") != std::string::npos,
        "audio constraint");

  EmucapSamplePacer pacer;
  const std::int64_t chunk_ns = 512LL * 1000000000LL / EMUCAP_AICA_RATE;
  check(pacer.deadline_after(512, 100, 1000) == 1000 + chunk_ns, "first chunk anchors now");
  check(pacer.deadline_after(512, 100, 2000) == 1000 + 2 * chunk_ns, "chunks accumulate");
  const std::int64_t slow = pacer.deadline_after(512, 1, 3000);
  check(slow == 1000 + 2 * chunk_ns + 512LL * 1000000000LL * 100 / EMUCAP_AICA_RATE,
        "1 percent is 100 chunk periods");
  const std::int64_t late = slow + EMUCAP_PACING_CATCHUP_NS + 1;
  check(pacer.deadline_after(512, 100, late) == late + chunk_ns, "a late host re-anchors");
  pacer.reanchor();
  check(pacer.deadline_after(512, 400, 5) == 5 + 512LL * 1000000000LL * 100 / (EMUCAP_AICA_RATE * 400),
        "reanchor and 400 percent");
  std::puts("ALL FLYCAST PACING TESTS PASSED");
  return 0;
}
