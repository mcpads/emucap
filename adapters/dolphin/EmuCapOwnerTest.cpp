// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#include "EmuCapOwner.h"
#include <cassert>
#include <limits>

int main()
{
  using namespace EmuCap::Temporal;
  const Attachment first{"broker", std::numeric_limits<uint64_t>::max(), 1};
  const Attachment second{"broker", first.registration, 2};
  const OperationKey key{"runtime", "owner", "parent"};
  const OperationKey next{"runtime", "owner", "next"};
  for (bool input : {false, true})
  {
    ProducerOwnership owner("runtime");
    assert(owner.Attach(first) == OwnerResult::Ok);
    assert(owner.Begin(second, key) == OwnerResult::WrongAttachment);
    assert(owner.Begin(first, key) == OwnerResult::Ok);
    assert(owner.Begin(first, key) == OwnerResult::Ok);
    assert(owner.Begin(first, next) == OwnerResult::Busy);
    assert(owner.AuthorizeUnscoped(first, false) == OwnerResult::Busy);
    assert(owner.AuthorizeUnscoped(first, true) == OwnerResult::Ok);
    if (input) assert(owner.Effect(first, key, 0) == OwnerResult::Ok);
    CleanupPlan plan;
    assert(owner.StartCleanup(first, key, plan) == OwnerResult::Ok);
    assert(owner.Effect(first, key, 1) == OwnerResult::NotActive);
    CleanupEvidence proof;
    if (input) { proof.stop_verified = true; proof.released_ports.insert(0); }
    assert(owner.FinishCleanup(plan, proof) == OwnerResult::Ok);
    assert(owner.Terminal(first, key) && !owner.Terminal(second, key));
    assert(owner.Begin(first, key) == OwnerResult::NotActive);
    assert(owner.Begin(first, next) == OwnerResult::Ok);
    assert(!owner.Terminal(first, key));
    assert(owner.FinishCleanup(plan, proof) == OwnerResult::NotActive);
    std::optional<CleanupPlan> detached;
    assert(owner.Detach(second, detached) == OwnerResult::Ok && !detached);
    assert(owner.ActiveKey() == next);
    assert(owner.Detach(first, detached) == OwnerResult::Ok && detached);
    assert(owner.Attach(second) == OwnerResult::Busy);
    assert(owner.FinishCleanup(*detached, {}) == OwnerResult::Ok);
    assert(owner.Attach(second) == OwnerResult::Ok);
  }
  for (int failure = 0; failure < 3; ++failure)
  {
    ProducerOwnership owner("runtime");
    assert(owner.Attach(first) == OwnerResult::Ok);
    assert(owner.Begin(first, key) == OwnerResult::Ok);
    assert(owner.Effect(first, key, 0) == OwnerResult::Ok);
    CleanupPlan plan;
    assert(owner.StartCleanup(first, key, plan) == OwnerResult::Ok);
    CleanupEvidence evidence{failure != 0, {0}};
    if (failure == 1) evidence.released_ports.clear();
    if (failure == 2) evidence.released_ports.insert(1);
    assert(owner.FinishCleanup(plan, evidence) == OwnerResult::Retired);
    assert(owner.Attach(second) == OwnerResult::Retired);
    assert(owner.Begin(first, next) == OwnerResult::Retired);
    assert(owner.FinishCleanup(plan, {true, {0}}) == OwnerResult::Retired);
    assert(!owner.Terminal(first, key));
  }
}
