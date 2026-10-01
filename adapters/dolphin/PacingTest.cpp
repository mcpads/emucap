// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#include "EmuCapPacing.h"
#include "EmuCapTemporal.h"
#include "Common/Config/Config.h"
#include <cassert>
#include <future>
#include <iostream>
#include <thread>

class EmptyLayer final : public Config::ConfigLayerLoader
{
public:
  EmptyLayer() : ConfigLayerLoader(Config::LayerType::CommandLine) {}
  void Load(Config::Layer*) override {}
  void Save(Config::Layer*) override {}
};

int main()
{
  using namespace EmuCap;
  const Config::Info<std::string> speed{{Config::System::Main, "Core", "EmulationSpeed"}, "1"};
  const Config::Info<std::string> other{{Config::System::Main, "Core", "Other"}, "false"};
  Config::Init();
  bool override = true;
  uint64_t override_revision = 1;
  const auto read = [&] {
    return Pacing::Observation{std::stof(Config::Get(speed)), override,
      {Config::GetSettingWriteRevision(speed.GetLocation()), Config::GetLayerViewRevision(),
       override_revision}};
  };
  const auto write = [&](float target) {
    Config::SetCurrent(speed, std::to_string(target));
    if (override) { override = false; ++override_revision; }
  };
  const auto admit = [] { return true; };
  auto apply = [&](float target) { return Pacing::Apply(target, admit, read, write); };
  auto first = apply(4.0f);
  assert(first.outcome == Pacing::Outcome::Completed);
  assert(first.previous.speed == 1.0f && first.previous.temp_disabled);
  assert(first.verified.speed == 4.0f && !first.verified.temp_disabled);
  Config::SetCurrent(speed, std::string("2.0"));
  assert(first.verified.speed == 4.0f); // reply retains the verified instant
  assert(apply(3.0f).previous.speed == 2.0f);

  {
    Config::SettingsTransaction occupied;
    Pacing::Outcome outcome;
    std::thread contender([&] { outcome = apply(8.0f).outcome; });
    contender.join();
    assert(outcome == Pacing::Outcome::Busy && std::stof(Config::Get(speed)) == 3.0f);
  }
  assert(Pacing::Apply(8.0f, [] { return false; }, read, write).outcome ==
         Pacing::Outcome::Cancelled);
  assert(std::stof(Config::Get(speed)) == 3.0f);

  // A callback's A->B->A write must conflict even though readback equals the target.
  bool changed = false;
  auto callback = Config::AddConfigChangedCallback([&] {
    if (changed) return;
    changed = true;
    Config::SetCurrent(speed, std::string("2.0"));
    Config::SetCurrent(speed, std::string("4.0"));
  });
  auto aba = apply(4.0f);
  assert(aba.outcome == Pacing::Outcome::Unverified && aba.verified.speed == 4.0f);
  assert(aba.applied.revision != aba.verified.revision);
  Config::RemoveConfigChangedCallback(callback);

  callback = Config::AddConfigChangedCallback([&] { Config::SetCurrent(other, std::string("true")); });
  assert(apply(3.0f).outcome == Pacing::Outcome::Completed);
  Config::RemoveConfigChangedCallback(callback);
  callback = Config::AddConfigChangedCallback([&] {
    if (std::stof(Config::Get(speed)) < 1.0f) Config::SetCurrent(speed, std::string("1.0"));
  });
  auto clamped = apply(0.5f);
  assert(clamped.outcome == Pacing::Outcome::Unverified && std::stof(Config::Get(speed)) == 1.0f);
  Config::RemoveConfigChangedCallback(callback);

  callback = Config::AddConfigChangedCallback([&] {
    Config::SettingsTransaction tx;
    override = true;
    ++override_revision;
  });
  assert(apply(2.0f).outcome == Pacing::Outcome::Unverified);
  assert(override && std::stof(Config::Get(speed)) == 2.0f); // no stale rollback
  Config::RemoveConfigChangedCallback(callback);

  changed = false;
  callback = Config::AddConfigChangedCallback([&] {
    if (changed) return;
    changed = true;
    Config::AddLayer(std::make_unique<EmptyLayer>());
  });
  assert(apply(3.0f).outcome == Pacing::Outcome::Unverified);
  Config::RemoveConfigChangedCallback(callback);

  // Hold the gate only after application/notification: this cannot be reported as busy.
  std::promise<void> held, release;
  auto released = release.get_future();
  std::thread holder;
  callback = Config::AddConfigChangedCallback([&] {
    holder = std::thread([&] {
      Config::SettingsTransaction tx;
      held.set_value();
      released.wait();
    });
    held.get_future().wait();
  });
  assert(apply(4.0f).outcome == Pacing::Outcome::Unverified);
  release.set_value();
  holder.join();
  Config::RemoveConfigChangedCallback(callback);

  // Deadline while a native callback owns the result construction. The caller does not inspect
  // unpublished fields; draining the callback cannot turn the job back into Completed.
  std::promise<void> entered, drain;
  auto drained = drain.get_future();
  callback = Config::AddConfigChangedCallback([&] { entered.set_value(); drained.wait(); });
  auto job = std::make_shared<Temporal::HostJob>(Temporal::HostJob::Clock::now() +
                                               std::chrono::milliseconds(100));
  Pacing::Change unpublished;
  std::thread worker([&] { job->Execute(admit, [&] { unpublished = apply(5.0f); return true; }); });
  entered.get_future().wait();
  assert(job->Wait() == Temporal::HostJobOutcome::Unverified);
  drain.set_value();
  worker.join();
  assert(job->Wait() == Temporal::HostJobOutcome::Unverified);
  assert(unpublished.verified.speed == 5.0f);
  Config::RemoveConfigChangedCallback(callback);
  Config::Shutdown();
  std::cout << "Native pacing: ownership, ABA, callback clamp, contention and deadline passed\n";
}
