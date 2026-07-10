#pragma once

#include <atomic>
#include <cstdint>

class EmucapInputOverride {
 public:
  EmucapInputOverride() : state_(0) {}

  void engage(uint16_t mask) {
    state_.store(kEngaged | mask, std::memory_order_release);
  }

  void release() {
    state_.store(0, std::memory_order_release);
  }

  bool engaged() const {
    return (state_.load(std::memory_order_acquire) & kEngaged) != 0;
  }

  uint16_t mask() const {
    return static_cast<uint16_t>(state_.load(std::memory_order_acquire));
  }

  void apply(unsigned char* data, unsigned length) const {
    if (!data || length == 0) return;
    const uint32_t state = state_.load(std::memory_order_acquire);
    if ((state & kEngaged) == 0) return;

    const uint16_t mask = static_cast<uint16_t>(state);
    data[0] = static_cast<unsigned char>(mask & 0xFF);
    if (length > 1) data[1] = static_cast<unsigned char>((mask >> 8) & 0xFF);
  }

 private:
  enum : uint32_t { kEngaged = 1u << 16 };
  std::atomic<uint32_t> state_;
};
