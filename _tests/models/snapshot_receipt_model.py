#!/usr/bin/env python3
"""Explore the proposed snapshot receipt safety rules, not a product implementation.

One logical request key, bounded attempts, atomic abstract publication, and independent
connection/runtime changes. This does not prove filesystem atomicity or timed liveness.
Run from any directory with Python 3.10 or later; no media or emulator is required.
"""
from collections import deque
from dataclasses import dataclass, replace
from itertools import product


@dataclass(frozen=True)
class State:
    phase: str = "new"
    safe: bool = True
    bound: bool = True
    lease: bool = True
    connected: bool = True
    alive: bool = True
    owned: bool = False
    attempts: int = 0
    committed: bool = False
    body: bool = False
    receipt: bool = False
    released: bool = False
    exported: bool = False
    outcome: str = "pending"


def ready(s):
    return s.safe and s.bound and s.lease and s.alive and s.connected


def terminal(s, outcome):
    return replace(s, phase="terminal", owned=False, outcome=outcome)


def successors(s, mutation=None):
    if s.phase == "new":
        if ready(s):
            yield "reserve key and fingerprint", replace(s, phase="reserved", owned=True)
        else:
            yield "reject admission", terminal(s, "rejected")
    if s.phase == "reserved":
        yield "record serialization intent", replace(s, phase="capturing")
    if s.phase == "capturing":
        if ready(s) or mutation == "serialize_without_boundary":
            yield "serialize and seal", replace(s, phase="sealed", attempts=s.attempts + 1)
        else:
            yield "reject changed boundary", terminal(s, "failed")
    if s.phase == "sealed" and ready(s):
        yield "commit pair", replace(s, phase="published", committed=True, body=True,
                                      receipt=mutation != "publish_half_pair")
    if s.phase == "published":
        if ready(s):
            yield "export and complete", replace(terminal(s, "completed"), exported=True)
        yield "export fails", terminal(s, "export_failed")
    if s.phase not in {"new", "terminal"}:
        yield "I/O failure or deadline", terminal(s, "failed")
        # A lost proof is observed before any subsequent serialization or commit.
        if s.safe:
            yield "boundary proof lost", replace(s, safe=False)
        if s.bound:
            yield "generation binding lost", replace(s, bound=False)
        if s.lease:
            yield "lease lost", replace(s, lease=False)
        yield "crash and recover", terminal(s, "issued" if s.committed else "indeterminate")
    if s.connected:
        disconnected = replace(s, connected=False)
        if s.phase not in {"new", "terminal"}:
            disconnected = terminal(disconnected, "aborted")
        if mutation == "disconnect_rolls_back" and s.committed and not s.released:
            disconnected = replace(disconnected, committed=False, body=False, receipt=False)
        yield "disconnect", disconnected
    if s.alive:
        stopped = replace(s, alive=False)
        if s.phase not in {"new", "terminal"}:
            stopped = terminal(stopped, "aborted")
        yield "runtime terminates", stopped
    if s.phase == "terminal":
        # Same-key observation is read-only; a different fingerprint is a conflict.
        yield "same-key replay observes original result", s
        yield "different fingerprint rejected", s
        if mutation == "replay_serializes_again" and s.attempts == 1:
            yield "incorrect replay", replace(s, attempts=2)
        if s.committed and not s.released:
            yield "explicit retention release with no readers", replace(
                s, released=True, body=False, receipt=False)


def violation(before, action, after):
    if action == "serialize and seal" and not (ready(before) and before.owned):
        return "serialization without current safe ownership"
    if after.attempts > 1:
        return "same key serialized more than once"
    if after.body != after.receipt:
        return "only half of the pair is publicly retained"
    if before.committed and not after.committed:
        return "historical commit was rolled back"
    if after.committed and not after.released and not (after.body and after.receipt):
        return "committed pair disappeared without retention release"
    if after.phase == "terminal" and after.owned:
        return "terminal request retains transient ownership"
    if after.outcome == "completed" and not (after.committed and after.exported):
        return "completed save lacks receipt or requested export"
    if after.outcome == "rejected" and (after.attempts or after.exported):
        return "admission rejection changed snapshot or destination"
    if action == "export and complete" and not ready(before):
        return "successful save after losing live boundary ownership"
    if action == "commit pair" and not ready(before):
        return "commit uses a lost generation or boundary"
    return None


def explore(mutation=None):
    initial = [State(safe=safe, bound=bound, lease=lease)
               for safe, bound, lease in product([False, True], repeat=3)]
    parents = {s: None for s in initial}
    queue = deque(initial)
    edges = 0
    while queue:
        before = queue.popleft()
        for action, after in successors(before, mutation):
            edges += 1
            error = violation(before, action, after)
            if error:
                trace = [action]
                cursor = before
                while parents[cursor] is not None:
                    cursor, previous_action = parents[cursor]
                    trace.append(previous_action)
                return len(parents), edges, error, list(reversed(trace))
            if after not in parents:
                parents[after] = (before, action)
                queue.append(after)
    return len(parents), edges, None, []


def verification_table():
    # These independent predicates stand for obligations discharged by future implementations.
    # The table checks the distinction between historical validity and requested applicability.
    checked = 0
    for well_formed, address, payload, issued, expected in product([False, True], repeat=5):
        integrity = well_formed and address and payload
        verified = integrity and issued
        verified_for = verified and expected
        assert not verified or issued
        assert not verified_for or (expected and payload and address)
        if verified and not expected:
            assert not verified_for  # A valid historical receipt may be borrowed for a new context.
        if integrity and not issued:
            assert not verified  # Caller-authored content with a correct hash is insufficient.
        checked += 1
    return checked


def main():
    states, edges, error, trace = explore()
    assert error is None, (error, trace)
    print(f"base model: {states} states, {edges} transitions; safety checks passed")
    for mutation in ["serialize_without_boundary", "publish_half_pair",
                     "disconnect_rolls_back", "replay_serializes_again"]:
        _, _, error, trace = explore(mutation)
        assert error is not None, f"mutation escaped checks: {mutation}"
        print(f"counterexample [{mutation}]: {' -> '.join(trace)}; {error}")
    print(f"verification predicates: {verification_table()} combinations checked")
    print("Scope: finite abstract safety only; no timed-liveness or native/filesystem proof.")


if __name__ == "__main__":
    main()
