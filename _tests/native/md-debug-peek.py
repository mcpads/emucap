#!/usr/bin/env python3
"""Exercise actual MD peek guards with controlled CPU, VDP and FM consumers."""
from pathlib import Path
import subprocess
import tempfile
root=Path(__file__).resolve().parents[2]
base=root/'adapters/mednafen/work/mednafen/src/md'
def function(file,signature):
 text=(base/file).read_text();start=text.index(signature)
 return text[start:text.index('\n}',start)+2]+'\n'
code=r'''
#include <cassert>
#include <cstdint>
using uint32=uint32_t;using uint8=uint8_t;using uint16=uint16_t;using int32=int32_t;
#define MDFN_FASTCALL
int MD_HackyHackyMode=0,zreset=1,zbusreq=0,z80_cycle_counter=0,z80_last_ts=0,md_timestamp=0;
int z80_calls=0,vdp_calls=0,fm_calls=0;
struct {int timestamp=10;} Main68K;
struct {void Run(){++vdp_calls;}} MainVDP;
int z80_do_opcode(){++z80_calls;return 1;}
static const unsigned obsim_values[3]={0,0x7fff,0xffff};
unsigned obsim=0;bool FMReset=false;
void UpdateFM(){++fm_calls;}
struct {int read(){return 0x5a;}} FMUnit;
'''
code+=function('system.cpp','void MD_UpdateSubStuff(void)')
code+=function('mem68k.cpp','unsigned int m68k_read_bus_16(')
code+=function('sound.cpp','int MDSound_ReadFM(')
code+=r'''
uint8 MD_ReadMemory8(uint32_t address) {
 MD_UpdateSubStuff();m68k_read_bus_16(address);return MDSound_ReadFM(address);
}
'''
code+=function('mem68k.cpp','MDFN_FASTCALL uint8 Main68K_BusPeek8(')
code+=r'''
int main(){
 assert(Main68K_BusPeek8(0xc00004)==0x5a);
 assert(MD_HackyHackyMode==0 && z80_calls==0 && vdp_calls==0 && fm_calls==0);
 assert(obsim==0 && z80_last_ts==0 && md_timestamp==0);
 assert(MD_ReadMemory8(0xc00004)==0x5a);
 assert(z80_calls>0 && vdp_calls>0 && fm_calls==1 && obsim==1);
 assert(z80_last_ts==70 && md_timestamp==70);
}
'''
with tempfile.TemporaryDirectory(prefix='md-debug-peek-') as d:
 p=Path(d);(p/'check.cpp').write_text(code)
 subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined',str(p/'check.cpp'),'-o',str(p/'check')],check=True)
 subprocess.run([str(p/'check')],check=True)
print('PASS native debug peeks preserve clocks and bus state; ordinary reads still advance devices')
