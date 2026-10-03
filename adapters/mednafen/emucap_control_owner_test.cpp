#include "emucap_control_owner.h"
#include <cassert>
#include <limits>

int main() {
  using namespace EmucapControl;
  const Attachment first("broker", std::numeric_limits<std::uint64_t>::max(), 1);
  const Attachment second("broker", first.registration, 2);
  const Key parent{"runtime", "owner", "parent"}, next{"runtime", "owner", "next"};
  for (int effects = 0; effects != 3; ++effects) {
    Ownership owner("runtime");
    assert(owner.Attach(first) == OwnerResult::Ok);
    assert(owner.Attach(second) == OwnerResult::Busy);
    assert(owner.Begin(second, parent) == OwnerResult::WrongAttachment);
    assert(owner.Begin(first, parent) == OwnerResult::Ok);
    assert(owner.Begin(first, parent) == OwnerResult::Ok);
    assert(owner.Begin(first, next) == OwnerResult::Busy);
    assert(owner.AuthorizeUnscoped(first, false) == OwnerResult::Busy);
    assert(owner.AuthorizeUnscoped(first, true) == OwnerResult::Ok);
    if (effects) assert(owner.Effect(first, parent) == OwnerResult::Ok);
    if (effects == 2) {
      assert(owner.InputAttempt(first, parent, 0) == OwnerResult::Ok);
      assert(owner.InputAttempt(first, parent, 0) == OwnerResult::Ok);
    }
    CleanupPlan plan;
    assert(owner.StartCleanup(first, parent, plan) == OwnerResult::Ok);
    assert(plan.effects_started == bool(effects));
    assert(plan.input_ports.size() == (effects == 2 ? 1u : 0u));
    assert(owner.InputAttempt(first, parent, 1) == OwnerResult::NotActive);
    CleanupPlan altered = plan; altered.input_ports.insert(7);
    assert(owner.FinishCleanup(altered, {}) == OwnerResult::NotActive);
    CleanupEvidence proof; proof.stop_verified = effects != 0;
    if (effects == 2) proof.released_ports.insert(0);
    assert(owner.FinishCleanup(plan, proof) == OwnerResult::Ok);
    assert(owner.Terminal(first, parent) && !owner.Terminal(second, parent));
    assert(owner.Begin(first, parent) == OwnerResult::NotActive);
    assert(owner.Begin(first, next) == OwnerResult::Ok);
    assert(!owner.Terminal(first, parent));
    assert(owner.FinishCleanup(plan, proof) == OwnerResult::NotActive);
    std::unique_ptr<CleanupPlan> detached;
    assert(owner.Detach(second, detached) == OwnerResult::Ok && !detached);
    assert(owner.ActiveKey() && *owner.ActiveKey() == next);
    assert(owner.Detach(first, detached) == OwnerResult::Ok && detached);
    assert(owner.Attach(second) == OwnerResult::Busy);
    const CleanupPlan old = *detached;
    assert(owner.FinishCleanup(old, {}) == OwnerResult::Ok);
    assert(owner.Attach(second) == OwnerResult::Ok);
    assert(owner.Begin(second, next) == OwnerResult::Ok);
    assert(owner.StartCleanup(second, next, plan) == OwnerResult::Ok);
    assert(owner.FinishCleanup(old, {}) == OwnerResult::NotActive);
    assert(owner.FinishCleanup(plan, {}) == OwnerResult::Ok);
  }
  // Failed native writes still require cleanup; missing/extra release evidence
  // or missing stop proof retires control across all subsequent attachments.
  for (int failure = 0; failure < 3; ++failure) {
    Ownership owner("runtime");
    assert(owner.Attach(first) == OwnerResult::Ok);
    assert(owner.Begin(first, parent) == OwnerResult::Ok);
    assert(owner.InputAttempt(first, parent, 0) == OwnerResult::Ok);
    CleanupPlan plan; assert(owner.StartCleanup(first, parent, plan) == OwnerResult::Ok);
    CleanupEvidence proof; proof.stop_verified = failure != 0;
    if (failure != 1) proof.released_ports.insert(0);
    if (failure == 2) proof.released_ports.insert(1);
    assert(owner.FinishCleanup(plan, proof) == OwnerResult::Retired);
    assert(owner.Retired());
    assert(owner.Attach(second) == OwnerResult::Retired);
    assert(owner.Begin(first, next) == OwnerResult::Retired);
    proof.stop_verified = true; proof.released_ports = {0};
    assert(owner.FinishCleanup(plan, proof) == OwnerResult::Retired);
    assert(!owner.Terminal(first, parent));
  }
  Ownership invalid("");
  assert(invalid.Attach(first) == OwnerResult::InvalidIdentity);
  Ownership owner("runtime");
  assert(owner.Attach(Attachment("broker", 0, 1)) == OwnerResult::InvalidIdentity);
  assert(owner.Attach(first) == OwnerResult::Ok);
  assert(owner.Begin(first, Key{"other", "owner", "parent"}) == OwnerResult::InvalidIdentity);
  owner.Retire();
  assert(owner.AuthorizeUnscoped(first, true) == OwnerResult::Retired);
}
