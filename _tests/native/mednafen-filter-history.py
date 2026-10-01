#!/usr/bin/env python3
"""Check native deinterlacer history across field/geometry changes and malformed blocks."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
source = args.source.resolve()
adapter = Path(__file__).resolve().parents[2] / 'adapters/mednafen'
code = r'''
#include <mednafen/mednafen.h>
#include <mednafen/MemoryStream.h>
#include <mednafen/video/Deinterlacer.h>
#include <cassert>
#include <vector>
using namespace Mednafen;

std::vector<uint8> save(Deinterlacer& filter) {
  MemoryStream stream;
  filter.SaveHistory(&stream);
  return {stream.map(), stream.map() + stream.map_size()};
}
std::unique_ptr<Deinterlacer> restore(unsigned mode, const std::vector<uint8>& data,
                                     unsigned max_width = 8, unsigned max_height = 8) {
  MemoryStream stream;
  if (!data.empty()) stream.write(data.data(), data.size());
  stream.rewind();
  return Deinterlacer::RestoreHistory(mode, &stream, max_width, max_height);
}
std::vector<uint32> process(Deinterlacer& filter, uint64 tag, bool field, unsigned n) {
  MDFN_Surface surface(nullptr, 8, 8, 10, MDFN_PixelFormat(tag));
  int32 widths[8];
  const int x_offset = n % 3;
  MDFN_Rect rect = {x_offset, 0, 0, n % 4 == 2 ? 4 : 8};
  for (unsigned y = 0; y < 8; ++y) {
    widths[y] = 8 - x_offset - (y % 3 == 1);
    for (unsigned x = 0; x < 8; ++x) {
      uint32 pixel = surface.MakeColor((n * 17 + x * 25) & 255, y * 30, field ? 210 : 30, 255);
      if (surface.format.opp == 2) surface.pixels16[y * 10 + x] = uint16(pixel);
      else surface.pixels[y * 10 + x] = pixel;
    }
  }
  if (n % 3 == 1) { widths[0] = -1; rect.w = 8 - x_offset; }
  filter.Process(&surface, rect, widths, field);
  std::vector<uint32> output{uint32(rect.x), uint32(rect.y), uint32(rect.w), uint32(rect.h)};
  for (const auto width : widths) output.push_back(uint32(width));
  for (unsigned y = 0; y < 8; ++y)
    for (unsigned x = 0; x < 8; ++x)
      output.push_back(surface.format.opp == 2 ? surface.pixels16[y * 10 + x]
                                             : surface.pixels[y * 10 + x]);
  return output;
}
int main() {
  const uint64 formats[] = {MDFN_PixelFormat::ABGR32_8888,
                           MDFN_PixelFormat::IRGB16_1555, MDFN_PixelFormat::RGB16_565};
  for (unsigned mode = 0; mode <= Deinterlacer::DEINT_BLEND_RG; ++mode)
    for (const auto format : formats)
      for (bool first_field : {false, true}) {
        std::unique_ptr<Deinterlacer> reference(Deinterlacer::Create(mode));
        const auto empty = save(*reference);
        auto cold = restore(mode, empty);
        assert(save(*cold) == empty);
        assert(process(*reference, format, first_field, 0) == process(*cold, format, first_field, 0));
        const auto origin = save(*reference);
        // A destination filter already contains unrelated field history.
        process(*cold, format, !first_field, 21);
        auto candidate = restore(mode, origin);
        assert(save(*candidate) == origin);
        cold.swap(candidate);
        for (unsigned n = 1; n <= 12; ++n) {
          assert(process(*reference, format, first_field ^ bool(n & 1), n)
              == process(*cold, format, first_field ^ bool(n & 1), n));
          const auto snapshot = save(*reference);
          auto staged = restore(mode, snapshot);
          assert(save(*staged) == snapshot);
          cold.swap(staged);
        }
        reference->ClearState();
        auto cleared = restore(mode, save(*reference));
        assert(process(*reference, format, first_field, 13) == process(*cleared, format, first_field, 13));

        const auto unchanged = save(*reference);
        auto reject = [&](const std::vector<uint8>& bad, unsigned expected_mode = 99,
                          unsigned max_width = 8, unsigned max_height = 8) {
          bool rejected = false;
          try { auto staged = restore(expected_mode == 99 ? mode : expected_mode, bad, max_width, max_height);
                reference.swap(staged); }
          catch (const std::exception&) { rejected = true; }
          assert(rejected && save(*reference) == unchanged);
        };
        for (size_t length = 0; length < origin.size(); ++length)
          reject({origin.begin(), origin.begin() + length});
        auto bad = origin; bad.push_back(0); reject(bad);
        bad = origin; bad[0] = 'X'; reject(bad);
        bad = origin; bad[12] = 2; reject(bad);
        reject(origin, (mode + 1) % 5);
        reject(origin, 5);
        reject(origin, 99, 0, 8);
        reject(origin, 99, 8, 0);
        if (mode >= Deinterlacer::DEINT_WEAVE) {
          const unsigned surface_start = mode == Deinterlacer::DEINT_WEAVE ? 32 : 20;
          bad = origin; bad[surface_start + 4] = 0; reject(bad);
          bad = origin; bad[surface_start + 8] = 100; reject(bad);
          bad = origin; bad[surface_start + 19] = 1; reject(bad);
          bad = origin; bad[bad.size() - 4] = bad[bad.size() - 3] = bad[bad.size() - 2] = bad.back() = 0;
          reject(bad);
          reject(origin, 99, 7, 8);
          reject(origin, 99, 8, 7);
        }
      }
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-filter-history-') as temp:
    cpp = Path(temp) / 'test.cpp'
    executable = Path(temp) / 'test'
    cpp.write_text(code)
    units = ['video/surface.cpp', 'video/convert.cpp', 'video/Deinterlacer.cpp',
             'video/Deinterlacer_Simple.cpp', 'video/Deinterlacer_Blend.cpp',
             'Stream.cpp', 'MemoryStream.cpp', 'error.cpp']
    platform_libs = ['-liconv', '-framework', 'CoreFoundation'] if sys.platform == 'darwin' else []
    subprocess.run([
        'clang++', '-std=c++11', '-DHAVE_CONFIG_H', '-O1',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I' + str(source / 'include'), '-I' + str(source / 'intl'), '-I' + str(adapter),
        str(cpp), *[str(source / 'src' / name) for name in units],
        str(source / 'src/libtrio.a'), str(source / 'intl/libintl.a'), *platform_libs,
        '-o', str(executable),
    ], check=True)
    subprocess.run([str(executable)], check=True,
                   env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1'))
print('Native deinterlacer history: five modes, three formats, both field origins, '
      '12 continuations, clear-state and malformed histories pass ASan/UBSan')
