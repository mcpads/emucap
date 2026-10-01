// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#include <cassert>
#include <cstdlib>
#include "Core/System.h"
#include "DolphinNoGUI/Platform.h"

class TestPlatform final : public Platform
{
public:
  void MainLoop() override { UpdateRunningFlag(); }
  WindowSystemInfo GetWindowSystemInfo() const override { return {}; }
};

static void SetLaunchMode(const char* port)
{
#ifdef _WIN32
  _putenv_s("EMUCAP_PORT", port ? port : "");
#else
  if (port)
    setenv("EMUCAP_PORT", port, 1);
  else
    unsetenv("EMUCAP_PORT");
#endif
}

int main()
{
  // Controlled shutdown must not consult uninitialized or retired native devices.
  for (bool ios : {false, true})
  {
    TestShutdown::ios_present = ios;
    TestShutdown::hook_installed = true;
    TestShutdown::device_reads = TestShutdown::power_presses = 0;
    SetLaunchMode("12345");
    TestPlatform managed;
    SetLaunchMode(nullptr);
    managed.RequestShutdown();
    managed.MainLoop();
    assert(!managed.IsRunning());
    assert(TestShutdown::device_reads == 0 && TestShutdown::power_presses == 0);
  }

  // A launch without the marker retains native guest-power acknowledgement behavior.
  for (const char* absent : {static_cast<const char*>(nullptr), ""})
  {
    SetLaunchMode(absent);
    TestShutdown::ios_present = TestShutdown::hook_installed = true;
    TestShutdown::power_presses = 0;
    TestPlatform unmanaged;
    SetLaunchMode("12345");
    unmanaged.RequestShutdown();
    unmanaged.MainLoop();
    assert(unmanaged.IsRunning() && TestShutdown::power_presses == 1);
    unmanaged.RequestShutdown();
    unmanaged.MainLoop();
    assert(!unmanaged.IsRunning() && TestShutdown::power_presses == 1);
  }
  SetLaunchMode(nullptr);
  TestShutdown::hook_installed = false;
  TestShutdown::power_presses = 0;
  TestPlatform no_hook;
  no_hook.RequestShutdown();
  no_hook.MainLoop();
  assert(!no_hook.IsRunning() && TestShutdown::power_presses == 0);
}
