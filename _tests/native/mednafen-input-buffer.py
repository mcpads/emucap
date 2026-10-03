#!/usr/bin/env python3
"""Compile the actual patched native input accessors against replaceable device buffers."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True)
args = parser.parse_args()
source = args.source.read_text()

def function(name, source=source, prefix='extern "C" bool '):
    begin = source.index(prefix + name + '(')
    body = source.index('{', begin)
    depth = 0
    for i in range(body, len(source)):
        depth += (source[i] == '{') - (source[i] == '}')
        if depth == 0:
            return source[begin:i+1]
    raise AssertionError('unterminated native function')

code = r'''
#include <cassert>
#include <cstdint>
using uint8 = std::uint8_t;
using uint32 = std::uint32_t;
static uint8* PortData[16] = {};
static uint32 PortDataLen[16] = {};
static void* MDFNGameInfo = nullptr;
'''
code += function('emucap_native_input_read') + '\n' + function('emucap_native_input_write')
adapter = Path(__file__).resolve().parents[2] / 'adapters/mednafen'
code += '''
#include "emucap_input.h"
static EmucapInputOverride g_input_override;
static bool g_input_control_unverified = false;
static bool g_frozen = false;
static uint16_t supported_button_bits() { return 0x0FFF; }
'''
code += function('apply_native_input', (adapter / 'emucap.cpp').read_text(), 'bool ')

code += r'''
int main() {
 unsigned short observed = 0x7777;
 unsigned char first[] = {0xFF, 0xFF, 0xA5, 0x5A};
 unsigned char second[] = {0x11, 0x22, 0x33};
 PortData[0] = first; PortDataLen[0] = sizeof(first);
 assert(!emucap_native_input_write(0, 0, 0xFFFF, &observed) && first[0] == 0xFF);
 MDFNGameInfo = first;
 assert(emucap_native_input_write(0, 0x1234, 0xFFFF, &observed) && observed == 0x1234);
 assert(first[0] == 0x34 && first[1] == 0x12 && first[2] == 0xA5 && first[3] == 0x5A);
 assert(emucap_native_input_write(0, 0, 0xFFFF, &observed) && observed == 0);
 assert(first[0] == 0 && first[1] == 0 && first[2] == 0xA5);
 // Replacing a device must immediately redirect reads/writes, not reuse its old buffer.
 PortData[0] = second; PortDataLen[0] = sizeof(second);
 assert(emucap_native_input_read(0, &observed) && observed == 0x2211);
 assert(emucap_native_input_write(0, 0xABCD, 0xFFFF, &observed));
 assert(second[0] == 0xCD && second[1] == 0xAB && second[2] == 0x33 && first[0] == 0);
 // PCE's mode-select bit is outside the injectable button set.
 second[0] = 0; second[1] = 0x10;
 assert(emucap_native_input_write(0, 1, 0x0FFF, &observed) && observed == 0x1001);
 assert(emucap_native_input_write(0, 0, 0x0FFF, &observed) && observed == 0x1000);
 assert(!emucap_native_input_write(0, 0x1000, 0x0FFF, &observed) && second[1] == 0x10);
 second[0] = 0xCD; second[1] = 0xAB;
 PortDataLen[0] = 1;
 assert(!emucap_native_input_write(0, 0x100, 0xFFFF, &observed) && second[0] == 0xCD);
 assert(emucap_native_input_write(0, 0x12, 0xFFFF, &observed) && observed == 0x12 && second[1] == 0xAB);
 PortData[1] = first; PortDataLen[1] = sizeof(first);
 assert(!emucap_native_input_write(1, 0xFFFF, 0xFFFF, &observed) && first[0] == 0);
 assert(!emucap_native_input_write(16, 0, 0xFFFF, &observed));
 assert(!emucap_native_input_read(0, nullptr));
 assert(!emucap_native_input_write(0, 0, 0xFFFF, nullptr) && second[0] == 0x12);
 PortDataLen[0] = 2; second[0] = 0; second[1] = 0x10;
 assert(apply_native_input(true, 1));
 assert(g_input_override.engaged() && g_input_override.mask() == 1 && second[1] == 0x10);
 assert(apply_native_input(false, 0));
 assert(!g_input_override.engaged() && second[0] == 0 && second[1] == 0x10);
 assert(apply_native_input(true, 1));
 PortDataLen[0] = 0;
 assert(!apply_native_input(false, 0));
 assert(g_input_control_unverified && g_frozen && g_input_override.engaged());
 second[0] = 0x12;
 assert(!emucap_native_input_write(0, 0, 0xFFFF, &observed) && second[0] == 0x12);
 PortData[0] = nullptr;
 assert(!emucap_native_input_read(0, &observed));
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-native-input-') as tmp:
    path = Path(tmp) / 'test.cpp'; path.write_text(code)
    binary = Path(tmp) / 'test'
    subprocess.run(['c++', '-std=c++11', '-Wall', '-Wextra', '-Werror',
                    '-fsanitize=address,undefined', '-I', str(adapter), str(path), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('native input read/write, replacement, rejection and protected bytes passed')
