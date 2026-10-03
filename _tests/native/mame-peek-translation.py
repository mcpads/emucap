#!/usr/bin/env python3
"""Exercise the actual PC-98 i386 debug translator without accessed/dirty writes."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
s=(root/'adapters/mame-pc98/work/mame-src/src/devices/cpu/i386/i386.cpp').read_text()
fn=s[s.index('bool i386_device::i386_translate_address('):s.index('//#define TEST_TLB')]
code=r'''
#include <cstdint>
#include <map>
#include <cassert>
#include <cstdio>
using offs_t=uint32_t;using vtlb_entry=uint32_t;
constexpr uint32_t TR_READ=1, TR_WRITE=2,TR_USER=4, CR0_PG=0x80000000,CR4_PSE=16,FLAG_DIRTY=64;
constexpr uint32_t WRITE_ALLOWED=2,USER_WRITE_ALLOWED=4,USER_READ_ALLOWED=8,WP=0;
struct Memory {std::map<uint32_t,uint32_t> cells;int reads=0,writes=0;
uint32_t read_dword(uint32_t a){++reads;return cells[a];}
void write_dword(uint32_t a,uint32_t v){++writes;cells[a]=v;}};
struct i386_device {uint32_t m_cr[5]={};Memory *m_program;
uint32_t get_permissions(uint32_t,uint32_t){return 0xff;}
bool i386_translate_address(int,bool,offs_t *,vtlb_entry *);};
'''+fn+r'''
int main(){Memory mem; i386_device cpu;cpu.m_program=&mem;cpu.m_cr[0]=CR0_PG;cpu.m_cr[3]=0x1000;
mem.cells[0x1000]=0x2001;mem.cells[0x2000]=0x3001;
auto before=mem.cells;offs_t address=0x123;
assert(cpu.i386_translate_address(TR_READ,true,&address,nullptr));assert(address==0x3123);assert(mem.writes==0 && mem.cells==before);
cpu.m_cr[4]=CR4_PSE;mem.cells[0x1000]=0x400081;before=mem.cells;address=0x123;
assert(cpu.i386_translate_address(TR_READ,true,&address,nullptr));assert(address==0x400123);assert(mem.writes==0 && mem.cells==before);
mem.cells[0x1000]=0;before=mem.cells;address=0x123;
assert(!cpu.i386_translate_address(TR_READ,true,&address,nullptr));assert(mem.writes==0 && mem.cells==before);
puts("MAME actual i386 debug translation: 4KiB/4MiB/unmapped paths preserve page tables; zero accessed/dirty writes");}
'''
with tempfile.TemporaryDirectory() as d:
 p=Path(d)/'test.cpp';p.write_text(code);exe=Path(d)/'test'
 subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(p),'-o',str(exe)],check=True)
 subprocess.run([str(exe)],check=True)
