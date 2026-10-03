#ifndef EMUCAP_FRAME_CALL_H
#define EMUCAP_FRAME_CALL_H
#include "emucap_frame_output.h"
#include <memory>
#include <utility>

// Native wrapper phase, independent of the destination's C++ stack. All methods
// run on the parked game thread. The enclosing snapshot transaction owns guest
// state, input, filter history and display publication.
class EmucapFrameCall {
 public:
  using Output = std::unique_ptr<EmucapFrameOutput>;
  class Replacement {
    friend class EmucapFrameCall;
    Output output_;
    unsigned channels_;
    bool consumed_ = false;
    Replacement(Output output, unsigned channels) : output_(std::move(output)), channels_(channels) {}
   public:
    Replacement(Replacement&& other) noexcept
        : output_(std::move(other.output_)), channels_(other.channels_), consumed_(other.consumed_) {
      other.consumed_ = true;
    }
  };

  class Scope {
   public:
    Scope(EmucapFrameCall& owner, Mednafen::EmulateSpecStruct& spec, unsigned channels)
        : owner_(owner), restored_(owner.enter(spec, channels)) {}
    ~Scope() { owner_.leave(); }
    bool restored() const noexcept { return restored_; }
    Scope(const Scope&) = delete;
    Scope& operator=(const Scope&) = delete;
   private:
    EmucapFrameCall& owner_;
    bool restored_;
  };

  Output capture() const {
    if (restart_) return {};
    if (active_) return Output(new EmucapFrameOutput(EmucapFrameOutput::capture(*active_, channels_)));
    if (pending_) return Output(new EmucapFrameOutput(*pending_));
    return {};
  }

  Replacement prepare(Output saved, const Mednafen::EmulateSpecStruct& binding,
                      unsigned channels) const {
    if (saved) saved->prepare(active_ ? *active_ : binding, channels);
    return Replacement(std::move(saved), channels);
  }

  bool commit(Replacement& replacement, unsigned channels) noexcept {
    if (replacement.consumed_ || channels != replacement.channels_
        || (active_ && channels != channels_)) return false;
    if (active_ && replacement.output_
        && !replacement.output_->swap_into(*active_, channels)) return false;
    const bool partial = bool(replacement.output_);
    if (active_) {
      pending_.reset();
      restart_ = !partial;
      restored_active_ = partial;
    } else {
      pending_ = std::move(replacement.output_);
      restart_ = restored_active_ = false;
    }
    replacement.consumed_ = true;
    return true;
  }

  bool pending() const noexcept { return bool(pending_); }
  bool restored_active() const noexcept { return restored_active_; }
  void invalidate() noexcept {
    pending_.reset();
    restart_ = restored_active_ = false;
  }

  // Call only after the restore acknowledgement park is released. A completed
  // source needs ordinary wrapper initialization before native guest work.
  template<class Initialize>
  void resume(Initialize initialize) {
    if (restart_) {
      if (!active_) throw std::runtime_error("missing native frame binding");
      initialize(*active_);
      restart_ = false;
    }
  }

 private:
  bool enter(Mednafen::EmulateSpecStruct& spec, unsigned channels) {
    if (active_) throw std::runtime_error("nested native frame call");
    const bool restored = bool(pending_);
    if (pending_ && !pending_->swap_into(spec, channels))
      throw std::runtime_error("restored native output binding changed");
    pending_.reset();
    active_ = &spec;
    channels_ = channels;
    restart_ = false;
    restored_active_ = restored;
    return restored;
  }
  void leave() noexcept {
    active_ = nullptr;
    restart_ = restored_active_ = false;
  }
  Mednafen::EmulateSpecStruct* active_ = nullptr;
  unsigned channels_ = 0;
  Output pending_;
  bool restart_ = false, restored_active_ = false;
};

// Internal native-wrapper bridge. External snapshot handlers must prepare all
// owners before guest mutation and commit them before acknowledging replacement.
// Read current driver buffers and host policy on the parked game thread.
// Does not initialize guest output or advance emulation.
Mednafen::EmulateSpecStruct emucap_frame_binding(bool skip);
EmucapFrameCall::Output emucap_capture_frame_output();
EmucapFrameCall::Replacement emucap_prepare_frame_output(
    EmucapFrameCall::Output saved, const Mednafen::EmulateSpecStruct& binding, unsigned channels);
bool emucap_commit_frame_output(EmucapFrameCall::Replacement&, unsigned channels) noexcept;
bool emucap_frame_output_pending() noexcept;
bool emucap_restored_frame_output_active() noexcept;
void emucap_invalidate_frame_output() noexcept;
void emucap_resume_frame_output();
#endif
