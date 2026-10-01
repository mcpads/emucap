#ifndef EMUCAP_VIDEO_FORMAT_H
#define EMUCAP_VIDEO_FORMAT_H
#include <cstdint>
#include <stdexcept>

inline unsigned emucap_video_pixel_bytes(std::uint64_t tag) {
  const unsigned opp = (tag >> 48) & 0xFF;
  if ((tag >> 56) != 0 || (opp != 2 && opp != 4))
    throw std::runtime_error("unsupported video history pixel format");
  std::uint64_t occupied = 0;
  for (unsigned channel = 0; channel < 4; ++channel) {
    const unsigned shift = (tag >> (channel * 6)) & 63;
    const unsigned precision = (tag >> (24 + channel * 6)) & 63;
    // The native 16-bit formats can carry alpha above the stored RGB word;
    // DecodeColor still requires nonzero precision and defined shifts.
    const unsigned limit = opp == 2 && channel != 3 ? 16 : 32;
    if (!precision || precision > 8 || shift >= limit || precision > limit - shift
        || (opp == 4 && (precision != 8 || shift % 8 != 0)))
      throw std::runtime_error("invalid video history color channel");
    const std::uint64_t mask = ((std::uint64_t(1) << precision) - 1) << shift;
    if (occupied & mask) throw std::runtime_error("overlapping video history color channels");
    occupied |= mask;
  }
  return opp;
}

#endif
