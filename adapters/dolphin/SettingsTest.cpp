// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
// Compile against the maintained native Config sources; no replacement cache or layer model.
#include "Common/Config/Config.h"
#include <atomic>
#include <cassert>
#include <functional>
#include <future>
#include <iostream>
#include <thread>

struct IO
{
  std::function<void(Config::Layer*)> load;
  std::function<void(Config::Layer*)> save;
};
class Loader final : public Config::ConfigLayerLoader
{
public:
  Loader(Config::LayerType type, std::shared_ptr<IO> io) : ConfigLayerLoader(type), m_io(std::move(io)) {}
  void Load(Config::Layer* layer) override { if (m_io->load) m_io->load(layer); }
  void Save(Config::Layer* layer) override { if (m_io->save) m_io->save(layer); }
private:
  std::shared_ptr<IO> m_io;
};

int main()
{
  const Config::Info<std::string> speed{{Config::System::Main, "Core", "EmulationSpeed"}, "1.0"};
  const Config::Info<std::string> other{{Config::System::Main, "Core", "OtherSetting"}, "default"};
  const auto& key = speed.GetLocation();
  Config::Init();
  auto current = Config::GetLayer(Config::LayerType::CurrentRun);
  Config::SetCurrent(speed, std::string("1.0"));
  assert(Config::Get(speed) == "1.0");
  const auto before = Config::GetSettingWriteRevision(key);
  current->Set(speed, std::string("4.0"));
  assert(Config::Get(speed) == "4.0"); // raw writes invalidate cache before publication
  current->Set(speed, std::string("1.0"));
  assert(Config::Get(speed) == "1.0" && Config::GetSettingWriteRevision(key) > before);
  const auto speed_revision = Config::GetSettingWriteRevision(key);
  current->Set(other, std::string("changed"));
  assert(Config::GetSettingWriteRevision(key) == speed_revision);
  auto section = current->GetSection(Config::System::Main, "Core");
  auto map = current->GetLayerMap();
  current->DeleteAllKeys();
  assert(Config::Get(speed) == "1.0" && !current->Exists(key));
  assert(map.at(key).value() == "1.0" && section.begin() != section.end());
  const auto deleted_revision = Config::GetSettingWriteRevision(key);
  current->Set(speed, std::string("2.0"));
  assert(Config::GetSettingWriteRevision(key) > deleted_revision);

  int callbacks = 0;
  auto callback = Config::AddConfigChangedCallback([&] {
    ++callbacks;
    bool available = false;
    std::thread contender([&] {
      Config::SettingsTransaction try_access(std::try_to_lock);
      available = try_access.OwnsLock();
    });
    contender.join();
    assert(available); // a recursive lock on this thread would hide an incorrectly held gate
  });
  {
    Config::SettingsTransaction outer;
    Config::SetCurrent(speed, std::string("4.0"));
    { Config::SettingsTransaction nested; Config::SetCurrent(speed, std::string("2.0")); }
    assert(Config::Get(speed) == "2.0" && callbacks == 0);
  }
  assert(callbacks == 1);
  { Config::SettingsTransaction read_only; assert(Config::Get(speed) == "2.0"); }
  assert(callbacks == 1);
  Config::RemoveConfigChangedCallback(callback);
  {
    Config::SettingsTransaction occupied;
    bool busy = false;
    std::thread contender([&] {
      Config::SettingsTransaction try_access(std::try_to_lock);
      busy = !try_access.OwnsLock();
    });
    contender.join();
    assert(busy);
  }
  auto writer = [&](const std::string& value) {
    for (int i = 0; i < 20000; ++i)
    {
      Config::SettingsTransaction operation;
      Config::SetCurrent(speed, value);
      assert(Config::Get(speed) == value);
    }
  };
  std::thread first(writer, "4.0"), second(writer, "0.5");
  first.join(); second.join();
  callbacks = 0;
  callback = Config::AddConfigChangedCallback([&] {
    ++callbacks;
    Config::SettingsTransaction clamp;
    if (Config::Get(speed) == "0.01") Config::SetCurrent(speed, std::string("1.0"));
  });
  Config::SetCurrent(speed, std::string("0.01"));
  assert(callbacks == 2 && Config::Get(speed) == "1.0");
  Config::RemoveConfigChangedCallback(callback);

  auto io = std::make_shared<IO>();
  io->load = [&](Config::Layer* layer) { layer->Set(speed, std::string("3.0")); };
  auto view_revision = Config::GetLayerViewRevision();
  Config::AddLayer(std::make_unique<Loader>(Config::LayerType::Base, io));
  assert(Config::GetLayerViewRevision() > view_revision);
  Config::ClearCurrentRunLayer();
  assert(Config::Get(speed) == "3.0");
  auto base = Config::GetLayer(Config::LayerType::Base);
  const auto active_revision = Config::GetSettingWriteRevision(key);
  current->Set(speed, std::string("9.0")); // retired layers cannot change active-setting stamps
  assert(Config::GetSettingWriteRevision(key) == active_revision && Config::Get(speed) == "3.0");

  // A reload parses without the gate, preserves concurrent writes to the same key, and still
  // applies its other keys. The caller-controlled barrier proves the overlap without sleeps.
  std::promise<void> loading, finish_load;
  auto load_release = finish_load.get_future();
  io->load = [&](Config::Layer* layer) {
    loading.set_value(); load_release.wait();
    layer->Set(speed, std::string("5.0"));
    layer->Set(other, std::string("loaded"));
  };
  std::thread loader([&] { base->Load(); });
  loading.get_future().wait();
  { Config::SettingsTransaction access(std::try_to_lock); assert(access.OwnsLock());
    base->Set(speed, std::string("8.0")); }
  finish_load.set_value(); loader.join();
  assert(Config::Get(speed) == "8.0" && base->Get(other) == "loaded");

  // A write during save must remain dirty, and a save sees its own consistent snapshot.
  std::promise<void> saving, finish_save;
  auto save_release = finish_save.get_future();
  int saves = 0;
  io->save = [&](Config::Layer* layer) {
    ++saves; assert(layer->Get(speed) == "8.0");
    saving.set_value(); save_release.wait();
    assert(layer->Get(speed) == "8.0");
  };
  std::thread saver([&] { base->Save(); });
  saving.get_future().wait();
  { Config::SettingsTransaction access(std::try_to_lock); assert(access.OwnsLock());
    base->Set(speed, std::string("6.0")); }
  finish_save.set_value(); saver.join();
  io->save = [&](Config::Layer* layer) { ++saves; assert(layer->Get(speed) == "6.0"); };
  base->Save(); base->Save();
  assert(saves == 2);
  io->load = [&](Config::Layer* layer) { layer->Set(speed, std::string("7.0")); };
  base->Load(); assert(Config::Get(speed) == "7.0");
  base->Save(); assert(saves == 2); // clean reload does not invent a dirty write
  view_revision = Config::GetLayerViewRevision();
  Config::RemoveLayer(Config::LayerType::Base);
  assert(Config::Get(speed) == "1.0" && Config::GetLayerViewRevision() > view_revision);
  Config::Shutdown();
  std::cout << "Native settings ownership: mutation, cache, callbacks, contention, topology, reload and save passed\n";
}
