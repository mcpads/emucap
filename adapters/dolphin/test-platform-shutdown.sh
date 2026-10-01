#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${DOLPHIN_SOURCE_DIR:-$HERE/work/dolphin-src}/Source/Core"
SHUTDOWN_TEST_TMP="$(mktemp -d "${TMPDIR:-/tmp}/emucap-dolphin-shutdown.XXXXXX")"
trap 'rm -rf "$SHUTDOWN_TEST_TMP"' EXIT
mkdir -p "$SHUTDOWN_TEST_TMP/Core/HW" "$SHUTDOWN_TEST_TMP/Core/IOS/STM"
# Compile the real platform implementation; only guest devices are replaced by counters.
cat > "$SHUTDOWN_TEST_TMP/Core/System.h" <<'CPP'
#pragma once
#include <memory>
#include <string>
namespace TestShutdown {
inline bool ios_present = true, hook_installed = true;
inline int device_reads = 0, power_presses = 0;
}
namespace IOS::HLE {
class STMEventHookDevice {
public:
  bool HasHookInstalled() const { return TestShutdown::hook_installed; }
};
class Kernel {
public:
  std::shared_ptr<STMEventHookDevice> GetDeviceByName(const std::string&) const {
    return std::make_shared<STMEventHookDevice>();
  }
};
}
namespace Core {
class ProcessorInterface {
public:
  void PowerButton_Tap() { ++TestShutdown::power_presses; }
};
class System {
public:
  static System& GetInstance() { static System instance; return instance; }
  std::shared_ptr<IOS::HLE::Kernel> GetIOS() const {
    ++TestShutdown::device_reads;
    return TestShutdown::ios_present ? std::make_shared<IOS::HLE::Kernel>() : nullptr;
  }
  ProcessorInterface& GetProcessorInterface() const { static ProcessorInterface pi; return pi; }
};
}
CPP
for stub in Core/HW/ProcessorInterface.h Core/IOS/IOS.h Core/IOS/STM/STM.h; do
  printf '#include "Core/System.h"\n' > "$SHUTDOWN_TEST_TMP/$stub"
done
"${CXX:-c++}" -std=c++20 -Wall -Wextra -Werror -Wno-unused-parameter \
  -I"$SHUTDOWN_TEST_TMP" -I"$SRC" "$HERE/PlatformShutdownTest.cpp" \
  "$SRC/DolphinNoGUI/Platform.cpp" -o "$SHUTDOWN_TEST_TMP/test"
"$SHUTDOWN_TEST_TMP/test"
echo '[dolphin-shutdown] managed host stop and native guest-power policy passed'
