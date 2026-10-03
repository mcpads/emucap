#!/usr/bin/env python3
"""Exercise Flycast's actual hit capture and drain with aliased breakpoints."""
import json
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/flycast/emucap.cpp').read_text()


def function(signature):
    start = source.index(signature)
    return source[start:source.index('\n}', start) + 2]


def declaration(name):
    return re.search(r'struct ' + name + r' \{[^\n]+\};', source).group()


code = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>
'''+declaration('EmuBp')+'\n'+declaration('FlyHit')+r'''
std::vector<EmuBp> g_bps;
std::vector<FlyHit> g_bp_hits;
uint64_t g_bp_hits_dropped=0, g_frame=19;
long g_step_id=-1,g_boundary_reply_id=-1,g_step_remaining=0;
bool g_frozen=false, writers_verified=true;
std::string g_boundary_reply;
unsigned parks=0;
uint32_t sh4_fold_pc(uint32_t pc){return pc&0x1fffffff;}
std::string emucap_capture_regs(){return "{\"r0\":42}";}
bool exclude_renderer_writes(long){return writers_verified;}
void emucap_park(){++parks;}
void contain_service_exception(const char*,const char*){assert(false);}
void reply_ok(long,const std::string& reply){std::cout<<reply<<'\n';}
'''+function('void record_breakpoint_hit(')+'\n'+function('void emucap_debugger_spin(')+'\n'+function('void emucap_bp_spin(')+'\n'+function('void handle_poll_events(')+r'''
int main(){
 g_bps={{11,0x8c001000},{12,0x0c001000},{13,0x8c002000}};
 g_step_id=7;g_step_remaining=60;
 emucap_bp_spin(0xac001000);
 assert(parks==1 && g_frozen && g_step_id==-1 && g_step_remaining==0);
 assert(g_boundary_reply_id==7);
 g_bps.clear(); // A queued event retains the identity after the point is cleared.
 handle_poll_events(1);
 handle_poll_events(2); // Drain is exactly once.
 g_bps={{21,0x8c001000}};
 emucap_debugger_spin(0xac001000,false); // A paused register watch at the same PC.
 record_breakpoint_hit(0xac001000,false); // A non-pausing register watch.
 handle_poll_events(3);
 writers_verified=false;
 emucap_bp_spin(0xac001000);
 handle_poll_events(4); // An unverified observation cannot mint a hit.
 writers_verified=true;
 for(unsigned i=0;i<4097;++i) record_breakpoint_hit(0xac001000,true);
 assert(g_bp_hits.size()==4096);
 handle_poll_events(5);
 handle_poll_events(6);
}
'''
with tempfile.TemporaryDirectory(prefix='flycast-events-') as directory:
    directory = Path(directory)
    (directory / 'check.cpp').write_text(code)
    subprocess.run(['clang++', '-std=c++17', '-O1', '-g', '-fsanitize=address,undefined',
                    str(directory / 'check.cpp'), '-o', str(directory / 'check')], check=True)
    result = subprocess.run([str(directory / 'check')], check=True, capture_output=True,
                            text=True, timeout=20)
rows = [json.loads(line) for line in result.stdout.splitlines()]
assert [event['breakpoint_id'] for event in rows[0]['events']] == [11, 12]
assert all(event['pc'] == 0xac001000 and event['registers'] == {'r0': 42}
           for event in rows[0]['events'])
assert rows[1] == {'events': [], 'dropped': 0}
assert len(rows[2]['events']) == 2
assert all('breakpoint_id' not in event for event in rows[2]['events'])
assert rows[3] == {'events': [], 'dropped': 0}
assert len(rows[4]['events']) == 4096 and rows[4]['dropped'] == 1
assert rows[5] == {'events': [], 'dropped': 0}
print('PASS Flycast hit identity, aliases, clear-before-drain, watch separation, loss accounting')
