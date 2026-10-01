#ifndef EMUCAP_DRIVER_VIDEO_H
#define EMUCAP_DRIVER_VIDEO_H
#include "emucap_driver_frames.h"
#include "emucap_completed_frame.h"

// Game-thread transaction guard. The caller separately parks all guest writers.
// End the scope before acknowledging restoration or waiting for another command.
class EmucapDriverVideo {
 public:
  EmucapDriverVideo();
  ~EmucapDriverVideo() noexcept;
  EmucapDriverVideo(const EmucapDriverVideo&) = delete;
  EmucapDriverVideo& operator=(const EmucapDriverVideo&) = delete;
  EmucapDriverFrames capture() const;
  bool can_restore(const EmucapDriverFrames&) const noexcept;
  bool restore(EmucapDriverFrames&) noexcept;
  // Prepared display image is independent from the observation and raster owners.
  // Publish only after every reversible participant has committed successfully.
  bool publish(EmucapCompletedFrame&) noexcept;
 private:
  void* mutex_;
};
#endif
