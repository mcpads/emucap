#!/usr/bin/env python3
"""Check native temporal-blur history staging, continuation, rollback and configuration drift."""
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
#include <mednafen/video/tblur.h>
#include <cassert>
#include <vector>
namespace Mednafen {
// The tested formats are supported; an unexpected warning must fail the probe.
void MDFN_Notify(MDFN_NoticeType, const char*, ...) noexcept { abort(); }
}
using namespace Mednafen;

void initialize(unsigned mode, double amount, unsigned width = 8, unsigned height = 8) {
  TBlur_Kill();
  if (mode) TBlur_Init(mode == 2, amount, width, height);
}
std::vector<uint8> save() {
  MemoryStream stream;
  TBlur_SaveHistory(&stream);
  return {stream.map(), stream.map() + stream.map_size()};
}
struct FaultingStream : MemoryStream {
  int reads = 0, fail_at = -1;
  uint64 read(void* data, uint64 count, bool error_on_eos = true) override {
    if (reads++ == fail_at) throw std::runtime_error("injected history read failure");
    return MemoryStream::read(data, count, error_on_eos);
  }
};
std::unique_ptr<TBlurHistory> prepare(const std::vector<uint8>& bytes, int fail_at = -1) {
  FaultingStream stream;
  if (!bytes.empty()) stream.write(bytes.data(), bytes.size());
  stream.rewind();
  stream.fail_at = fail_at;
  return TBlur_PrepareHistory(&stream);
}
std::vector<uint32> process(uint64 tag, unsigned n) {
  MDFN_Surface surface(nullptr, 8, 8, 10, MDFN_PixelFormat(tag));
  int32 widths[8];
  const int offset = n % 3;
  EmulateSpecStruct spec;
  spec.surface = &surface;
  spec.DisplayRect = {offset, int32(n % 2), 0, n % 4 == 2 ? 4 : 6};
  spec.LineWidths = widths;
  for (unsigned y = 0; y < 8; ++y) {
    widths[y] = 8 - offset - (y % 3 == 1);
    for (unsigned x = 0; x < 8; ++x) {
      const uint32 pixel = surface.MakeColor((n * 17 + x * 25) & 255, y * 30,
                                              (n * 41 + x * 15) & 255, 255);
      if (surface.format.opp == 2) surface.pixels16[y * 10 + x] = uint16(pixel);
      else surface.pixels[y * 10 + x] = pixel;
    }
  }
  if (n % 3 == 1) { widths[0] = -1; spec.DisplayRect.w = 8 - offset; }
  TBlur_Run(&spec);
  std::vector<uint32> output;
  for (unsigned y = 0; y < 8; ++y)
    for (unsigned x = 0; x < 8; ++x)
      output.push_back(surface.format.opp == 2 ? surface.pixels16[y * 10 + x]
                                             : surface.pixels[y * 10 + x]);
  return output;
}
int main() {
  const uint64 formats[] = {MDFN_PixelFormat::ABGR32_8888,
                           MDFN_PixelFormat::IRGB16_1555, MDFN_PixelFormat::RGB16_565};
  for (unsigned mode = 0; mode <= 2; ++mode)
    for (double amount : mode == 2 ? std::vector<double>{0., 25., 50., 75., 100.}
                                   : std::vector<double>{50.})
      for (const auto format : formats) {
        initialize(mode, amount);
        const auto empty = save();
        auto cold = prepare(empty);
        assert(TBlur_CommitHistory(*cold) && save() == empty);
        for (unsigned n = 0; n < 8; ++n) process(format, n);
        const auto origin = save();
        std::vector<std::vector<uint32>> expected;
        for (unsigned n = 8; n < 20; ++n) expected.push_back(process(format, n));
        initialize(mode, amount);
        for (unsigned n = 100; n < 108; ++n) process(format, n);
        const auto wrong_history = process(format, 8);
        if (mode == 1 || (mode == 2 && amount > 0 && amount < 100))
          assert(wrong_history != expected[0]);
        const auto destination = save();
        auto staged = prepare(origin);
        assert(save() == destination);
        assert(TBlur_CommitHistory(*staged) && save() == origin);
        assert(TBlur_CommitHistory(*staged) && save() == destination);
        assert(TBlur_CommitHistory(*staged) && save() == origin);
        for (unsigned n = 8; n < 20; ++n) {
          assert(process(format, n) == expected[n - 8]);
          auto next = prepare(save());
          assert(TBlur_CommitHistory(*next));
        }
        const auto unchanged = save();
        auto reject = [&](const std::vector<uint8>& bytes, int fail_at = -1) {
          bool rejected = false;
          try { auto invalid = prepare(bytes, fail_at); TBlur_CommitHistory(*invalid); }
          catch (const std::exception&) { rejected = true; }
          assert(rejected && save() == unchanged);
        };
        for (size_t n = 0; n < origin.size(); ++n) reject({origin.begin(), origin.begin() + n});
        for (int failure = 0; failure < (mode ? 6 : 5); ++failure) reject(origin, failure);
        auto bad = origin; bad.push_back(0); reject(bad);
        bad = origin; bad[0] = 'X'; reject(bad);
        bad = origin; bad[8] = 3; reject(bad);
        bad = origin; bad[15] = 128; reject(bad);
        bad = origin; bad[19] = 128; reject(bad);
        bad = origin; bad[23] = 128; reject(bad);
        if (mode) { bad = origin; bad[12] = 7; reject(bad); }

        // A producer configuration change between prepare and commit is preserved.
        for (unsigned change = 0; change < (mode ? 4U : 1U); ++change) {
          initialize(mode, amount);
          auto pending = prepare(origin);
          if (change == 0) initialize(mode == 1 ? 2 : 1, amount);
          if (change == 1) initialize(mode, amount, 7, 8);
          if (change == 2) initialize(mode, amount, 8, 7);
          if (change == 3) {
            if (mode == 1) initialize(2, amount);
            else initialize(mode, amount == 50 ? 75 : 50);
          }
          const auto changed = save();
          assert(!TBlur_CommitHistory(*pending) && save() == changed);
          initialize(mode, amount);
          assert(TBlur_CommitHistory(*pending) && save() == origin);
          assert(process(format, 8) == expected[0]);
        }
        if (mode == 1) {
          auto pending = prepare(origin);
          initialize(1, 25); // This coefficient is inactive in pair mode.
          assert(TBlur_CommitHistory(*pending) && save() == origin);
          assert(process(format, 8) == expected[0]);
        }
      }
  TBlur_Kill();
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-blur-history-') as temp:
    cpp = Path(temp) / 'test.cpp'
    executable = Path(temp) / 'test'
    cpp.write_text(code)
    units = ['video/surface.cpp', 'video/convert.cpp', 'video/tblur.cpp',
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
print('Native temporal blur: disabled/pair/accumulation, five accumulation coefficients, three formats, '
      '12-frame continuations, reversible commit, invalid input/read faults and configuration drift pass ASan/UBSan')
