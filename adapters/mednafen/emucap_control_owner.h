#pragma once
#include "emucap_control_identity.h"
#include <memory>
#include <set>

namespace EmucapControl {
// C++11 native counterpart of live::temporal::owner, following the Dolphin
// producer's transitions. Only the emulation owner calls this after transport
// authentication. The adapter supplies actual native stop/release evidence.
struct CleanupPlan {
  Key key;
  Attachment attachment;
  std::set<std::uint64_t> input_ports;
  bool effects_started = false;
  bool operator==(const CleanupPlan& other) const {
    return key == other.key && attachment == other.attachment &&
        input_ports == other.input_ports && effects_started == other.effects_started;
  }
};
struct CleanupEvidence {
  bool stop_verified = false;
  std::set<std::uint64_t> released_ports;
};
enum class OwnerResult { Ok, InvalidIdentity, WrongAttachment, Busy, NotActive, Retired };

class Ownership {
 public:
  explicit Ownership(std::string runtime) : runtime_(std::move(runtime)) {}
  OwnerResult Attach(const Attachment& attachment) {
    if (retired_) return OwnerResult::Retired;
    if (!ValidIdentityPart(runtime_) || !attachment.Valid()) return OwnerResult::InvalidIdentity;
    if (Attached(attachment)) return OwnerResult::Ok;
    if (parent_ || attachment_) return OwnerResult::Busy;
    std::unique_ptr<Attachment> next(new Attachment(attachment));
    terminal_.reset();
    attachment_ = std::move(next);
    return OwnerResult::Ok;
  }
  OwnerResult Begin(const Attachment& attachment, const Key& key) {
    if (retired_) return OwnerResult::Retired;
    if (!key.Valid() || key.runtime != runtime_) return OwnerResult::InvalidIdentity;
    if (!Attached(attachment)) return OwnerResult::WrongAttachment;
    if (parent_) return parent_->key == key && !stopping_ ? OwnerResult::Ok : OwnerResult::Busy;
    if (terminal_ && terminal_->key == key) return OwnerResult::NotActive;
    std::unique_ptr<CleanupPlan> next(new CleanupPlan);
    next->key = key;
    next->attachment = attachment;
    terminal_.reset();
    parent_ = std::move(next);
    stopping_ = false;
    return OwnerResult::Ok;
  }
  OwnerResult AuthorizeUnscoped(const Attachment& attachment, bool observation) const {
    if (retired_) return OwnerResult::Retired;
    if (!Attached(attachment)) return OwnerResult::WrongAttachment;
    return parent_ && !observation ? OwnerResult::Busy : OwnerResult::Ok;
  }
  OwnerResult Authorize(const Attachment& attachment, const Key& key) const {
    if (retired_) return OwnerResult::Retired;
    if (!Attached(attachment)) return OwnerResult::WrongAttachment;
    return parent_ && parent_->key == key && !stopping_ ? OwnerResult::Ok : OwnerResult::NotActive;
  }
  OwnerResult Effect(const Attachment& attachment, const Key& key) {
    const auto result = Authorize(attachment, key);
    if (result == OwnerResult::Ok) parent_->effects_started = true;
    return result;
  }
  OwnerResult InputAttempt(const Attachment& attachment, const Key& key, std::uint64_t port) {
    const auto result = Authorize(attachment, key);
    if (result != OwnerResult::Ok) return result;
    parent_->input_ports.insert(port); // Acquire the obligation before the native write.
    parent_->effects_started = true;
    return OwnerResult::Ok;
  }
  OwnerResult StartCleanup(const Attachment& attachment, const Key& key, CleanupPlan& plan) {
    if (retired_) return OwnerResult::Retired;
    if (!parent_ || !(parent_->key == key) || !(parent_->attachment == attachment)) return OwnerResult::NotActive;
    plan = *parent_;
    stopping_ = true;
    return OwnerResult::Ok;
  }
  OwnerResult Detach(const Attachment& attachment, std::unique_ptr<CleanupPlan>& plan) {
    plan.reset();
    if (retired_) return OwnerResult::Retired;
    if (!Attached(attachment)) return OwnerResult::Ok;
    if (parent_) plan.reset(new CleanupPlan(*parent_));
    attachment_.reset();
    terminal_.reset();
    if (parent_) stopping_ = true;
    return OwnerResult::Ok;
  }
  OwnerResult FinishCleanup(const CleanupPlan& plan, const CleanupEvidence& evidence) {
    if (retired_) return OwnerResult::Retired;
    if (!parent_ || !stopping_ || !(*parent_ == plan)) return OwnerResult::NotActive;
    if ((plan.effects_started && !evidence.stop_verified) || evidence.released_ports != plan.input_ports) {
      Retire();
      return OwnerResult::Retired;
    }
    if (Attached(plan.attachment)) terminal_ = std::move(parent_);
    else parent_.reset();
    stopping_ = false;
    return OwnerResult::Ok;
  }
  const CleanupPlan* Terminal(const Attachment& attachment, const Key& key) const {
    return !retired_ && terminal_ && terminal_->attachment == attachment && terminal_->key == key ? terminal_.get() : nullptr;
  }
  const Key* ActiveKey() const { return parent_ ? &parent_->key : nullptr; }
  void Retire() { retired_ = true; terminal_.reset(); }
  bool Retired() const { return retired_; }

 private:
  bool Attached(const Attachment& attachment) const { return attachment_ && *attachment_ == attachment; }
  const std::string runtime_;
  std::unique_ptr<Attachment> attachment_;
  std::unique_ptr<CleanupPlan> parent_, terminal_;
  bool stopping_ = false, retired_ = false;
};
} // namespace EmucapControl
