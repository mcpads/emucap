#pragma once
#include <algorithm>
#include <cstddef>
#include <string>

// Frame storage shared by ordinary and pacing service. Inspect queued input
// before receiving again, and consume at most one frame per owner callback.
class EmucapReceiveBuffer {
 public:
  explicit EmucapReceiveBuffer(std::size_t cap = 8 * 1024 * 1024) : cap_(cap) {}
  void set_limit(std::size_t cap) { cap_ = cap; }
  enum Result { incomplete, ready, oversized };
  Result peek(std::string& line) const {
    const std::size_t end = bytes_.find('\n');
    if (end == std::string::npos)
      return bytes_.size() > cap_ ? oversized : incomplete;
    if (end > cap_) return oversized;
    line.assign(bytes_, 0, end);
    return ready;
  }
  std::size_t read_limit(std::size_t chunk) const {
    return bytes_.size() > cap_ ? 0 : std::min(chunk, cap_ + 1 - bytes_.size());
  }
  void append(const char* data, std::size_t length) { bytes_.append(data, length); }
  void consume() {
    const std::size_t end = bytes_.find('\n');
    if (end != std::string::npos) bytes_.erase(0, end + 1);
  }
  void clear() { bytes_.clear(); }
 private:
  std::size_t cap_;
  std::string bytes_;
};
