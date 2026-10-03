#!/usr/bin/env python3
"""External restoration keeps frontend values without changing device latches."""
from pathlib import Path
import os
import subprocess
import tempfile
root=Path(__file__).resolve().parents[2]
code=r'''
#include "emucap_live_input.h"
#include <cassert>
#include <cstdlib>
#include <new>
static bool fail_alloc=false;
void* operator new(std::size_t size) {
 if(fail_alloc) throw std::bad_alloc();
 if(auto p=std::malloc(size ? size : 1))return p;
 throw std::bad_alloc();
}
void operator delete(void* p) noexcept {std::free(p);}
int main() {
 int game=0,other=0;uint8_t first[]={1,2,3,4},last[]={9,8};
 EmucapLiveInput::Views views{};views[0]={3,first,4};views[15]={5,last,2};
 const auto saved=EmucapLiveInput::capture(&game,views);
 first[0]=6;last[1]=7;
 for(unsigned fault=0;fault<4;++fault) {
  auto changed=views;
  if(fault==0)changed[15].device=8;
  if(fault==1)changed[15].size=1;
  if(fault==2)changed[15].data=nullptr;
  assert(!saved.restore(fault==3 ? &other : &game,changed));
  assert(first[0]==6 && last[1]==7);
 }
 fail_alloc=true;assert(saved.restore(&game,views));fail_alloc=false;
 assert(first[0]==1 && first[1]==2 && first[2]==3 && first[3]==4 && last[1]==8);
 uint8_t relocated[4]={};views[0].data=relocated;
 assert(saved.restore(&game,views) && relocated[0]==1 && relocated[3]==4);
 fail_alloc=true;
 try {EmucapLiveInput::capture(&game,views);assert(false);}catch(const std::bad_alloc&){}
 fail_alloc=false;assert(relocated[0]==1 && last[1]==8);
 try {EmucapLiveInput::capture(nullptr,views);assert(false);}catch(const std::runtime_error&){}
 views[15].data=nullptr;
 try {EmucapLiveInput::capture(&game,views);assert(false);}catch(const std::runtime_error&){}
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-live-input-') as temp:
    cpp=Path(temp)/'test.cpp';exe=Path(temp)/'test';cpp.write_text(code)
    subprocess.run(['clang++','-std=c++11','-O1','-fsanitize=address,undefined',
        '-fno-omit-frame-pointer','-I'+str(root/'adapters/mednafen'),str(cpp),'-o',str(exe)],check=True)
    subprocess.run([str(exe)],check=True,env=dict(os.environ,UBSAN_OPTIONS='halt_on_error=1'))
print('Live input: all-port preflight, owner/config rejection, allocation failure and allocation-free restoration pass ASan/UBSan')
