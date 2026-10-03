#!/usr/bin/env python3
"""Exercise the maintained NDS policy owner with competing debugger/frontend writers."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True)
parser.add_argument('--sanitizer', choices=['address,undefined', 'thread'], default='address,undefined')
args = parser.parse_args()
code = r'''
#include "EmucapPacing.h"
#include <cassert>
#include <algorithm>
#include <thread>
#include <vector>
#include <cstdio>
int main() {
    EmucapPacingOwner owner;
    owner.ObserveNative(true);
    auto cleared = owner.Apply(200);
    assert(cleared.previous.nativeUnlimited && !cleared.applied.nativeUnlimited);
    assert(!owner.ObserveNative(true).nativeUnlimited); // unchanged source cannot reclaim override
    owner.ObserveNative(false);
    assert(owner.ObserveNative(true).nativeUnlimited); // a new native source edge can
    auto invalid = owner.Apply(10001);
    assert(!invalid.accepted && invalid.previous.revision == invalid.applied.revision);
    assert(owner.Read().nativeUnlimited);
    auto unlimited = owner.Apply(0);
    assert(unlimited.accepted && unlimited.applied.percent == 0 && !unlimited.applied.nativeUnlimited);
    owner.ObserveNative(false);
    auto retained = owner.Read();
    owner.Apply(400);
    assert(retained.percent == 0);

    EmucapPacingOwner competing;
    competing.ObserveNative(false);
    std::vector<EmucapPacingChange> arm9, arm7;
    auto write = [&](auto &results, unsigned offset) {
        for (unsigned i=0; i<4000; ++i) results.push_back(competing.Apply(200 + i*2 + offset));
    };
    std::thread first([&]{write(arm9,0);});
    std::thread second([&]{write(arm7,1);});
    first.join(); second.join();
    arm9.insert(arm9.end(), arm7.begin(), arm7.end());
    std::sort(arm9.begin(),arm9.end(),[](auto a,auto b){return a.applied.revision < b.applied.revision;});
    EmucapPacingSnapshot previous;
    for(auto result:arm9) {
        assert(result.accepted && result.previous.percent == previous.percent);
        assert(result.previous.revision == previous.revision);
        assert(result.applied.revision == previous.revision + 1);
        previous = result.applied;
    }
    std::thread frontend([&]{for(int i=0;i<20000;++i) competing.ObserveNative(i%2);});
    for(int i=0;i<20000;++i) {
        auto result=competing.Apply(50);
        assert(result.accepted && !result.applied.nativeUnlimited && result.applied.percent==50);
        assert(result.applied.revision >= result.previous.revision);
        auto value=competing.Read();
        assert(value.percent==50); // frontend changes only its own override
    }
    frontend.join();
    puts("NDS owner: serialized two-port results, override edges, immutable snapshots and admission pass");
}
'''
with tempfile.TemporaryDirectory(prefix='emucap-nds-owner-') as tmp:
    root = Path(tmp)
    (root/'EmucapPacing.h').write_text(args.source.read_text())
    (root/'probe.cpp').write_text(code)
    subprocess.run(['c++','-std=c++17','-pthread','-fsanitize='+args.sanitizer,str(root/'probe.cpp'),'-o',str(root/'probe')], check=True)
    subprocess.run([str(root/'probe')], check=True)
