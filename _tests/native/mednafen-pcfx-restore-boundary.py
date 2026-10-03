#!/usr/bin/env python3
"""Exercise V810 replacement dispatch with controlled CPU/event seams."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
source = (args.source / 'src/hw_cpu/v810/v810_oploop.inc').read_text()
start = source.index('    while(Running)')
end = source.index('\n\t{\n\t //printf', start)
prefix = source[start:end]
macro = next(line for line in (args.source / 'src/hw_cpu/v810/v810_cpu.cpp').read_text().splitlines()
             if line.startswith('#define RB_CPUHOOK_DBG'))
code = r'''
#include <cassert>
#include <cstdint>
using uint32=uint32_t;
static int v810_timestamp, next_event_ts, PC, IPendingCache, Halted, in_bstr, in_bstr_to;
static bool Running, pending, repeat;
static int mode, hooks, instructions, events, transfers, acknowledgements;
static int P_REG[32];
static void replace() {
 v810_timestamp=20; PC=200; next_event_ts=30;
 Halted=mode==1; in_bstr=mode==2; IPendingCache=mode==3;
 if(mode==4) next_event_ts=20;
 if(mode==5) Running=false;
 pending=true;
}
static void hook(int,int) { ++hooks; replace(); }
static void (*CPUHook)(int,int)=hook;
static bool emucap_native_restore_pending() { return pending; }
static bool service(bool ready,int ts,uint32 oldpc) {
 assert(ts==v810_timestamp && oldpc==(uint32)PC);
 assert(ready==(Running && ts<next_event_ts && !Halted && !in_bstr && !IPendingCache));
 ++acknowledgements;
 if(repeat) { repeat=false; mode=0; replace(); return true; }
 pending=false; return false;
}
#define emucap_native_restore_service(ready) service(ready,timestamp_rl,old_PC)
#define RB_GETPC() PC
#define RB_DEBUGMODE
''' + macro + r'''
#define RB_CPUHOOK(n) RB_CPUHOOK_DBG(n)
static void enter() {
 int timestamp_rl=v810_timestamp;
 uint32 opcode=0;
''' + prefix + r'''
  if(IPendingCache) { ++events; return; }
  assert(timestamp_rl==v810_timestamp);
  ++instructions; return;
 }
 ++events; return;
 }
 return;
 op_BSTR: ++transfers; return;
}
static void clear(int m) {
 v810_timestamp=1; next_event_ts=10; PC=100; Running=true;
 mode=m; pending=repeat=false; IPendingCache=Halted=in_bstr=0; in_bstr_to=0;
 hooks=instructions=events=transfers=acknowledgements=0;
}
int main() {
 clear(0); enter(); assert(hooks==1 && acknowledgements==1 && instructions==1);
 clear(1); enter(); assert(hooks==1 && acknowledgements==1 && instructions==0 && events==1);
 clear(2); enter(); assert(hooks==1 && acknowledgements==1 && instructions==0 && transfers==1);
 clear(3); enter(); assert(hooks==1 && acknowledgements==1 && instructions==0 && events==1);
 clear(4); enter(); assert(hooks==1 && acknowledgements==1 && instructions==0 && events==1);
 clear(5); enter(); assert(hooks==1 && acknowledgements==1 && instructions==0 && events==0);
 clear(2); repeat=true; enter(); assert(hooks==1 && acknowledgements==2 && instructions==1);
 clear(0); IPendingCache=1; enter(); assert(hooks==0 && acknowledgements==0 && instructions==0 && events==1);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-pcfx-restore-') as temp:
    cpp=Path(temp)/'test.cpp'; binary=Path(temp)/'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Native V810 restore: instruction, halt, bitstring, interrupt, due event, '
      'exit and repeated replacement pass ASan/UBSan')
