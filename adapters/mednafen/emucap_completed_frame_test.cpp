// Exercise the production owner with a surface allocation facade. Native header
// compatibility is checked by the producer build; this facade permits a precise
// allocation failure without exhausting the test host.
#include <cassert>
#include <cstdint>
#include <memory>
#include <vector>
using int32 = std::int32_t;
using uint64 = std::uint64_t;
namespace Mednafen {
struct MDFN_Rect { int32 x, y, w, h; };
constexpr uint64 format32 = (uint64(4) << 48) | (uint64(8) << 6) | (uint64(16) << 12)
    | (uint64(24) << 18) | (uint64(8) << 24) | (uint64(8) << 30)
    | (uint64(8) << 36) | (uint64(8) << 42);
constexpr uint64 format16 = (uint64(2) << 48) | 11 | (uint64(5) << 6)
    | (uint64(16) << 18) | (uint64(5) << 24) | (uint64(6) << 30)
    | (uint64(5) << 36) | (uint64(8) << 42);
struct MDFN_PixelFormat {
  uint64 tag;
  unsigned opp;
  MDFN_PixelFormat(uint64 value) : tag(value), opp((value >> 48) & 255) {}
  bool operator!=(const MDFN_PixelFormat& other) const { return tag != other.tag; }
};
struct MDFN_Surface {
  static bool fail_allocation;
  static unsigned allocations;
  std::vector<std::uint32_t> storage;
  std::vector<std::uint16_t> storage16;
  int32 w, h, pitchinpix;
  MDFN_PixelFormat format;
  std::uint16_t* pixels16;
  std::uint32_t* pixels;
  MDFN_Surface(void*, int32 width, int32 height, int32 pitch, MDFN_PixelFormat f)
      : w(width), h(height), pitchinpix(pitch), format(f) {
    if (fail_allocation) throw std::bad_alloc();
    ++allocations;
    if (f.opp == 2) storage16.resize(std::size_t(pitch) * height);
    else storage.resize(std::size_t(pitch) * height);
    pixels = storage.data();
    pixels16 = storage16.data();
  }
};
bool MDFN_Surface::fail_allocation = false;
unsigned MDFN_Surface::allocations = 0;
}
// The facade is provided in place of the native include by the test runner.
#include "emucap_completed_frame.h"

int main() {
  using namespace Mednafen;
  EmucapCompletedFrame frame;
  assert(frame.surface() == nullptr);
  MDFN_Surface source(nullptr, 3, 2, 5, {format32});
  source.storage = {1, 2, 3, 99, 99, 4, 5, 6, 99, 99};
  source.pixels = source.storage.data();
  int32 widths[2] = {-1, 0};
  MDFN_Rect rect = {0, 0, 3, 2};
  frame.capture(source, rect, widths);
  const auto* owned = frame.surface();
  const unsigned allocations = MDFN_Surface::allocations;
  assert(owned != &source && owned->pitchinpix == 3);
  for (unsigned i = 0; i < 6; ++i) assert(owned->pixels[i] == i + 1);
  source.pixels[0] = 70;
  widths[0] = 3;
  widths[1] = 2;
  assert(owned->pixels[0] == 1 && frame.line_widths()[0] == -1);
  frame.capture(source, rect, widths);
  assert(frame.surface() == owned && MDFN_Surface::allocations == allocations);
  assert(owned->pixels[0] == 70 && frame.line_widths()[1] == 2);

  for (unsigned fault = 0; fault != 12; ++fault) {
    auto bad_rect = rect;
    int32 bad_widths[2] = {3, 2};
    auto* data = source.pixels;
    switch (fault) {
      case 0: bad_rect.x = -1; break;
      case 1: bad_rect.y = 2; break;
      case 2: bad_rect.h = 3; break;
      case 3: bad_widths[1] = 4; break;
      case 4: bad_widths[1] = 0; break;
      case 5: bad_widths[0] = -1; bad_rect.w = 4; break;
      case 6: source.pitchinpix = 2; break;
      case 7: source.format.opp = 1; break;
      case 8: source.pixels = nullptr; break;
      case 9: source.pitchinpix = 0x7fffffff; break;
      case 10: bad_rect.h = 0; break;
    }
    bool rejected = false;
    try { frame.capture(source, bad_rect, fault == 11 ? nullptr : bad_widths); }
    catch (const std::runtime_error&) { rejected = true; }
    assert(rejected && frame.surface() == owned && owned->pixels[0] == 70);
    assert(frame.rect().w == 3 && frame.line_widths()[1] == 2);
    source.pixels = data; source.pitchinpix = 5; source.format.opp = 4;
  }

  MDFN_Surface other(nullptr, 2, 2, 3, {format16});
  other.pixels16[0] = 11; other.pixels16[1] = 12;
  other.pixels16[3] = 13; other.pixels16[4] = 14;
  const int32 narrow_widths[] = {-1, 0};
  MDFN_Surface::fail_allocation = true;
  bool failed = false;
  try { frame.capture(other, {0, 0, 2, 2}, narrow_widths); }
  catch (const std::bad_alloc&) { failed = true; }
  assert(failed && frame.surface() == owned && owned->pixels[0] == 70);
  assert(frame.rect().w == 3 && frame.line_widths()[1] == 2);
  MDFN_Surface::fail_allocation = false;
  frame.capture(other, {0, 0, 2, 2}, narrow_widths);
  assert(frame.surface()->format.opp == 2 && frame.surface()->pitchinpix == 2);
  for (unsigned i = 0; i < 4; ++i) assert(frame.surface()->pixels16[i] == 11 + i);
  other.pixels16[0] = 0;
  assert(frame.surface()->pixels16[0] == 11);
  // Per-line mode permits a zero nominal width and a cropped offset rectangle.
  const int32 cropped_widths[] = {1, 1};
  frame.capture(other, {1, 1, 0, 1}, cropped_widths);
  assert(frame.rect().x == 1 && frame.rect().y == 1 && frame.rect().w == 0);

  // Portable, complete ownership after encode/decode, including absent history.
  const std::vector<std::uint8_t> absent = {'E','C','F','R','A','M','E','2',0,0,0,0};
  EmucapCompletedFrame empty;
  assert(empty.encode() == absent && !EmucapCompletedFrame::decode(absent).surface());
  for (int field : {-1,0,1})
  for (unsigned opp : {2U, 4U}) {
    MDFN_Surface input(nullptr, 2, 1, 3, {opp == 2 ? format16 : format32});
    if (opp == 2) { input.pixels16[0] = 0x1234; input.pixels16[1] = 0xABCD; }
    else { input.pixels[0] = 0x12345678; input.pixels[1] = 0x9ABCDEF0; }
    const int32 lines[] = {-1};
    EmucapCompletedFrame original;
    original.capture(input, {0, 0, 2, 1}, lines, field);
    const auto bytes = original.encode();
    assert(bytes.size() == 52 + 2 * opp);
    assert(bytes[12] == 2 && bytes[16] == 1 && bytes[44] == 255);
    const std::vector<std::uint8_t> expected = opp == 2
        ? std::vector<std::uint8_t>{0x34,0x12,0xCD,0xAB}
        : std::vector<std::uint8_t>{0x78,0x56,0x34,0x12,0xF0,0xDE,0xBC,0x9A};
    assert(std::vector<std::uint8_t>(bytes.begin() + 48, bytes.end() - 4) == expected);
    auto restored = EmucapCompletedFrame::decode(bytes);
    assert(restored.encode() == bytes && restored.surface() != original.surface());
    assert(restored.field()==field);
    for(unsigned i=0;i<4;++i)assert(bytes[bytes.size()-4+i]==std::uint8_t(std::uint32_t(field)>>(8*i)));
    const auto* preserved = restored.surface();
    auto reject = [&](const std::vector<std::uint8_t>& bad) {
      const auto allocations_before = MDFN_Surface::allocations;
      bool rejected = false;
      try { auto staging = EmucapCompletedFrame::decode(bad); restored.swap(staging); }
      catch (const std::runtime_error&) { rejected = true; }
      assert(rejected && restored.surface() == preserved && restored.encode() == bytes);
      assert(MDFN_Surface::allocations == allocations_before);
    };
    for (std::size_t end = 0; end < bytes.size(); ++end)
      reject(std::vector<std::uint8_t>(bytes.begin(), bytes.begin() + end));
    auto bad = bytes; bad.push_back(0); reject(bad);
    bad = bytes; bad[0] = 'X'; reject(bad);
    bad = bytes; bad[7] = '1'; reject(bad);
    bad = bytes; bad[8] = 2; reject(bad);
    bad = bytes; bad[8] = 0; reject(bad); // absent history cannot carry a payload
    bad = bytes; bad[12] = 0; reject(bad);
    bad = bytes; bad[19] = 128; reject(bad); // oversized height before multiplication
    bad = bytes; bad[27] = 1; reject(bad); // unsupported colorspace
    bad = bytes; bad[26] = 1; reject(bad); // paletted pixel data lacks a palette
    bad = bytes; bad[23] &= 0xC0; reject(bad); // zero red precision
    bad = bytes; bad[20] = (bad[20] & 0xC0) | 63; reject(bad); // invalid shift
    bad = bytes; bad[20] = (bad[20] & 0xC0) | (opp == 2 ? 5 : 8); reject(bad);
    bad = bytes; bad[28] = 2; reject(bad); // x outside image
    bad = bytes; bad[28] = bad[29] = bad[30] = bad[31] = 255; reject(bad);
    bad = bytes; bad[36] = 3; reject(bad); // uniform width outside image
    bad = bytes; bad[44] = 0; bad[45] = bad[46] = bad[47] = 0; reject(bad);
    bad=bytes;bad[bad.size()-4]=2;bad[bad.size()-3]=bad[bad.size()-2]=bad[bad.size()-1]=0;reject(bad);
    MDFN_Surface::fail_allocation = true;
    bool allocation_failed = false;
    try { auto staging = EmucapCompletedFrame::decode(bytes); restored.swap(staging); }
    catch (const std::bad_alloc&) { allocation_failed = true; }
    MDFN_Surface::fail_allocation = false;
    assert(allocation_failed && restored.surface() == preserved && restored.encode() == bytes);
    auto no_frame = EmucapCompletedFrame::decode(absent);
    restored.swap(no_frame);
    assert(!restored.surface() && no_frame.encode() == bytes);
  }
}
