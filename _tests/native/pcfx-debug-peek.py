#!/usr/bin/env python3
"""Check PC-FX debugger CPU reads against native RAM history and I/O dispatch."""
import argparse
from pathlib import Path
import subprocess
import tempfile

root=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,default=root/'adapters/mednafen/work/mednafen/src/pcfx')
p.add_argument('--baseline',action='store_true',help='Require the original RAM-history and I/O side effects')
a=p.parse_args()
source=(a.source/'pcfx.cpp').read_text()
mem=(a.source/'mem-handler.inc').read_text()
def function(text,signature):
 start=text.index(signature);end=text.index('\n}',start)+2
 return text[start:end]
macro_start=source.index('#define RAMLPCHECK')
macro_end=source.index('\n}',macro_start)+2
code=r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>
#include <stdexcept>
struct MDFN_Error:std::runtime_error{MDFN_Error(int,const char* s):std::runtime_error(s){}};
using uint8=uint8_t;using uint16=uint16_t;using uint32=uint32_t;using int32=int32_t;
using v810_timestamp_t=int32_t;
#define MDFN_FASTCALL
#define FXDBG(...) ((void)0)
#define trio_sscanf std::sscanf
uint8 RAM[0x200000],BIOSROM[0x100000],BackupRAM[0x8000],ExBackupRAM[0x20000];
uint8* FXSCSIROM=nullptr;
uint32 RAM_LPA=0x5000,RAM_PageNOTMask=0xfffff000;
bool BRAMDisabled=false;uint32 BackupControl=2;
int device_reads=0;
uint8 port_rbyte(v810_timestamp_t&,uint32) {++device_reads;return 0x5a;}
struct Disc {bool ReadSectors(uint8*,int32,int) {assert(false);return false;}};
std::vector<Disc*>* cdifs=nullptr;
uint8 GAS_SectorCache[2048];int GAS_SectorCacheWhich=-1;
'''
code+=source[macro_start:macro_end]+'\n'
code+=function(mem,'uint8 MDFN_FASTCALL mem_peekbyte(')+'\n'
code+=function(mem,'static uint8 MDFN_FASTCALL mem_rbyte(')+'\n'
code+=function(source,'static void PCFXDBG_GetAddressSpaceBytes(')+'\n'
code+=r'''
int main() {
 RAM[0x12000]=0x39;uint8 value=0;
 PCFXDBG_GetAddressSpaceBytes("cpu",0x12000,1,&value);assert(value==0x39);
 assert(RAM_LPA==EXPECTED_PAGE);
 PCFXDBG_GetAddressSpaceBytes("cpu",0x80000600,1,&value);
 assert(device_reads==EXPECTED_READS);
 BackupRAM[3]=0x37;ExBackupRAM[4]=0x48;
 PCFXDBG_GetAddressSpaceBytes("cpu",0xE0000006,1,&value);assert(value==0x37);
 PCFXDBG_GetAddressSpaceBytes("cpu",0xE8000008,1,&value);assert(value==0x48);
 BRAMDisabled=true;
 PCFXDBG_GetAddressSpaceBytes("cpu",0xE0000006,1,&value);assert(value==0xff);
 PCFXDBG_GetAddressSpaceBytes("cpu",0xE8000008,1,&value);assert(value==0xff);
}
'''.replace('EXPECTED_PAGE','0x12000' if a.baseline else '0x5000').replace('EXPECTED_READS','1' if a.baseline else '0')
with tempfile.TemporaryDirectory(prefix='pcfx-debug-peek-') as directory:
 cpp=Path(directory)/'check.cpp';binary=Path(directory)/'check';cpp.write_text(code)
 subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined',str(cpp),'-o',str(binary)],check=True)
 subprocess.run([str(binary)],check=True)
print('REPRODUCED native debugger RAM_LPA mutation and I/O dispatch' if a.baseline else 'PASS debugger reads preserve RAM history and avoid I/O dispatch')
