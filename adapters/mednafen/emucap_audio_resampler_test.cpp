// Native regression witness; compile with the pinned source's sound/Fir_Resampler.cpp.
#include "emucap_pacing.h"
#include "Fir_Resampler.h"
#include <algorithm>
#include <cassert>
#include <cstdio>
#include <vector>

static std::vector<short> streamed(double ratio, int chunk, bool mono) {
  Fir_Resampler<16> r;
  assert(!r.buffer_size(44100));
  r.time_ratio(ratio, 0.9965);
  std::vector<short> result;
  std::vector<short> output(44100);
  constexpr int total = 882 * 2 * 12;
  for (int offset = 0; offset < total;) {
    const int count = std::min(chunk, total - offset);
    assert(r.max_write() >= count);
    for (int i = 0; i < count; ++i) r.buffer()[i] = ((offset + i) % 64) * 100;
    r.write(count);
    const int available = r.avail();
    assert(available >= 0 && available <= 44100);
    const int read = mono ? r.read_mono_hack(output.data(), available)
                          : r.read(output.data(), available);
    assert(read >= 0 && read <= available);
    result.insert(result.end(), output.begin(), output.begin() + read);
    offset += count;
  }
  return result;
}

int main() {
  Fir_Resampler<16> native;
  // The old driver passed this zero-consumption ratio to avail(), which cannot terminate.
  assert(native.time_ratio(0.01, 0.9965) == 0.0);
  for (unsigned percent : {1u, 2u, 3u, 5u, 10u, 24u, 25u, 26u, 50u, 100u, 200u, 400u,
                           1500u, 1600u, 1601u, 9999u, 10000u}) {
    Fir_Resampler<16> resampler;
    assert(!resampler.buffer_size(44100));
    assert(resampler.time_ratio(emucap_audio_ratio(percent / 100.0), 0.9965) > 0.0);
    // A stereo 50-Hz frame at 44.1 kHz fits the native 500-ms output buffer even at the floor.
    constexpr int samples = 882 * 2;
    assert(resampler.max_write() >= samples);
    for (int i = 0; i < samples; ++i) resampler.buffer()[i] = (i % 64) * 100;
    resampler.write(samples);
    const int available = resampler.avail();
    assert(available >= 0 && available <= 44100);
    std::vector<short> output(44100);
    const int read = resampler.read(output.data(), available);
    assert(read > 0 && read <= available);
    const double ratio = emucap_audio_ratio(percent / 100.0);
    const auto stereo = streamed(ratio, 1764, false);
    const auto mono = streamed(ratio, 1764, true);
    assert(!stereo.empty() && stereo.size() == mono.size() * 2);
    for (size_t i = 0; i < mono.size(); ++i) assert(mono[i] == stereo[i * 2]);
    for (int chunk : {2, 34, 4096}) {
      assert(streamed(ratio, chunk, false) == stereo);
      assert(streamed(ratio, chunk, true) == mono);
    }
  }
  std::puts("MEDNAFEN NATIVE AUDIO RESAMPLER PASSED");
}
