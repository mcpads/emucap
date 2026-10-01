#ifndef EMUCAP_COMPLETED_FRAME_H
#define EMUCAP_COMPLETED_FRAME_H

#include <mednafen/video/surface.h>
#include "emucap_video_format.h"
#include <cstring>
#include <cstdint>
#include <memory>
#include <stdexcept>
#include <vector>

// Each instance has a serialized owner: game-thread observation, or the native
// display transaction protected by VTMutex. Consumers receive const views.
class EmucapCompletedFrame {
 public:
  const Mednafen::MDFN_Surface* surface() const { return surface_.get(); }
  const Mednafen::MDFN_Rect& rect() const { return rect_; }
  int field() const { return surface_ ? field_ : -1; }
  const int32* line_widths() const { return widths_.data(); }

  // A self-contained block for the enclosing native snapshot transaction. Pixel
  // words and metadata are little endian; pitch padding and host pointers are not
  // part of the artifact. Decoding stages a new owner before commit.
  std::vector<std::uint8_t> encode() const {
    std::vector<std::uint8_t> bytes{'E', 'C', 'F', 'R', 'A', 'M', 'E', '2'};
    put(bytes, surface_ ? 1 : 0, 4);
    if (!surface_) return bytes;
    validate(*surface_, rect_, widths_.data());
    emucap_video_pixel_bytes(surface_->format.tag);
    put(bytes, surface_->w, 4);
    put(bytes, surface_->h, 4);
    put(bytes, surface_->format.tag, 8);
    put(bytes, std::uint32_t(rect_.x), 4);
    put(bytes, std::uint32_t(rect_.y), 4);
    put(bytes, std::uint32_t(rect_.w), 4);
    put(bytes, std::uint32_t(rect_.h), 4);
    bytes.reserve(48 + std::size_t(surface_->h) * 4
                  + std::size_t(surface_->w) * surface_->h * surface_->format.opp);
    for (const auto width : widths_) put(bytes, std::uint32_t(width), 4);
    const std::size_t count = std::size_t(surface_->w) * surface_->h;
    for (std::size_t i = 0; i < count; ++i)
      put(bytes, surface_->format.opp == 2 ? surface_->pixels16[i] : surface_->pixels[i],
          surface_->format.opp);
    put(bytes, std::uint32_t(field_), 4);
    return bytes;
  }

  static EmucapCompletedFrame decode(const std::vector<std::uint8_t>& bytes) {
    if (bytes.size() < 12 || std::memcmp(bytes.data(), "ECFRAME2", 8))
      throw std::runtime_error("invalid completed frame block");
    const auto present = get(bytes, 8, 4);
    EmucapCompletedFrame result;
    if (!present && bytes.size() == 12) return result;
    if (present != 1 || bytes.size() < 48)
      throw std::runtime_error("invalid completed frame presence");
    const auto w = get(bytes, 12, 4), h = get(bytes, 16, 4);
    const auto tag = get(bytes, 20, 8);
    const unsigned opp = emucap_video_pixel_bytes(tag);
    // Bound each dimension before multiplying untrusted 32-bit values.
    if (!w || !h || w > max_bytes / opp || h > max_bytes / (w * opp)
        || h > max_bytes / 4 || bytes.size() != 48 + h * 4 + w * h * opp)
      throw std::runtime_error("invalid completed frame block length");
    Mednafen::MDFN_Rect rect = {
        signed_word(get(bytes, 28, 4)), signed_word(get(bytes, 32, 4)),
        signed_word(get(bytes, 36, 4)), signed_word(get(bytes, 40, 4))};
    if (rect.x < 0 || rect.y < 0 || rect.h <= 0 || uint64(rect.x) >= w
        || uint64(rect.y) >= h || uint64(rect.h) > h - rect.y)
      throw std::runtime_error("invalid completed frame block rectangle");
    const bool uniform = get(bytes, 44, 4) == 0xFFFFFFFFULL;
    if (uniform) {
      if (rect.w <= 0 || uint64(rect.w) > w - rect.x)
        throw std::runtime_error("invalid completed frame block width");
    } else {
      for (int32 y = rect.y; y < rect.y + rect.h; ++y) {
        const auto width = get(bytes, 44 + std::size_t(y) * 4, 4);
        if (!width || width > w - rect.x)
          throw std::runtime_error("invalid completed frame block line width");
      }
    }
    const int field = signed_word(get(bytes, bytes.size() - 4, 4));
    validate_field(field);
    // No live owner has changed, including when either allocation fails.
    result.widths_.resize(h);
    result.surface_.reset(new Mednafen::MDFN_Surface(
        nullptr, w, h, w, Mednafen::MDFN_PixelFormat(tag)));
    result.rect_ = rect;
    result.field_ = field;
    for (std::size_t i = 0; i < h; ++i)
      result.widths_[i] = signed_word(get(bytes, 44 + i * 4, 4));
    const std::size_t start = 44 + h * 4;
    for (std::size_t i = 0; i < w * h; ++i) {
      const auto value = get(bytes, start + i * opp, opp);
      if (opp == 2) result.surface_->pixels16[i] = std::uint16_t(value);
      else result.surface_->pixels[i] = std::uint32_t(value);
    }
    return result;
  }

  void swap(EmucapCompletedFrame& other) noexcept {
    surface_.swap(other.surface_);
    widths_.swap(other.widths_);
    std::swap(rect_, other.rect_);
    std::swap(field_, other.field_);
  }

  void capture(const Mednafen::MDFN_Surface& source,
               const Mednafen::MDFN_Rect& rect, const int32* widths, int field = -1) {
    validate_field(field);
    validate(source, rect, widths);
    const bool replace = !surface_ || surface_->w != source.w
        || surface_->h != source.h || surface_->format != source.format;
    // Complete every allocation before changing the previously owned image.
    std::vector<int32> next_widths;
    std::unique_ptr<Mednafen::MDFN_Surface> next_surface;
    if (replace) {
      next_widths.resize(source.h);
      next_surface.reset(new Mednafen::MDFN_Surface(
          nullptr, source.w, source.h, source.w, source.format));
    }
    auto* target = replace ? next_surface.get() : surface_.get();
    auto* target_widths = replace ? next_widths.data() : widths_.data();
    const std::size_t row_bytes = std::size_t(source.w) * source.format.opp;
    const auto* src = pixels(source);
    auto* dst = const_cast<unsigned char*>(pixels(*target));
    for (int32 y = 0; y < source.h; ++y)
      std::memcpy(dst + std::size_t(y) * row_bytes,
                  src + std::size_t(y) * source.pitchinpix * source.format.opp,
                  row_bytes);
    std::memcpy(target_widths, widths, std::size_t(source.h) * sizeof(int32));
    if (replace) {
      surface_.swap(next_surface);
      widths_.swap(next_widths);
    }
    rect_ = rect;
    field_ = field;
  }

 private:
  static void validate_field(int field) {
    if (field < -1 || field > 1) throw std::runtime_error("invalid completed frame field");
  }
  static constexpr uint64 max_bytes = 64ULL * 1024 * 1024;

  static void put(std::vector<std::uint8_t>& bytes, uint64 value, unsigned size) {
    for (unsigned i = 0; i < size; ++i) bytes.push_back(std::uint8_t(value >> (8 * i)));
  }

  static uint64 get(const std::vector<std::uint8_t>& bytes, std::size_t offset, unsigned size) {
    if (offset > bytes.size() || size > bytes.size() - offset)
      throw std::runtime_error("truncated completed frame block");
    uint64 value = 0;
    for (unsigned i = 0; i < size; ++i) value |= uint64(bytes[offset + i]) << (8 * i);
    return value;
  }

  static int32 signed_word(uint64 value) {
    return value <= 0x7FFFFFFFULL ? int32(value) : -1 - int32(0xFFFFFFFFULL - value);
  }


  static const unsigned char* pixels(const Mednafen::MDFN_Surface& surface) {
    return surface.format.opp == 2
        ? reinterpret_cast<const unsigned char*>(surface.pixels16)
        : reinterpret_cast<const unsigned char*>(surface.pixels);
  }

  static void validate(const Mednafen::MDFN_Surface& surface,
                       const Mednafen::MDFN_Rect& rect, const int32* widths) {
    // The producer allocates native surfaces, but never encode an invalid core
    // rectangle or an unbounded surface as a successful screenshot.
    if (!widths || surface.w <= 0 || surface.h <= 0
        || surface.pitchinpix < surface.w
        || uint64(surface.h) > max_bytes / 4
        || (surface.format.opp != 2 && surface.format.opp != 4)
        || !pixels(surface)
        || uint64(surface.pitchinpix) * uint64(surface.h) * surface.format.opp > max_bytes
        || rect.x < 0 || rect.y < 0 || rect.h <= 0
        || rect.x >= surface.w || rect.y >= surface.h
        || rect.h > surface.h - rect.y)
      throw std::runtime_error("invalid completed frame geometry");
    // A -1 first entry means all rows use rect.w; otherwise each visible row
    // carries its own width and rect.w need not be positive (PSX/PC-FX).
    if (widths[0] == -1) {
      if (rect.w <= 0 || rect.w > surface.w - rect.x)
        throw std::runtime_error("invalid completed frame width");
    } else {
      for (int32 y = rect.y; y < rect.y + rect.h; ++y)
        if (widths[y] <= 0 || widths[y] > surface.w - rect.x)
          throw std::runtime_error("invalid completed frame line width");
    }
  }

  std::unique_ptr<Mednafen::MDFN_Surface> surface_;
  Mednafen::MDFN_Rect rect_ = {};
  int field_ = -1;
  std::vector<int32> widths_;
};
#endif
