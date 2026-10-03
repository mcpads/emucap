// Compare native ownership semantics with the established Dolphin producer.
// This is separate from the C++11 test because Dolphin's header requires C++20.
#include "emucap_control_owner.h"
#include "../dolphin/EmuCapOwner.h"
#include <cassert>

namespace M = EmucapControl;
namespace D = EmuCap::Temporal;
M::OwnerResult result(D::OwnerResult value) {
  switch (value) {
    case D::OwnerResult::Ok: return M::OwnerResult::Ok;
    case D::OwnerResult::InvalidIdentity: return M::OwnerResult::InvalidIdentity;
    case D::OwnerResult::WrongAttachment: return M::OwnerResult::WrongAttachment;
    case D::OwnerResult::Busy: return M::OwnerResult::Busy;
    case D::OwnerResult::NotActive: return M::OwnerResult::NotActive;
    case D::OwnerResult::Retired: return M::OwnerResult::Retired;
  }
  assert(false); return M::OwnerResult::Retired;
}
int main() {
  const M::Attachment attachments[] = {{"broker", 1, 1}, {"broker", 1, 2}, {"broker", 2, 1}};
  const M::Key keys[] = {{"runtime", "owner", "parent"}, {"runtime", "owner", "next"}, {"other", "owner", "parent"}};
  std::uint32_t seed = 0x3b451;
  const auto draw = [&seed](unsigned limit) { seed = seed * 1664525u + 1013904223u; return (seed >> 8) % limit; };
  unsigned seen = 0;
  for (unsigned trial = 0; trial < 128; ++trial) {
    M::Ownership native("runtime"); D::ProducerOwnership reference("runtime");
    assert(native.Attach(attachments[0]) == result(reference.Attach({"broker", 1, 1})));
    assert(native.Begin(attachments[0], keys[0]) == result(reference.Begin({"broker", 1, 1}, {"runtime", "owner", "parent"})));
    M::CleanupPlan saved; D::CleanupPlan expected;
    for (unsigned index = 0; index < 128; ++index) {
      const auto& a = attachments[draw(3)]; const auto& k = keys[draw(3)];
      const D::Attachment da{a.broker_instance, a.registration, a.session};
      const D::OperationKey dk{k.runtime, k.owner_id, k.operation_id};
      M::OwnerResult actual; D::OwnerResult wanted;
      const unsigned action = draw(9); seen |= 1u << action;
      switch (action) {
        case 0: actual = native.Attach(a); wanted = reference.Attach(da); break;
        case 1: actual = native.Begin(a, k); wanted = reference.Begin(da, dk); break;
        case 2: actual = native.Authorize(a, k); wanted = reference.Authorize(da, dk); break;
        case 3: {
          const bool observation = draw(2);
          actual = native.AuthorizeUnscoped(a, observation); wanted = reference.AuthorizeUnscoped(da, observation); break;
        }
        case 4: actual = native.Effect(a, k); wanted = reference.Effect(da, dk); break;
        case 5: {
          const auto port = draw(2);
          actual = native.InputAttempt(a, k, port); wanted = reference.Effect(da, dk, port); break;
        }
        case 6: actual = native.StartCleanup(a, k, saved); wanted = reference.StartCleanup(da, dk, expected); break;
        case 7: {
          std::unique_ptr<M::CleanupPlan> plan; std::optional<D::CleanupPlan> other;
          actual = native.Detach(a, plan); wanted = reference.Detach(da, other);
          assert(bool(plan) == bool(other));
          if (plan) { saved = *plan; expected = *other; }
          break;
        }
        default: {
          M::CleanupEvidence proof; D::CleanupEvidence other;
          proof.stop_verified = other.stop_verified = draw(2);
          const unsigned ports = draw(4);
          for (unsigned port = 0; port < 2; ++port)
            if (ports & (1u << port)) { proof.released_ports.insert(port); other.released_ports.insert(port); }
          actual = native.FinishCleanup(saved, proof); wanted = reference.FinishCleanup(expected, other); break;
        }
      }
      assert(actual == result(wanted));
      const auto active = native.ActiveKey(); const auto ref = reference.ActiveKey();
      assert(bool(active) == bool(ref));
      if (active) assert(active->runtime == ref->runtime && active->owner_id == ref->owner_id && active->operation_id == ref->operation_id);
      const auto terminal = native.Terminal(a, k); const auto retained = reference.Terminal(da, dk);
      assert(bool(terminal) == bool(retained));
      if (terminal) assert(terminal->input_ports == retained->input_ports && terminal->effects_started == retained->effects_started);
    }
  }
  assert(seen == (1u << 9) - 1);
}
