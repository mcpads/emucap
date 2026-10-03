#!/usr/bin/env python3
"""Run the maintained NDS RSP policy handlers and parser with the actual native owner."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True, help='patched gdbstub.cpp')
args = parser.parse_args()
source = args.source.read_text()
def extract(marker):
    start = source.index(marker)
    brace = source.index('{', start)
    depth, end = 1, brace + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]
helpers = extract('static int\nhex (') + '\n' + extract('static int\nhexToInt(')
handlers = extract('else if (strcmp((const char *)packet, "qEmucap,rateInfo")')
handlers += '\n' + extract('else if (strncmp((const char *)packet, "QEmucap,rate:",')
epoch = next(line for line in source.splitlines() if '++emucap_memory_epoch;' in line and 'strchr' in line)
code = r'''
#include "EmucapPacing.h"
#include <atomic>
#include <cassert>
#include <cstdio>
#include <cstring>
#include <string>
static EmucapPacingOwner emucap_pacing;
static std::atomic<uint64_t> emucap_vblank_clock{7}, emucap_memory_epoch{3};
static std::atomic<bool> emucap_pace_wake{false};
constexpr size_t BUFMAX=0x8000;
''' + helpers + '\n' + extract('static bool emucap_running_safe_packet(') + r'''
std::string request(const char *text) {
    const uint8_t *packet=(const uint8_t*)text;
    char buffer[BUFMAX]; uint8_t *out_ptr=(uint8_t*)buffer; int send_size=0;
''' + epoch + '\n if(false) {}\n' + handlers + r'''
    return std::string(buffer,send_size);
}
int main() {
    emucap_pacing.ObserveNative(false);
    assert(request("qEmucap,rateInfo")=="1;64,0,1,7");
    assert(request("QEmucap,rate:c8")=="1;64,0,1,7;c8,0,2,7");
    assert(emucap_memory_epoch==3 && emucap_pace_wake);
    emucap_pace_wake=false;
    for(const char *bad:{"", "-1", "2711", "100000000", "c8junk", "0;extra"}) {
        auto before=emucap_pacing.Read();
        assert(request((std::string("QEmucap,rate:")+bad).c_str())=="E01");
        assert(emucap_pacing.Read().revision==before.revision);
        assert(!emucap_pace_wake && emucap_memory_epoch==3);
    }
    assert(request("QEmucap,rate:c8")=="1;c8,0,2,7;c8,0,2,7");
    emucap_pacing.ObserveNative(true);
    assert(request("QEmucap,rate:0")=="1;c8,1,3,7;0,0,4,7");
    assert(!emucap_pacing.ObserveNative(true).nativeUnlimited);
    assert(request("QEmucap,rate:1")=="1;0,0,4,7;1,0,5,7");
    assert(request("QEmucap,rate:2710")=="1;1,0,5,7;2710,0,6,7");
    for(const char *wire:{"qEmucap,rateInfo", "QEmucap,rate:c8"})
        assert(emucap_running_safe_packet((const uint8_t*)wire));
    puts("NDS transaction: version, exact previous/final, override clearing, domain and no-effect rejection pass");
}
'''
with tempfile.TemporaryDirectory(prefix='emucap-nds-transaction-') as tmp:
    root=Path(tmp)
    (root/'EmucapPacing.h').write_text((args.source.parent/'EmucapPacing.h').read_text())
    (root/'probe.cpp').write_text(code)
    subprocess.run(['c++','-std=c++17','-pthread','-fsanitize=address,undefined',str(root/'probe.cpp'),'-o',str(root/'probe')],check=True)
    subprocess.run([str(root/'probe')],check=True)
