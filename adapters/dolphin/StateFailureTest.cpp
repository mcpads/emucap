// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#include "Common/ChunkFile.h"
#include "VideoCommon/AbstractStagingTexture.h"
#include <array>
#include <cassert>
#include <cstdlib>
#include <iostream>
#include <limits>

namespace Common
{
bool MsgAlertFmtImpl(bool, MsgType, Log::LogType, const char *, int, fmt::string_view,
                     const fmt::format_args &)
{
  std::abort();
}
} // namespace Common

// Exercise native readback with a backend whose Map can fail. No GPU is claimed.
class Readback final : public AbstractStagingTexture
{
public:
  explicit Readback(bool available)
      : AbstractStagingTexture(StagingTextureType::Readback,
                               TextureConfig(1, 1, 1, 1, 1, AbstractTextureFormat::RGBA8, 0,
                                             AbstractTextureType::Texture_2D)),
        available(available)
  {
  }
  bool Map() override
  {
    if (!available)
      return false;
    m_map_pointer = pixels.data();
    m_map_stride = 4;
    return true;
  }
  void Unmap() override { m_map_pointer = nullptr; }
  void Flush() override {}
  void CopyFromTexture(const AbstractTexture *, const MathUtil::Rectangle<int> &, u32, u32,
                       const MathUtil::Rectangle<int> &) override
  {
    std::abort();
  }
  void CopyToTexture(const MathUtil::Rectangle<int> &, AbstractTexture *,
                     const MathUtil::Rectangle<int> &, u32, u32) override
  {
    std::abort();
  }
  bool available;
  std::array<char, 4> pixels{1, 2, 3, 4};
};

int main()
{
  std::array<u8, 32> storage{};
  u8 *cursor = storage.data();
  PointerWrap grow(&cursor, 0, PointerWrap::Mode::Write);
  u32 value = 17;
  grow.Do(value);
  assert(grow.IsMeasureMode() && !grow.HasError());
  assert(grow.GetOffsetFromPreviousPosition(storage.data()) == sizeof(value));

  for (auto mode : {PointerWrap::Mode::Write, PointerWrap::Mode::Read, PointerWrap::Mode::Measure})
  {
    cursor = storage.data();
    PointerWrap failed(&cursor, storage.size(), mode);
    failed.SetError();
    failed.Do(value);
    assert(failed.HasError() && failed.IsMeasureMode());
    assert(storage[0] == 0 && value == 17);
    failed.SetMeasureMode();
    assert(failed.HasError());
  }

  std::array<char, 4> result{9, 9, 9, 9};
  const MathUtil::Rectangle<int> rect{0, 0, 1, 1};
  Readback unavailable(false);
  assert(!unavailable.ReadTexels(rect, result.data(), 4));
  assert((result == std::array<char, 4>{9, 9, 9, 9}));
  Readback available(true);
  assert(available.ReadTexels(rect, result.data(), 4));
  assert(result == available.pixels);
  const TextureConfig image(4, 2, 3, 2, 1, AbstractTextureFormat::RGBA8, 0,
                            AbstractTextureType::Texture_2DArray);
  assert(image.GetSerializedSize(16384) == 88); // (32 + 8 + 4) bytes per layer.
  const std::array<u32, 14> sizes{64, 64, 64, 128, 32, 64, 64, 64, 32, 32, 64, 64, 64, 128};
  for (u32 i = 0; i < sizes.size(); ++i)
  {
    TextureConfig config(4, 4, 1, 1, 1, static_cast<AbstractTextureFormat>(i), 0,
                         AbstractTextureType::Texture_2DArray);
    assert(config.GetSerializedSize(16384) == sizes[i]);
  }
  // Invalid metadata is rejected without allocation, including overflow and unsafe shifts.
  for (auto mutate : std::array<void (*)(TextureConfig&), 13>{
           [](auto& c) { c.width = 0; }, [](auto& c) { c.height = 0; },
           [](auto& c) { c.width = 16385; }, [](auto& c) { c.levels = 0; },
           [](auto& c) { c.levels = 32; }, [](auto& c) { c.layers = 0; },
           [](auto& c) { c.layers = std::numeric_limits<u32>::max(); },
           [](auto& c) { c.samples = 0; }, [](auto& c) { c.samples = 2; },
           [](auto& c) { c.format = AbstractTextureFormat::Undefined; },
           [](auto& c) { c.format = static_cast<AbstractTextureFormat>(0xffffffff); },
           [](auto& c) { c.type = static_cast<AbstractTextureType>(255); },
           [](auto& c) { c.flags = 4; }})
  {
    auto invalid = image;
    mutate(invalid);
    assert(!invalid.GetSerializedSize(16384));
  }
  TextureConfig large(16384, 16384, 1, 1, 1, AbstractTextureFormat::RGBA16F, 0,
                      AbstractTextureType::Texture_2DArray);
  assert(large.GetSerializedSize(16384) == 2147483648u);
  large.layers = 2;
  assert(!large.GetSerializedSize(16384));
  large.width = std::numeric_limits<u32>::max();
  large.height = large.layers = 1;
  assert(!large.GetSerializedSize(std::numeric_limits<u32>::max()));
  std::cout << "native serializer, image extent and readback checks passed\n";
}
