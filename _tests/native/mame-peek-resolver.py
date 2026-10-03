#!/usr/bin/env python3
"""Compile the shipped native range resolver with counted translation/payload owners."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
s=(root/'adapters/mame-pc98/work/mame-src/src/frontend/mame/luaengine_mem.cpp').read_text()
resolver=s[s.index('\tauto resolve_peek_block ='):s.index('\taddr_space_type.set_function("validate_peek_block",')]
code=r'''
#include <cstdint>
#include <vector>
#include <utility>
#include <cassert>
#include <cstdio>
using u32=uint32_t; using u64=uint64_t; using offs_t=uint32_t;
struct device_memory_interface { enum { TR_READ=1 }; };
struct Machine { bool disabled=false; struct Guard { Machine &m; ~Guard(){m.disabled=false;} }; Guard disable_side_effects(){disabled=true;return {*this};} };
struct Device { Machine owner; Machine &machine(){return owner;} };
struct address_space { Device d; int payload=0; Device &device(){return d;} int spacenum(){return 0;} int read_byte(offs_t){++payload;return 0;} };
struct Translator { address_space &s; int calls=0; bool translate(int,int,offs_t &a,address_space *&t){assert(s.d.owner.disabled);++calls;t=&s;return a<256;} };
struct addr_space { address_space &space; Translator &dev; };
int main(){
'''+resolver+r'''
address_space memory; Translator dev{memory}; addr_space sp{memory,dev};
std::vector<std::pair<address_space *,offs_t>> result;
assert(!resolve_peek_block(sp,254,3,result)); assert(memory.payload==0);assert(!memory.d.owner.disabled);
result.clear();assert(resolve_peek_block(sp,254,2,result));assert(result.size()==2 && result[1].second==255);assert(memory.payload==0);
int calls=dev.calls;result.clear();assert(!resolve_peek_block(sp,0xffffffff,2,result));assert(dev.calls==calls);
assert(!resolve_peek_block(sp,0,0,result));assert(!resolve_peek_block(sp,0,65537,result));assert(dev.calls==calls);
puts("MAME native resolver: full translation before payload, bounds and side-effect guard pass");
}
'''
with tempfile.TemporaryDirectory() as d:
 p=Path(d)/'test.cpp';p.write_text(code);exe=Path(d)/'test'
 subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined','-g',str(p),'-o',str(exe)],check=True)
 subprocess.run([str(exe)],check=True)
