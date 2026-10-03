#ifndef EMUCAP_VIDEO_RESTORE_H
#define EMUCAP_VIDEO_RESTORE_H

#include <mednafen/mednafen.h>
#include <mednafen/MemoryStream.h>
#include "emucap_driver_video.h"
#include <exception>
#include <utility>

// Internal participants of the enclosing guest/driver snapshot transaction.
// These blocks do not constitute a file format or carry guest/continuation state.
struct EmucapVideoBlocks {
  std::vector<std::uint8_t> raster, filters, completed;
};

class EmucapVideoRestore {
 public:
  static EmucapVideoBlocks capture(const EmucapDriverVideo& video,
                                   const EmucapCompletedFrame& observation) {
    EmucapVideoBlocks result;
    result.raster = video.capture().encode();
    Mednafen::MemoryStream filters;
    Mednafen::MDFNI_SaveVideoHistory(&filters);
    result.filters.assign(filters.map(), filters.map() + filters.map_size());
    result.completed = observation.encode();
    return result;
  }

  // Guest writers must be parked throughout capture, preparation and commit.
  // Preparation changes no live participant and allocates both image owners.
  static std::unique_ptr<EmucapVideoRestore> prepare(const EmucapVideoBlocks& blocks,
                                                   const EmucapDriverVideo& video) {
    auto raster = EmucapDriverFrames::decode(blocks.raster);
    if (!video.can_restore(raster)) throw std::runtime_error("driver video configuration differs");
    if (blocks.filters.size() > 128ULL * 1024 * 1024 + 40)
      throw std::runtime_error("video filter history exceeds bound");
    Mednafen::MemoryStream stream(blocks.filters.size(), -1);
    if (!blocks.filters.empty())
      std::memcpy(stream.map(), blocks.filters.data(), blocks.filters.size());
    auto filters = Mednafen::MDFNI_PrepareVideoHistory(&stream);
    auto observation = EmucapCompletedFrame::decode(blocks.completed);
    if (observation.surface()
        && (observation.surface()->w != Mednafen::MDFNGameInfo->fb_width
            || observation.surface()->h != Mednafen::MDFNGameInfo->fb_height))
      throw std::runtime_error("completed video geometry differs");
    auto publication = EmucapCompletedFrame::decode(blocks.completed);
    return std::unique_ptr<EmucapVideoRestore>(new EmucapVideoRestore(
        std::move(raster), std::move(filters), std::move(observation), std::move(publication)));
  }

  // Final visual commit: no allocations and no publication before all owners
  // agree. Call only after the enclosing guest/continuation work has succeeded.
  // On false the destination and queue are unchanged; successful owners are
  // consumed, so a duplicate commit cannot republish abandoned destination data.
  bool commit(EmucapDriverVideo& video, EmucapCompletedFrame& observation) noexcept {
    if (committed_ || !video.can_restore(raster_)) return false;
    if (!Mednafen::MDFNI_CommitVideoHistory(*filters_)) return false;
    if (!video.restore(raster_)) {
      if (!Mednafen::MDFNI_CommitVideoHistory(*filters_)) std::terminate();
      return false;
    }
    if (!video.publish(publication_)) {
      if (!video.restore(raster_) || !Mednafen::MDFNI_CommitVideoHistory(*filters_))
        std::terminate();
      return false;
    }
    observation.swap(observation_);
    committed_ = true;
    return true;
  }

 private:
  EmucapVideoRestore(EmucapDriverFrames&& raster,
                    std::unique_ptr<Mednafen::MDFNVideoHistory>&& filters,
                    EmucapCompletedFrame&& observation, EmucapCompletedFrame&& publication)
      : raster_(std::move(raster)), filters_(std::move(filters)),
        observation_(std::move(observation)), publication_(std::move(publication)) {}
  EmucapDriverFrames raster_;
  std::unique_ptr<Mednafen::MDFNVideoHistory> filters_;
  EmucapCompletedFrame observation_, publication_;
  bool committed_ = false;
};
#endif
