#!/usr/bin/env python3
"""Native MD dispatch distinguishes opcode entry from pending CPU work."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
source = (args.source / 'src/md/system.cpp').read_text()
body = source[source.index('static int system_frame('):source.index('static void Emulate(')]
code = r'''
#include <cassert>
#include <vector>
#define WANT_DEBUGGER 1
#define MDFN_UNLIKELY(x) (x)
static bool run_cpu, suspend68k, MD_DebugMode=true;
static int gen_running=1, rounds, hooks, idles, opcodes;
static unsigned replacement;
static bool replace, replace_again;
static std::vector<bool> readiness;
struct CPU {
 unsigned XPending=0;
 void Step() {
  if(XPending==0x10) XPending=0; // Native reset/exception work, no opcode.
  else if(!XPending && !suspend68k) ++opcodes;
 }
} Main68K;
void MDDBG_CPUHook() {
 ++hooks;
 if(replace) { replace=false; Main68K.XPending=replacement; }
}
void emucap_native_idle_service() { ++idles; }
bool emucap_native_restore_service(bool ready) {
 readiness.push_back(ready);
 if(replace_again) { replace_again=false; Main68K.XPending=0; return true; }
 return false;
}
void MD_UpdateSubStuff() { if(++rounds==2) run_cpu=false; }
''' + body + r'''
void clear(unsigned pending=0) {
 Main68K.XPending=pending; rounds=hooks=idles=opcodes=0;
 suspend68k=replace=replace_again=false; readiness.clear();
}
int main() {
 clear(); system_frame(0); assert(hooks==2 && opcodes==2 && idles==0);
 clear(0x10); system_frame(0); assert(hooks==1 && opcodes==1 && idles==1);
 clear(0x400); system_frame(0); assert(hooks==0 && opcodes==0 && idles==2);
 clear(); suspend68k=true; system_frame(0); assert(hooks==0 && opcodes==0 && idles==2);
 clear(); replace=true; replacement=0x400; system_frame(0);
 assert(hooks==1 && opcodes==0 && idles==1);
 assert((readiness==std::vector<bool>{false,false}));
 clear(); replace=true; replacement=0x400; replace_again=true; system_frame(0);
 assert((readiness==std::vector<bool>{false,true,true}));
 assert(opcodes==2);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-md-boundary-') as temp:
    cpp = Path(temp) / 'test.cpp'; binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Native MD dispatch: opcode, pending reset, hard halt, bus suspension, '
      'replacement and repeated reclassification pass ASan/UBSan')
