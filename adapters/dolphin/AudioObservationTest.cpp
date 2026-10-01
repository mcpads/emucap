// Tests host audio evidence independently of guest execution and device availability.
#include "EmuCapAudio.h"

#include <atomic>
#include <cassert>
#include <string_view>
#include <thread>

using AudioCommon::OutputObservationOwner;
using namespace std::literals;

int main()
{
  OutputObservationOwner owner;
  assert(owner.Read().phase == "absent"sv);
  const auto init = owner.BeginInitialization();
  assert(owner.Read().phase == "initializing"sv);
  owner.Constructed(init, "native");
  owner.Initialized(init, true);
  assert(owner.Read().initialized && !owner.Read().start_verified);

  auto command = owner.BeginRun(true);
  assert(owner.Read().phase == "starting"sv);
  owner.FinishedRun(command, true, false);
  auto observed = owner.Read();
  assert(observed.last_run_result == "failed"sv);
  assert(observed.failure == "start_failed"sv && !observed.start_verified);
  // An intent-based native no-op performs no publication and cannot clear this failure.
  assert(owner.Read().failure == "start_failed"sv);

  command = owner.BeginRun(true);
  owner.FinishedRun(command, true, true);
  command = owner.BeginRun(false);
  owner.FinishedRun(command, false, true);
  observed = owner.Read();
  assert(observed.phase == "ready"sv && observed.last_run_result == "stopped"sv);
  assert(observed.start_verified && !observed.failure);
  command = owner.BeginRun(false);
  owner.FinishedRun(command, false, false);
  assert(owner.Read().failure == "stop_failed"sv && owner.Read().start_verified);

  const auto previous_generation = owner.Read().generation;
  const auto replacement = owner.BeginInitialization();
  owner.Constructed(replacement, "No Audio Output", "backend_initialization_failed");
  owner.Initialized(replacement, true);
  // Neither an old initializer nor a delayed transition owns the replacement stream.
  owner.Constructed(init, "stale");
  owner.Initialized(init, false);
  owner.FinishedRun(command, true, true);
  observed = owner.Read();
  assert(observed.generation != previous_generation);
  assert(observed.backend == "No Audio Output" && observed.initialized);
  assert(!observed.start_verified && observed.last_run_result == "unattempted"sv);
  assert(!observed.failure && observed.fallback == "backend_initialization_failed"sv);

  const auto closing = owner.BeginClose();
  assert(owner.Read().phase == "closing"sv);
  owner.Closed(closing);
  owner.Initialized(replacement, true);
  observed = owner.Read();
  assert(observed.phase == "absent"sv && !observed.initialized && observed.backend.empty());

  const auto failed_init = owner.BeginInitialization();
  owner.Constructed(failed_init, "broken");
  owner.Initialized(failed_init, false);
  command = owner.BeginRun(true);
  owner.FinishedRun(command, true, true);
  assert(!owner.Read().initialized && owner.Read().failure == "initialization_failed"sv);

  // A blocked device command does not retain the observation lock. Other threads can copy
  // the pending evidence, and repeated lifetime changes never expose a torn metadata tuple.
  std::atomic<bool> finished = false;
  std::thread writer([&] {
    for (int i = 0; i < 10000; ++i)
    {
      const auto token = owner.BeginInitialization();
      owner.Constructed(token, "concurrent");
      owner.Initialized(token, true);
      auto run = owner.BeginRun(true);
      owner.FinishedRun(run, true, true);
      owner.Closed(owner.BeginClose());
    }
    finished = true;
  });
  while (!finished)
  {
    const auto view = owner.Read();
    if (view.phase == "absent"sv)
      assert(!view.initialized && !view.start_verified && view.backend.empty());
    if (view.initialized)
      assert(!view.backend.empty());
  }
  writer.join();
}
