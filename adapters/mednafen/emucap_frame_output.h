#ifndef EMUCAP_FRAME_OUTPUT_H
#define EMUCAP_FRAME_OUTPUT_H

#include <mednafen/mednafen.h>
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <limits>
#include <stdexcept>
#include <vector>

// The in-flight output of one native Emulate call. This is an internal owner,
// with a bounded portable block. The enclosing transaction parks guest writers and
// restores core/resampler/input history separately. Host policy and all live
// pointer bindings belong to the destination.
class EmucapFrameOutput {
 public:
  static EmucapFrameOutput capture(const Mednafen::EmulateSpecStruct& spec,
                                   unsigned channels) {
    if (!valid(spec, channels)) throw std::runtime_error("invalid native frame output");
    EmucapFrameOutput result;
    result.channels_ = channels;
    result.rate_ = spec.SoundRate;
    result.width_ = spec.surface->w;
    result.height_ = spec.surface->h;
    result.metadata_ = metadata(spec);
    const auto count = std::size_t(spec.SoundBufSize) * channels;
    if (count) result.samples_.assign(spec.SoundBuf, spec.SoundBuf + count);
    return result;
  }

  std::vector<std::uint8_t> encode() const {
    if (!valid_owned()) throw std::runtime_error("invalid owned frame output");
    std::vector<std::uint8_t> bytes{'E','C','O','U','T','P','U','T'};
    bytes.reserve(header_size + samples_.size() * 2);
    put(bytes, channels_, 4);
    static_assert(sizeof(double) == 8 && std::numeric_limits<double>::is_iec559,
                  "frame output requires IEEE-754 binary64");
    std::uint64_t rate_bits;
    std::memcpy(&rate_bits, &rate_, 8);
    put(bytes, rate_bits, 8);
    for (int32 value : {width_, height_, metadata_.rect.x, metadata_.rect.y,
                       metadata_.rect.w, metadata_.rect.h})
      put(bytes, std::uint32_t(value), 4);
    put(bytes, unsigned(metadata_.interlaced) | (unsigned(metadata_.field) << 1)
        | (unsigned(metadata_.video_changed) << 2) | (unsigned(metadata_.sound_changed) << 3)
        | (unsigned(metadata_.sound_reverse) << 4), 4);
    for (int32 value : {metadata_.sound_size, metadata_.internal_sound, metadata_.driver_sound})
      put(bytes, std::uint32_t(value), 4);
    for (int64 value : {metadata_.cycles, metadata_.internal_cycles, metadata_.driver_cycles})
      put(bytes, std::uint64_t(value), 8);
    for (int16 value : samples_) put(bytes, std::uint16_t(value), 2);
    return bytes;
  }

  static EmucapFrameOutput decode(const std::vector<std::uint8_t>& bytes) {
    if (bytes.size() < header_size || bytes.size() > header_size + max_samples * 2
        || std::memcmp(bytes.data(), "ECOUTPUT", 8))
      throw std::runtime_error("invalid frame output block");
    std::size_t cursor = 8;
    EmucapFrameOutput result;
    result.channels_ = get(bytes, cursor, 4);
    const auto rate_bits = get(bytes, cursor, 8);
    std::memcpy(&result.rate_, &rate_bits, 8);
    result.width_ = signed32(get(bytes, cursor, 4));
    result.height_ = signed32(get(bytes, cursor, 4));
    auto& m = result.metadata_;
    m.rect.x = signed32(get(bytes, cursor, 4)); m.rect.y = signed32(get(bytes, cursor, 4));
    m.rect.w = signed32(get(bytes, cursor, 4)); m.rect.h = signed32(get(bytes, cursor, 4));
    const auto flags = get(bytes, cursor, 4);
    if (flags & ~std::uint64_t(31)) throw std::runtime_error("invalid frame output flags");
    m.interlaced = flags & 1; m.field = flags & 2;
    m.video_changed = flags & 4; m.sound_changed = flags & 8; m.sound_reverse = flags & 16;
    m.sound_size = signed32(get(bytes, cursor, 4));
    m.internal_sound = signed32(get(bytes, cursor, 4));
    m.driver_sound = signed32(get(bytes, cursor, 4));
    m.cycles = signed64(get(bytes, cursor, 8));
    m.internal_cycles = signed64(get(bytes, cursor, 8));
    m.driver_cycles = signed64(get(bytes, cursor, 8));
    if (!result.valid_header()) throw std::runtime_error("invalid frame output metadata");
    const auto count = std::size_t(m.sound_size) * result.channels_;
    if (bytes.size() - cursor != count * 2)
      throw std::runtime_error("frame output sample length differs");
    result.samples_.resize(count);
    for (auto& sample : result.samples_) {
      const std::uint16_t bits = get(bytes, cursor, 2);
      std::memcpy(&sample, &bits, 2);
    }
    return result;
  }

  // Reserve rollback space before any guest mutation. Even a zero-length source
  // may replace a nonempty destination. Only initialized samples are captured.
  void prepare(const Mednafen::EmulateSpecStruct& destination, unsigned channels) {
    if (!compatible(destination, channels))
      throw std::runtime_error("native frame output configuration differs");
    samples_.reserve(std::size_t(destination.SoundBufMaxSize) * channels);
  }

  bool can_swap_into(const Mednafen::EmulateSpecStruct& destination,
                     unsigned channels) const noexcept {
    return compatible(destination, channels)
        && samples_.capacity() >= std::size_t(destination.SoundBufSize) * channels;
  }

  // Reversible, allocation-free exchange. No surface, row-width, audio pointer,
  // sample-rate, volume, speed, skip or rewind policy is copied from the source.
  bool swap_into(Mednafen::EmulateSpecStruct& destination, unsigned channels) noexcept {
    if (!can_swap_into(destination, channels)) return false;
    const auto saved_count = samples_.size();
    const auto old_count = std::size_t(destination.SoundBufSize) * channels;
    if (old_count > saved_count) samples_.resize(old_count);
    for (std::size_t i = 0; i < std::max(saved_count, old_count); ++i) {
      const int16 old = i < old_count ? destination.SoundBuf[i] : 0;
      if (i < saved_count) destination.SoundBuf[i] = samples_[i];
      if (i < old_count) samples_[i] = old;
    }
    samples_.resize(old_count);
    const auto old = metadata(destination);
    apply(metadata_, destination);
    metadata_ = old;
    return true;
  }

 private:
  struct Metadata {
    Mednafen::MDFN_Rect rect;
    bool interlaced, field, video_changed, sound_changed, sound_reverse;
    int32 sound_size, internal_sound, driver_sound;
    int64 cycles, internal_cycles, driver_cycles;
  };
  static Metadata metadata(const Mednafen::EmulateSpecStruct& s) noexcept {
    return {s.DisplayRect, s.InterlaceOn, s.InterlaceField, s.VideoFormatChanged,
            s.SoundFormatChanged, s.NeedSoundReverse, s.SoundBufSize,
            s.SoundBufSize_InternalProcessed, s.SoundBufSize_DriverProcessed,
            s.MasterCycles, s.MasterCycles_InternalProcessed, s.MasterCycles_DriverProcessed};
  }
  static void apply(const Metadata& m, Mednafen::EmulateSpecStruct& s) noexcept {
    s.DisplayRect = m.rect;
    s.InterlaceOn = m.interlaced; s.InterlaceField = m.field;
    s.VideoFormatChanged = m.video_changed; s.SoundFormatChanged = m.sound_changed;
    s.NeedSoundReverse = m.sound_reverse;
    s.SoundBufSize = m.sound_size;
    s.SoundBufSize_InternalProcessed = m.internal_sound;
    s.SoundBufSize_DriverProcessed = m.driver_sound;
    s.MasterCycles = m.cycles;
    s.MasterCycles_InternalProcessed = m.internal_cycles;
    s.MasterCycles_DriverProcessed = m.driver_cycles;
  }
  static constexpr std::size_t header_size = 84;
  static constexpr std::size_t max_samples = 2 * 1024 * 1024;
  static void put(std::vector<std::uint8_t>& bytes, std::uint64_t value, unsigned size) {
    for (unsigned i = 0; i < size; ++i) bytes.push_back(std::uint8_t(value >> (8 * i)));
  }
  static std::uint64_t get(const std::vector<std::uint8_t>& bytes,
                           std::size_t& cursor, unsigned size) {
    if (cursor > bytes.size() || size > bytes.size() - cursor)
      throw std::runtime_error("truncated frame output block");
    std::uint64_t value = 0;
    for (unsigned i = 0; i < size; ++i) value |= std::uint64_t(bytes[cursor++]) << (8 * i);
    return value;
  }
  static int32 signed32(std::uint32_t bits) noexcept {
    int32 value; std::memcpy(&value, &bits, 4); return value;
  }
  static int64 signed64(std::uint64_t bits) noexcept {
    int64 value; std::memcpy(&value, &bits, 8); return value;
  }
  static bool valid_metadata(const Metadata& m, int32 width, int32 height) noexcept {
    const auto& r = m.rect;
    return width > 0 && height > 0 && r.x >= 0 && r.y >= 0 && r.w >= 0 && r.h >= 0
        && r.x <= width && r.w <= width - r.x && r.y <= height && r.h <= height - r.y
        && 0 <= m.driver_sound && m.driver_sound <= m.internal_sound
        && m.internal_sound <= m.sound_size && 0 <= m.driver_cycles
        && m.driver_cycles <= m.internal_cycles && m.internal_cycles <= m.cycles;
  }
  bool valid_header() const noexcept {
    return (channels_ == 1 || channels_ == 2) && std::isfinite(rate_) && rate_ >= 0
        && valid_metadata(metadata_, width_, height_)
        && std::uint64_t(metadata_.sound_size) * channels_ <= max_samples
        && (rate_ != 0 || metadata_.sound_size == 0);
  }
  bool valid_owned() const noexcept {
    return valid_header() && samples_.size() == std::size_t(metadata_.sound_size) * channels_;
  }
  static bool valid(const Mednafen::EmulateSpecStruct& s, unsigned channels) noexcept {
    if (!s.surface || s.surface->w <= 0 || s.surface->h <= 0 || !s.LineWidths
        || (channels != 1 && channels != 2) || !std::isfinite(s.SoundRate)
        || s.SoundRate < 0 || s.SoundBufMaxSize < 0
        || std::uint64_t(s.SoundBufMaxSize) * channels > max_samples
        || bool(s.SoundBuf) != bool(s.SoundRate)
        || bool(s.SoundBufMaxSize) != bool(s.SoundRate)) return false;
    return valid_metadata(metadata(s), s.surface->w, s.surface->h)
        && s.SoundBufSize <= s.SoundBufMaxSize;
  }
  bool compatible(const Mednafen::EmulateSpecStruct& s, unsigned channels) const noexcept {
    return valid_owned() && valid(s, channels) && channels == channels_ && s.SoundRate == rate_
        && s.surface->w == width_ && s.surface->h == height_
        && metadata_.sound_size <= s.SoundBufMaxSize;
  }
  unsigned channels_ = 0;
  double rate_ = 0;
  int32 width_ = 0, height_ = 0;
  Metadata metadata_{};
  std::vector<int16> samples_;
};
#endif
