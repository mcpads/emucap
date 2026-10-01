#ifndef EMUCAP_PRESENTATION_SURFACE_H
#define EMUCAP_PRESENTATION_SURFACE_H

#include <mednafen/video/surface.h>
#include <mednafen/video/convert.h>
#include "emucap_video_format.h"
#include <memory>
#include <stdexcept>

// Display-thread storage. The original driver surface remains authoritative and
// is never reformatted or reallocated by presentation.
class EmucapPresentationSurface {
 public:
  const Mednafen::MDFN_Surface& convert(const Mednafen::MDFN_Surface& source,
                                      const Mednafen::MDFN_PixelFormat& format) {
    const unsigned source_bytes = emucap_video_pixel_bytes(source.format.tag);
    const unsigned target_bytes = emucap_video_pixel_bytes(format.tag);
    const std::uint64_t limit = 64ULL * 1024 * 1024;
    if (source.w <= 0 || source.h <= 0 || source.pitchinpix < source.w
        || std::uint64_t(source.pitchinpix) * source.h > limit / source_bytes
        || std::uint64_t(source.w) * source.h > limit / target_bytes || !pixels(source))
      throw std::runtime_error("invalid presentation surface");
    if (source.format == format) return source;
    const bool replace = !surface_ || surface_->w != source.w || surface_->h != source.h
        || surface_->format != format;
    std::unique_ptr<Mednafen::MDFN_Surface> candidate;
    if (replace) candidate.reset(new Mednafen::MDFN_Surface(
        nullptr, source.w, source.h, source.w, format));
    auto* target = replace ? candidate.get() : surface_.get();
    Mednafen::MDFN_PixelFormatConverter converter(source.format, format);
    for (int32 y = 0; y < source.h; ++y)
      converter.Convert(pixels(source) + std::size_t(y) * source.pitchinpix * source_bytes,
                        pixels(*target) + std::size_t(y) * target->pitchinpix * target_bytes,
                        source.w);
    if (replace) surface_.swap(candidate);
    return *surface_;
  }

  void clear() noexcept { surface_.reset(); }

 private:
  static uint8* pixels(const Mednafen::MDFN_Surface& surface) {
    return reinterpret_cast<uint8*>(surface.format.opp == 2
        ? static_cast<void*>(surface.pixels16) : static_cast<void*>(surface.pixels));
  }
  std::unique_ptr<Mednafen::MDFN_Surface> surface_;
};
#endif
