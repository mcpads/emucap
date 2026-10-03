#pragma once
#include <cstdint>
#include <limits>

namespace EmucapControl {
// Sole native owner only. These observations are never serialized with guest state.
class NativeControl {
 public:
  enum class Context { None, Frame, Cpu, Continuation, Device, Pacing, Idle };
  class Scope {
   public:
    Scope(NativeControl& owner, Context context)
        : owner_(owner), previous_(owner.context_), generation_(owner.context_generation_) {
      owner_.context_ = context;
      owner_.context_generation_ = owner_.generation_;
    }
    ~Scope() {
      owner_.context_ = previous_;
      owner_.context_generation_ = generation_;
    }
    Scope(const Scope&) = delete;
    Scope& operator=(const Scope&) = delete;
   private:
    NativeControl& owner_;
    Context previous_;
    std::uint64_t generation_;
  };

  void GenerationChanged() { Increment(generation_); }
  void Invalidate() { valid_ = false; }
  void FrameCompleted() { Increment(frames_); }
  // Raw callbacks, NOT executed instructions: cold entry precedes an instruction,
  // and ordinary operation need not arm continuous callbacks.
  void CpuCallback() { Increment(callbacks_); }
  bool Parked(bool quiescent) const {
    return valid_ && quiescent && (context_ == Context::Frame ||
        ((context_ == Context::Cpu || context_ == Context::Continuation)
         && context_generation_ == generation_));
  }
  Context CurrentContext() const { return context_; }
  const char* ContextName() const {
    switch (context_) {
      case Context::Frame: return "frame_boundary";
      case Context::Cpu: return "cpu_callback";
      case Context::Continuation: return "restored_continuation";
      case Context::Device: return "device_callback";
      case Context::Pacing: return "pacing_wait";
      case Context::Idle: return "cpu_idle";
      default: return "none";
    }
  }
  bool Valid() const { return valid_; }
  std::uint64_t Generation() const { return generation_; }
  std::uint64_t Frames() const { return frames_; }
  std::uint64_t Callbacks() const { return callbacks_; }
 private:
  void Increment(std::uint64_t& value) {
    if (value == std::numeric_limits<std::uint64_t>::max()) valid_ = false;
    else ++value;
  }
  std::uint64_t generation_ = 1, frames_ = 0, callbacks_ = 0;
  std::uint64_t context_generation_ = 0;
  Context context_ = Context::None;
  bool valid_ = true;
};
}  // namespace EmucapControl
