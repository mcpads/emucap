#!/usr/bin/env python3
"""Check actual PSX CPU wait paths and the scoped debugger clock."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
source = (args.source / 'src/psx/cpu.cpp').read_text()

def block_after(marker, condition):
    start = source.index(condition, source.index(marker))
    opening = source.index('{', start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

gte = block_after('case 0x00:\t\t// MFC2', 'if(timestamp < gte_ts_done)')
hi = block_after('BEGIN_OPF(MFHI)', 'if(timestamp < muldiv_ts_done)')
lo = block_after('BEGIN_OPF(MFLO)', 'if(timestamp < muldiv_ts_done)')
code = r'''
#include <cassert>
#include <cstdint>
#include <initializer_list>
struct Result { int timestamp, absorb; };
'''
for name, body in [('gte', gte), ('hi', hi), ('lo', lo)]:
    code += 'Result ' + name + r'''(int deadline) {
 int timestamp=0, gte_ts_done=deadline, muldiv_ts_done=deadline;
 int LDAbsorb=0, ReadAbsorb[1]={100}, ReadAbsorbWhich=0;
''' + body + (r'''
 return {timestamp,LDAbsorb}; }
''' if name == 'gte' else r'''
 return {timestamp,100-ReadAbsorb[0]}; }
''')
debug = (args.source / 'src/psx/debug.cpp').read_text()
scope = debug[debug.index('static const unsigned REG_CPU_TIMESTAMP'):debug.index('static void (*LogFunc)')]
registers = debug[debug.index('static uint32 GetRegister_CPU'):debug.index('static const RegGroupType CPURegsGroup')]
code += r'''
using uint32 = uint32_t;
using pscpu_timestamp_t = int32_t;
struct CPUStub {
 unsigned writes=0;
 uint32 GetRegister(unsigned, char*, uint32) { return 77; }
 void SetRegister(unsigned, uint32) { ++writes; }
} cpu;
CPUStub* CPU=&cpu;
''' + scope + registers
code += r'''
int main() {
 assert(GetRegister_CPU(REG_CPU_TIMESTAMP,nullptr,0)==0);
 {
  CallbackTimestampScope scope(123);
  assert(GetRegister_CPU(REG_CPU_TIMESTAMP,nullptr,0)==123);
  SetRegister_CPU(REG_CPU_TIMESTAMP,456);
  assert(cpu.writes==0 && GetRegister_CPU(REG_CPU_TIMESTAMP,nullptr,0)==123);
  assert(GetRegister_CPU(1,nullptr,0)==77);
  SetRegister_CPU(1,456); assert(cpu.writes==1);
 }
 assert(GetRegister_CPU(REG_CPU_TIMESTAMP,nullptr,0)==0);
 try { CallbackTimestampScope scope(789); throw 1; } catch(int) {}
 assert(GetRegister_CPU(REG_CPU_TIMESTAMP,nullptr,0)==0);
 for(auto wait : {gte,hi,lo}) {
  for(int expired : {-189259,-11914,-1,0}) {
   auto value=wait(expired);assert(value.timestamp==0 && value.absorb==0);
  }
  auto pending=wait(8);assert(pending.timestamp==8 && pending.absorb==8);
 }
 assert(gte(1).timestamp==1);
 // The native mul/div path grants its one-cycle optimization.
 assert(hi(1).timestamp==0 && lo(1).timestamp==0);
 assert(hi(2).timestamp==2 && lo(2).timestamp==2);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-psx-deadlines-') as temp:
    cpp = Path(temp) / 'test.cpp'
    binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Actual PSX wait paths and read-only clock scope/exception cleanup pass ASan/UBSan')
