#pragma once
#include <algorithm>
#include <cstdint>
#include <limits>
#include <string>

namespace EmucapControl {
struct AdvanceObservation {
  std::uint64_t generation, frames, callbacks, host_ms;
};
// Requires continuous callbacks on the admitted CPU throughout the operation.
// The caller supplies actual park evidence separately before publishing a terminal.
class NativeAdvance {
 public:
  NativeAdvance(bool instructions, std::uint64_t requested, AdvanceObservation origin,
                bool cold)
      : instructions_(instructions), requested_(requested), cold_(instructions && cold ? 1 : 0),
        origin_(origin), previous_(origin) {
    valid_ = requested > 0 && requested <= 5000 && origin.generation != 0 &&
        origin.host_ms <= std::numeric_limits<std::uint64_t>::max() - 245000;
  }
  bool Observe(AdvanceObservation value) {
    if (!valid_ || value.generation != origin_.generation || value.frames < previous_.frames ||
        value.callbacks < previous_.callbacks || value.host_ms < previous_.host_ms) {
      valid_ = false; return false;
    }
    previous_ = value;
    const auto delta = instructions_ ? value.callbacks - origin_.callbacks : value.frames - origin_.frames;
    count_ = std::min(requested_, delta > cold_ ? delta - cold_ : 0);
    if (reason_.empty() && count_ == requested_) reason_ = "completed";
    if (reason_.empty() && value.host_ms - origin_.host_ms >= 245000) reason_ = "host_deadline";
    return true;
  }
  void Cancel(const char* reason) { if (reason_.empty()) reason_ = reason; }
  bool TargetAt(std::uint64_t callbacks) const {
    return valid_ && instructions_ && callbacks >= origin_.callbacks &&
        callbacks - origin_.callbacks >= requested_ + cold_;
  }
  bool Valid() const { return valid_; }
  bool Stopping() const { return !reason_.empty(); }
  bool Instructions() const { return instructions_; }
  std::uint64_t Count() const { return count_; }
  std::uint64_t Requested() const { return requested_; }
  const std::string& Reason() const { return reason_; }
 private:
  bool instructions_, valid_ = true;
  std::uint64_t requested_, cold_, count_ = 0;
  AdvanceObservation origin_, previous_;
  std::string reason_;
};
} // namespace EmucapControl
