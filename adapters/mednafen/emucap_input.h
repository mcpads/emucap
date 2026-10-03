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

  void apply(unsigned char* data, unsigned length, uint16_t buttons = 0xFFFF) const {
    if (!data || length == 0) return;
    const uint32_t state = state_.load(std::memory_order_acquire);
    if ((state & kEngaged) == 0) return;

    const uint16_t previous = data[0] | (length > 1 ? uint16_t(data[1]) << 8 : 0);
    const uint16_t merged = (previous & ~buttons) | (static_cast<uint16_t>(state) & buttons);
    data[0] = static_cast<unsigned char>(merged & 0xFF);
    if (length > 1) data[1] = static_cast<unsigned char>((merged >> 8) & 0xFF);
  }

 private:
  enum : uint32_t { kEngaged = 1u << 16 };
  std::atomic<uint32_t> state_;
};
