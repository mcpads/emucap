#pragma once

#include <atomic>
#include <cstdint>

// Flycast input is active-low, but callers describe the pressed bits as active-high. Keep the
// ownership bit and mask in one atomic snapshot so the Maple consumer never observes a newly
// engaged override with an old mask. An empty mask is the explicit native-input handoff.
class EmucapFlycastInputOverride {
 public:
  EmucapFlycastInputOverride() : state_(0) {}

  void set(std::uint32_t pressed_mask) {
    if (pressed_mask == 0) {
      release();
      return;
    }
    state_.store(kEngaged | pressed_mask, std::memory_order_release);
  }

  void release() {
    state_.store(0, std::memory_order_release);
  }

  bool engaged() const {
    return (state_.load(std::memory_order_acquire) & kEngaged) != 0;
  }

  std::uint32_t pressed_mask() const {
    return static_cast<std::uint32_t>(state_.load(std::memory_order_acquire));
  }

  std::uint32_t kcode() const {
    return ~pressed_mask();
  }

 private:
  static constexpr std::uint64_t kEngaged = std::uint64_t{1} << 32;
  std::atomic<std::uint64_t> state_;
};
