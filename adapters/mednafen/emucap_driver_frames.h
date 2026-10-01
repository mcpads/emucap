#ifndef EMUCAP_DRIVER_FRAMES_H
#define EMUCAP_DRIVER_FRAMES_H

#include <mednafen/video/surface.h>
#include "emucap_video_format.h"
#include <array>
#include <cstring>
#include <cstdint>
#include <stdexcept>
#include <vector>

// The native driver must exclude core writers and hold its video mutex while
// capturing or committing these views. Buffers are stored by render/front role,
// never by a process-local address. Commit preserves every live allocation.
class EmucapDriverFrames {
 public:
  struct View {
    Mednafen::MDFN_Surface* surface;
    Mednafen::MDFN_Rect* rect;
    int32* widths;
    int* field;
  };

  static EmucapDriverFrames capture(const std::array<View, 2>& views, unsigned back) {
    if (back > 1) throw std::runtime_error("invalid driver render role");
    EmucapDriverFrames result;
    for (unsigned role = 0; role < 2; ++role) {
      const auto& view = views[back ^ role];
      validate(view);
      auto& frame = result.frames_[role];
      const auto& surface = *view.surface;
      frame.w = surface.w; frame.h = surface.h; frame.tag = surface.format.tag;
      frame.rect = *view.rect; frame.field = *view.field;
      frame.widths.assign(view.widths, view.widths + surface.h);
      const auto row = std::size_t(surface.w) * surface.format.opp;
      frame.pixels.resize(row * surface.h);
      for (int32 y = 0; y < surface.h; ++y)
        std::memcpy(frame.pixels.data() + y * row,
                    pixels(surface) + std::size_t(y) * surface.pitchinpix * surface.format.opp, row);
    }
    return result;
  }

  std::vector<std::uint8_t> encode() const {
    std::vector<std::uint8_t> result{'E','C','D','R','V','F','R','1'};
    for (const auto& frame : frames_) {
      validate(frame);
      const auto opp = emucap_video_pixel_bytes(frame.tag);
      put(result, frame.w, 4); put(result, frame.h, 4); put(result, frame.tag, 8);
      for (int32 value : {frame.rect.x, frame.rect.y, frame.rect.w, frame.rect.h, int32(frame.field)})
        put(result, std::uint32_t(value), 4);
      for (auto width : frame.widths) put(result, std::uint32_t(width), 4);
      for (std::size_t offset = 0; offset < frame.pixels.size(); offset += opp) {
        std::uint32_t word = 0;
        if (opp == 2) { std::uint16_t value; std::memcpy(&value, frame.pixels.data() + offset, 2); word = value; }
        else std::memcpy(&word, frame.pixels.data() + offset, 4);
        put(result, word, opp);
      }
    }
    return result;
  }

  static EmucapDriverFrames decode(const std::vector<std::uint8_t>& bytes) {
    if (bytes.size() < 8 || std::memcmp(bytes.data(), "ECDRVFR1", 8))
      throw std::runtime_error("invalid driver frame history");
    std::size_t cursor = 8;
    EmucapDriverFrames result;
    for (auto& frame : result.frames_) {
      frame.w = word(get(bytes, cursor, 4)); frame.h = word(get(bytes, cursor, 4));
      frame.tag = get(bytes, cursor, 8);
      const auto opp = emucap_video_pixel_bytes(frame.tag);
      dimensions(frame.w, frame.h, opp);
      frame.rect.x = word(get(bytes, cursor, 4)); frame.rect.y = word(get(bytes, cursor, 4));
      frame.rect.w = word(get(bytes, cursor, 4)); frame.rect.h = word(get(bytes, cursor, 4));
      frame.field = word(get(bytes, cursor, 4));
      metadata(frame.w, frame.h, frame.rect, frame.field);
      const auto count = std::size_t(frame.w) * frame.h * opp;
      const auto remaining = std::size_t(frame.h) * 4 + count;
      if (cursor > bytes.size() || remaining > bytes.size() - cursor)
        throw std::runtime_error("truncated driver frame history");
      frame.widths.resize(frame.h);
      for (auto& width : frame.widths) {
        width = word(get(bytes, cursor, 4));
        if (width < -1 || width > frame.w) throw std::runtime_error("invalid driver row width");
      }
      frame.pixels.resize(count);
      for (std::size_t offset = 0; offset < count; offset += opp) {
        const auto value = get(bytes, cursor, opp);
        if (opp == 2) { const std::uint16_t v = value; std::memcpy(frame.pixels.data() + offset, &v, 2); }
        else { const std::uint32_t v = value; std::memcpy(frame.pixels.data() + offset, &v, 4); }
      }
    }
    if (cursor != bytes.size()) throw std::runtime_error("trailing driver frame history");
    return result;
  }

  // Inspect both roles before guest mutation. The caller retains writer exclusion
  // through commit; this result is not a reservation against later changes.
  bool can_swap_into(const std::array<View, 2>& views, unsigned back) const noexcept {
    if (back > 1 || views[0].surface == views[1].surface
        || views[0].widths == views[1].widths || views[0].rect == views[1].rect
        || views[0].field == views[1].field) return false;
    try {
      for (unsigned role = 0; role < 2; ++role) {
        const auto& view = views[back ^ role];
        validate(view);
        const auto& frame = frames_[role];
        validate(frame);
        if (view.surface->w != frame.w || view.surface->h != frame.h
            || view.surface->format.opp != emucap_video_pixel_bytes(frame.tag)) return false;
      }
    } catch (...) { return false; }
    return true;
  }

  // Recheck both views before either changes. The prepared owner retains the
  // destination state, so a second successful swap restores it without allocation.
  bool swap_into(const std::array<View, 2>& views, unsigned back) noexcept {
    if (!can_swap_into(views, back)) return false;
    for (unsigned role = 0; role < 2; ++role) {
      const auto& view = views[back ^ role]; auto& frame = frames_[role];
      auto& surface = *view.surface;
      const auto row = std::size_t(frame.w) * surface.format.opp;
      for (int32 y = 0; y < frame.h; ++y) {
        auto* live = pixels(surface) + std::size_t(y) * surface.pitchinpix * surface.format.opp;
        auto* staged = frame.pixels.data() + std::size_t(y) * row;
        for (std::size_t x = 0; x < row; ++x) std::swap(live[x], staged[x]);
        std::swap(view.widths[y], frame.widths[y]);
      }
      const auto previous_format = surface.format;
      surface.format = Mednafen::MDFN_PixelFormat(frame.tag);
      frame.tag = previous_format.tag;
      std::swap(*view.rect, frame.rect); std::swap(*view.field, frame.field);
    }
    return true;
  }

 private:
  EmucapDriverFrames() = default;
  struct Frame {
    int32 w = 0, h = 0;
    std::uint64_t tag = 0;
    Mednafen::MDFN_Rect rect{};
    int field = -1;
    std::vector<int32> widths;
    std::vector<std::uint8_t> pixels;
  };
  static constexpr std::uint64_t max_bytes = 64ULL * 1024 * 1024;
  static void dimensions(int32 w, int32 h, unsigned opp) {
    if (w <= 0 || h <= 0 || std::uint64_t(w) > max_bytes / opp
        || std::uint64_t(h) > max_bytes / (std::uint64_t(w) * opp)
        || std::uint64_t(h) > max_bytes / 4)
      throw std::runtime_error("invalid driver frame dimensions");
  }
  static void metadata(int32 w, int32 h, const Mednafen::MDFN_Rect& rect, int field) {
    if (rect.x < 0 || rect.y < 0 || rect.x > w || rect.y > h || rect.w < 0 || rect.h < 0
        || rect.w > w - rect.x || rect.h > h - rect.y || field < -1 || field > 1)
      throw std::runtime_error("invalid driver frame metadata");
  }
  static void validate(const View& view) {
    if (!view.surface || !view.rect || !view.widths || !view.field)
      throw std::runtime_error("missing driver frame view");
    const auto& surface = *view.surface;
    const auto opp = emucap_video_pixel_bytes(surface.format.tag);
    dimensions(surface.w, surface.h, opp);
    if (!pixels(surface) || surface.pitchinpix < surface.w
        || std::uint64_t(surface.pitchinpix) * surface.h * opp > max_bytes)
      throw std::runtime_error("invalid driver frame allocation");
    metadata(surface.w, surface.h, *view.rect, *view.field);
    for (int32 y = 0; y < surface.h; ++y)
      if (view.widths[y] < -1 || view.widths[y] > surface.w)
        throw std::runtime_error("invalid driver row width");
  }
  static void validate(const Frame& frame) {
    const auto opp = emucap_video_pixel_bytes(frame.tag);
    dimensions(frame.w, frame.h, opp);
    metadata(frame.w, frame.h, frame.rect, frame.field);
    if (frame.widths.size() != std::size_t(frame.h)
        || frame.pixels.size() != std::size_t(frame.w) * frame.h * opp)
      throw std::runtime_error("incomplete driver frame owner");
  }
  static std::uint8_t* pixels(const Mednafen::MDFN_Surface& surface) {
    return reinterpret_cast<std::uint8_t*>(surface.format.opp == 2
        ? static_cast<void*>(surface.pixels16) : static_cast<void*>(surface.pixels));
  }
  static int32 word(std::uint64_t value) {
    const auto v = std::uint32_t(value);
    return v <= 0x7FFFFFFFU ? int32(v) : -1 - int32(0xFFFFFFFFU - v);
  }
  static void put(std::vector<std::uint8_t>& bytes, std::uint64_t value, unsigned count) {
    for (unsigned i = 0; i < count; ++i) bytes.push_back(std::uint8_t(value >> (i * 8)));
  }
  static std::uint64_t get(const std::vector<std::uint8_t>& bytes, std::size_t& cursor, unsigned count) {
    if (cursor > bytes.size() || count > bytes.size() - cursor)
      throw std::runtime_error("truncated driver frame history");
    std::uint64_t result = 0;
    for (unsigned i = 0; i < count; ++i) result |= std::uint64_t(bytes[cursor++]) << (i * 8);
    return result;
  }
  std::array<Frame, 2> frames_;
};
#endif
