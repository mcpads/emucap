#!/usr/bin/env python3
"""Native track reads reject failed acquisition and preserve the last valid cache."""
from pathlib import Path
import argparse,subprocess,tempfile
root=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--baseline',action='store_true')
p.add_argument('--source',type=Path,default=root/'adapters/mednafen/work/mednafen/src/pcfx/pcfx.cpp');a=p.parse_args()
s=a.source.read_text();start=s.index('static void PCFXDBG_GetAddressSpaceBytes(');body=s[start:s.index('\n}',start)+2]
code=r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <stdexcept>
#include <vector>
using uint8=uint8_t;using uint32=uint32_t;using int32=int32_t;
#define trio_sscanf std::sscanf
struct MDFN_Error:std::runtime_error{MDFN_Error(int,const char* s):std::runtime_error(s){}};
uint8 RAM[0x200000],BackupRAM[0x8000],ExBackupRAM[0x20000],BIOSROM[0x100000];
uint8 mem_peekbyte(int,uint32){return 0;}
int reads=0;bool fail=false;
struct Disc{bool ReadSectors(uint8* p,int32 sector,int){
 ++reads;memset(p,0xee,2048);if(fail)return false;memset(p,0x31+sector,2048);return true;
}} disc;
std::vector<Disc*> discs{&disc};auto* cdifs=&discs;
uint8 GAS_SectorCache[2048];int GAS_SectorCacheWhich=-1;
'''+body+r'''
int main(){
 uint8 value=0;PCFXDBG_GetAddressSpaceBytes("track0-1-0",0,1,&value);
 assert(value==0x31 && reads==1 && GAS_SectorCacheWhich==0);
 fail=true;bool threw=false;value=0x5a;
 try{PCFXDBG_GetAddressSpaceBytes("track0-1-0",2048,1,&value);}catch(const MDFN_Error&){threw=true;}
 assert(threw==EXPECT_THROW);
 if(EXPECT_THROW){
  assert(value==0x5a && GAS_SectorCacheWhich==0 && GAS_SectorCache[0]==0x31);
  PCFXDBG_GetAddressSpaceBytes("track0-1-0",0,1,&value);assert(value==0x31 && reads==2);
 }else{assert(value==0 && GAS_SectorCacheWhich==1);}
 fail=false;GAS_SectorCacheWhich=0;
 PCFXDBG_GetAddressSpaceBytes("track0-1-0",2048,1,&value);assert(value==0x32 && reads==3);
 // A cross-sector failure must throw even after filling an earlier byte.
 GAS_SectorCacheWhich=-1;uint8 two[2]={};
 PCFXDBG_GetAddressSpaceBytes("track0-1-0",2047,1,two);fail=true;
 threw=false;try{PCFXDBG_GetAddressSpaceBytes("track0-1-0",2047,2,two);}catch(const MDFN_Error&){threw=true;}
 assert(threw==EXPECT_THROW);
}
'''
code=code.replace('EXPECT_THROW','false' if a.baseline else 'true')
with tempfile.TemporaryDirectory(prefix='pcfx-track-read-') as d:
 p=Path(d);(p/'check.cpp').write_text(code)
 subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined',str(p/'check.cpp'),'-o',str(p/'check')],check=True)
 subprocess.run([str(p/'check')],check=True)
print('REPRODUCED failed sectors published and cached as zero bytes' if a.baseline else 'PASS failed sectors throw, prior cache survives, retry and cross-sector failure work')
