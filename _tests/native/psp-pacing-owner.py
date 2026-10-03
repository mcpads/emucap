#!/usr/bin/env python3
"""Exercise the maintained PSP pacing owner with concurrent readers and writers."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True, help='patched Core/EmucapPacing.h')
parser.add_argument('--sanitizer', choices=['address,undefined', 'thread'], default='address,undefined')
args = parser.parse_args()
network_source = (args.source.parent / 'HLE/sceNet.cpp').read_text()
start = network_source.index('bool NetworkAllowSpeedControl()')
end = network_source.index('\n}', start) + 2
network_function = network_source[start:end]
apctl_functions = ''
for signature in ['static void SetNetApctlState(', 'bool __NetApctlConnected()']:
    start = network_source.index(signature)
    end = network_source.index('\n}', start) + 2
    apctl_functions += network_source[start:end] + '\n'
adhoc_source = (args.source.parent / 'HLE/sceNetAdhoc.cpp').read_text()
adhoc_functions = ''
for signature in ['void __NetSetAdhocctlState(', 'bool __NetAdhocConnected()']:
    start = adhoc_source.index(signature)
    end = adhoc_source.index('\n}', start) + 2
    adhoc_functions += adhoc_source[start:end] + '\n'
code = r'''
#include "EmucapPacing.h"
#include <algorithm>
#include <atomic>
#include <cassert>
#include <thread>
#include <vector>
#include <cstdio>
static EmucapPacingOwner networkOwner;
static bool connected = false;
bool IsNetworkConnected() { return connected; }
EmucapPacingSnapshot __DisplayGetEmucapPacing() { return networkOwner.Read(); }
using u32 = uint32_t;
static u32 netApctlState;
constexpr u32 PSP_NET_APCTL_STATE_GOT_IP = 4;
void __DisplayPublishEmucapApctlState(uint32_t &native, uint32_t value, bool active) {
    networkOwner.PublishApctlState(native, value, active);
}
static int adhocctlState = 0;
constexpr int ADHOCCTL_STATE_CONNECTED = 1, ADHOCCTL_STATE_GAMEMODE = 3;
void __DisplayPublishEmucapAdhocState(int &native, int value, bool active) {
    networkOwner.PublishAdhocState(native, value, active);
}
''' + network_function + '\n' + apctl_functions + adhoc_functions + r'''
int main() {
    for (bool connection : {false, true}) for (bool permission : {false, true}) {
        SetNetApctlState(connection ? 4 : 0);
        networkOwner.SetNetworkSpeedPermission(permission);
        assert(NetworkAllowSpeedControl() == (!connection || permission));
    }

    for (uint32_t state : {0u, 3u, 4u, 5u, 0u}) {
        SetNetApctlState(state);
        assert(netApctlState == state && __NetApctlConnected() == (state >= 4));
    }
    bool nativeInit = false, nativeControlInit = false;
    for (bool init : {false, true}) for (bool ctl : {false, true}) {
        networkOwner.PublishAdhocInit(nativeInit, init, false);
        networkOwner.PublishAdhocInit(nativeControlInit, ctl, true);
        for (int state : {0, 1, 2, 3}) {
            __NetSetAdhocctlState(state);
            assert(adhocctlState == state);
            assert(__NetAdhocConnected() == (init && ctl && (state == 1 || state == 3)));
        }
    }
    EmucapPacingOwner owner;
    assert(owner.Read().percent == 100 && owner.Read().revision == 0);
    std::atomic<bool> done{false};
    std::vector<std::thread> readers;
    for (int i = 0; i < 4; ++i) readers.emplace_back([&] {
        do {
            auto value = owner.Read();
            assert(value.percent == (value.revision == 0 ? 100 : int(value.revision % 10001)));
        } while (!done.load());
    });
    for (unsigned i = 1; i <= 20000; ++i) {
        auto update = owner.SetPercent(int(i % 10001));
        assert(update.previous.revision == i - 1 && update.applied.revision == i);
    }
    done = true;
    for (auto &reader : readers) reader.join();

    EmucapPacingOwner competing;
    std::vector<EmucapPacingUpdate> a, b;
    auto write = [&](auto &updates, int percent) {
        for (int i = 0; i < 4000; ++i) updates.push_back(competing.SetPercent(percent));
    };
    std::thread first([&] { write(a, 50); });
    std::thread second([&] { write(b, 400); });
    first.join(); second.join();
    a.insert(a.end(), b.begin(), b.end());
    std::sort(a.begin(), a.end(), [](auto x, auto y) { return x.applied.revision < y.applied.revision; });
    EmucapPacingSnapshot previous;
    for (auto update : a) {
        assert(update.previous.revision == previous.revision);
        assert(update.previous.percent == previous.percent);
        assert(update.applied.revision == previous.revision + 1);
        previous = update.applied;
    }
    assert(competing.Read().revision == 8000);
    // A held value snapshot is immutable and does not retain the owner's lock.
    auto retained = competing.Read();
    auto next = competing.SetPercent(0);
    assert(retained.percent == next.previous.percent && retained.revision == next.previous.revision);
    assert(next.applied.percent == 0);
    EmucapPacingOwner lifecycle;
    lifecycle.InitializeFastForward(true);
    assert(lifecycle.Read().fastForward);
    lifecycle.SetPercent(250);
    auto change = lifecycle.SetFastForward(false);
    assert(change.previous.fastForward && !change.applied.fastForward);
    assert(change.applied.percent == 250 && change.previous.percent == 250);
    assert(change.applied.revision == change.previous.revision + 1);
    lifecycle.InitializeFastForward(true); // Machine reset must not restore a launch default.
    assert(!lifecycle.Read().fastForward && lifecycle.Read().percent == 250);
    std::thread overrideWriter([&] {
        for (int i = 0; i < 4000; ++i) lifecycle.SetFastForward(i % 2);
    });
    for (int i = 0; i < 4000; ++i) {
        auto updated = lifecycle.SetPercent(400);
        assert(updated.previous.fastForward == updated.applied.fastForward);
        assert(updated.previous.revision + 1 == updated.applied.revision);
    }
    overrideWriter.join();
    assert(lifecycle.Read().revision == 8003);
    EmucapPacingOwner limits;
    limits.ConfigureLimits(-1, 120, 30, false);
    auto configured = limits.Read().revision;
    limits.CycleLimit(); // A disabled first custom target is not skipped.
    assert(limits.Read().fpsLimit == FPSLimit::NORMAL && limits.Read().revision == configured);
    limits.ConfigureLimits(120, 240, 30, false);
    limits.CycleLimit();
    assert(limits.Read().fpsLimit == FPSLimit::CUSTOM1);
    auto custom = limits.Read();
    assert(!limits.SetAnalogInput(1.0f));
    assert(limits.Read().revision == custom.revision && limits.Read().analogFpsLimit == 0);
    limits.CycleLimit();
    assert(limits.Read().fpsLimit == FPSLimit::CUSTOM2);
    assert(!limits.ChangeLimit(FPSLimit::CUSTOM1, FPSLimit::NORMAL));
    assert(limits.ChangeLimit(FPSLimit::CUSTOM2, FPSLimit::NORMAL));
    assert(limits.SetAnalogInput(1.0f));
    assert(limits.Read().fpsLimit == FPSLimit::ANALOG && limits.Read().analogFpsLimit == 30);
    limits.CycleLimit(); // A held analog mode keeps its precedence.
    assert(limits.Read().fpsLimit == FPSLimit::ANALOG);
    assert(limits.SetAnalogInput(0.0f));
    assert(limits.Read().fpsLimit == FPSLimit::NORMAL);
    limits.ConfigureLimits(120, 240, 120, false);
    done = false;
    std::thread analog([&] {
        for (int i = 0; i < 20000; ++i) limits.SetAnalogInput(i % 2 ? 0.0f : 1.0f);
        done = true;
    });
    do {
        auto value = limits.Read();
        assert((value.fpsLimit == FPSLimit::NORMAL && value.analogFpsLimit == 60) ||
               (value.fpsLimit == FPSLimit::ANALOG && value.analogFpsLimit == 120));
    } while (!done.load());
    analog.join();
    EmucapPacingOwner configuration;
    configuration.ConfigureLimits(120, 240, 360, false);
    auto unchanged = configuration.Read();
    configuration.ConfigureLimits(120, 240, 360, false);
    assert(configuration.Read().revision == unchanged.revision);
    configuration.ConfigureLimits(0, -1, 0, false);
    auto disabled = configuration.Read();
    assert(!configuration.SetAnalogInput(1.0f));
    assert(configuration.Read().revision == disabled.revision);
    configuration.ConfigureLimits(120, 240, 360, false);
    done = false;
    std::thread settings([&] {
        for (int i = 0; i < 20000; ++i) {
            if (i % 2) configuration.ConfigureLimits(120, 240, 360, false);
            else configuration.ConfigureLimits(30, 60, 90, false);
        }
        done = true;
    });
    do {
        auto snapshot = configuration.Read();
        assert(snapshot.customFps2 == snapshot.customFps1 * 2);
        assert(snapshot.analogTargetFps == snapshot.customFps1 * 3);
    } while (!done.load());
    settings.join();
    auto permissionBefore = configuration.Read();
    configuration.SetNetworkSpeedPermission(true);
    auto permissionAfter = configuration.Read();
    assert(permissionAfter.allowNetworkSpeed && permissionAfter.revision == permissionBefore.revision + 1);
    assert(permissionAfter.customFps1 == permissionBefore.customFps1);
    configuration.SetNetworkSpeedPermission(true);
    assert(configuration.Read().revision == permissionAfter.revision);
    configuration.ConfigureLimits(120, 240, 360, false);
    assert(!configuration.Read().allowNetworkSpeed);
    done = false;
    std::thread permissionWriter([&] {
        for (int i = 0; i < 10000; ++i) configuration.SetNetworkSpeedPermission(i % 2);
        done = true;
    });
    do {
        auto applied = configuration.SetPercent(200);
        assert(applied.previous.allowNetworkSpeed == applied.applied.allowNetworkSpeed);
    } while (!done.load());
    permissionWriter.join();
    EmucapPacingOwner apctl;
    uint32_t nativeState = 0;
    done = false;
    std::thread connect([&] {
        for (int i = 0; i < 10000; ++i) apctl.PublishApctlState(nativeState, 4, true);
    });
    std::thread disconnect([&] {
        for (int i = 0; i < 10000; ++i) apctl.PublishApctlState(nativeState, 0, false);
    });
    for (int i = 0; i < 20000; ++i) {
        auto observed = apctl.Read();
        assert(observed.apctlConnected == (observed.apctlState >= 4));
    }
    connect.join(); disconnect.join();
    assert(apctl.Read().apctlState == nativeState);
    apctl.PublishApctlState(nativeState, 3, false);
    auto beforeDuplicate = apctl.Read();
    apctl.PublishApctlState(nativeState, 3, false);
    assert(apctl.Read().revision == beforeDuplicate.revision);
    apctl.PublishApctlState(nativeState, 4, true); // Same path for restored state.
    assert(nativeState == 4 && apctl.Read().apctlConnected);
    EmucapPacingOwner adhoc;
    int adhocState = 0;
    bool initialized = false, controlInitialized = false;
    for (bool init : {false, true}) for (bool control : {false, true})
    for (int state : {0, 1, 3}) for (bool permission : {false, true}) {
        bool stateConnected = state == 1 || state == 3;
        adhoc.RestoreAdhoc(adhocState, initialized, controlInitialized,
            state, stateConnected, init, control);
        adhoc.SetNetworkSpeedPermission(permission);
        auto value = adhoc.Read();
        assert(value.NetworkConnected() == (init && control && stateConnected));
        assert(value.NetworkForced() == (init && control && stateConnected && !permission));
    }
    adhoc.RestoreAdhoc(adhocState, initialized, controlInitialized, 0, false, true, false);
    auto live = adhoc.Read();
    adhoc.RestoreAdhoc(adhocState, initialized, controlInitialized, 1, true,
        live.adhocInited, live.adhocctlInited);
    assert(initialized && !controlInitialized && !adhoc.Read().NetworkConnected());
    std::thread worker([&] {
        for (int i = 0; i < 10000; ++i) adhoc.PublishAdhocState(adhocState, i % 2, i % 2);
    });
    for (int i = 0; i < 10000; ++i) {
        adhoc.PublishAdhocInit(controlInitialized, i % 2, true);
        auto value = adhoc.Read();
        assert(value.adhocStateConnected == (value.adhocState == 1 || value.adhocState == 3));
        assert(value.NetworkConnected() == (value.adhocInited && value.adhocctlInited && value.adhocStateConnected));
    }
    worker.join();
    assert(adhocState == adhoc.Read().adhocState);
    assert(controlInitialized == adhoc.Read().adhocctlInited);
    EmucapPacingOwner transaction;
    uint32_t connectionState = 0;
    std::thread restrictions([&] {
        for (int i = 0; i < 10000; ++i) {
            transaction.PublishApctlState(connectionState, i % 2 ? 4 : 0, i % 2);
            transaction.SetFastForward(true);
        }
    });
    for (int i = 0; i < 10000; ++i) {
        auto result = transaction.ApplySpeed(200);
        assert(result.accepted == !result.previous.NetworkForced());
        if (result.accepted) {
            assert(result.applied.percent == 200 && !result.applied.fastForward);
            assert(result.applied.fpsLimit == FPSLimit::NORMAL);
            assert(result.applied.revision == result.previous.revision + 1);
        } else {
            assert(result.applied.revision == result.previous.revision);
            assert(result.applied.percent == result.previous.percent);
            assert(result.applied.fastForward == result.previous.fastForward);
            assert(result.applied.fpsLimit == result.previous.fpsLimit);
        }
    }
    restrictions.join();
    transaction.PublishApctlState(connectionState, 0, false);
    auto committed = transaction.ApplySpeed(400);
    transaction.SetFastForward(true);
    assert(committed.accepted && !committed.applied.fastForward);
    assert(transaction.Read().fastForward);
    for (int invalid : {-1, 10001}) {
        auto refused = transaction.ApplySpeed(invalid);
        assert(!refused.accepted && refused.previous.revision == refused.applied.revision);
    }
    puts("PSP pacing owner: coherent snapshots and serialized previous/applied transitions passed");
}
'''
with tempfile.TemporaryDirectory(prefix='emucap-psp-owner-') as temporary:
    path = Path(temporary)
    (path / 'EmucapPacing.h').write_text(args.source.read_text())
    (path / 'probe.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++17', '-pthread', '-fsanitize=' + args.sanitizer,
                    str(path / 'probe.cpp'), '-o', str(path / 'probe')], check=True)
    subprocess.run([str(path / 'probe')], check=True)
