#!/usr/bin/env python3
"""Check native frame entry against six-button handshake timeout continuity."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
system = (args.source / 'src/md/system.cpp').read_text()
pad = (args.source / 'src/md/input/gamepad.cpp').read_text()
entry = system[system.index('static void Emulate('):system.index(' MDINPUT_Frame();')]
methods = pad[pad.index('void Gamepad6::Run('):pad.index('/*\n How it\'s implemented')]
code = r'''
#include <cassert>
#include <cstdint>
using int32 = int32_t;
struct EmulateSpecStruct {};
struct Gamepad6 {
 int32 prev_timestamp=0, timeout=0, count=0;
 void Run(int32);
 void BeginTimePeriod(int32);
 void EndTimePeriod(int32);
};
''' + methods + r'''
static Gamepad6 device;
static int32 md_timestamp;
void MDFNMP_ApplyPeriodicCheats() {}
void MDIO_BeginTimePeriod(int32 timestamp) { device.BeginTimePeriod(timestamp); }
''' + entry + r'''
}
int main() {
 // Ordinary fresh frames retain their zero origin and continue timeout state.
 device.prev_timestamp=0; device.timeout=30; device.count=2; md_timestamp=0;
 Emulate(nullptr); device.Run(100);
 assert(device.timeout==130 && device.count==2 && device.prev_timestamp==100);
 // This partial-frame origin must retain time since the last actual pad access.
 device.prev_timestamp=12; device.timeout=57200; device.count=5; md_timestamp=100;
 Emulate(nullptr);
 assert(device.prev_timestamp==12);
 device.Run(180);
 assert(device.timeout==0 && device.count==0 && device.prev_timestamp==180);
 // Without a frame-entry transition the same restored origin has the same result.
 device.prev_timestamp=12; device.timeout=57200; device.count=5;
 device.Run(180);
 assert(device.timeout==0 && device.count==0);
 // Normal end/rebase/start must neither drop nor charge elapsed clocks twice.
 device.prev_timestamp=10; device.timeout=20; device.count=2;
 device.EndTimePeriod(100); device.BeginTimePeriod(0); md_timestamp=0;
 Emulate(nullptr); device.Run(40);
 assert(device.timeout==150 && device.count==2 && device.prev_timestamp==40);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-md-input-origin-') as temp:
    cpp = Path(temp) / 'test.cpp'; binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Native MD frame entry and six-button timeout preserve partial origins and '
      'ordinary frame rebasing under ASan/UBSan')
