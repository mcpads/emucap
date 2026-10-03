#!/usr/bin/env python3
"""PCSX2 batches require the same completed native park authority as owned frame terminals."""
import argparse
from pathlib import Path
import subprocess
import tempfile
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,required=True)
a=p.parse_args()
pine=(a.source/'PINE.cpp').read_text()
start=pine.index('\t\t\tcase MsgEmuCapMemoryBatch:')
end=pine.index('\t\t\tcase MsgEmuCapReadBytes:',start)
handler=pine[start:end]
vm=(a.source/'VMManager.cpp').read_text()
start=vm.index('bool VMManager::IsEmucapExecutionParkedOnCPUThread()')
predicate=vm[start:vm.index('\n}',start)+2]
code=r'''
#include <cassert>
#include <cstdint>
#include <cstring>
#include <functional>
#include <span>
#include <utility>
#include <vector>
using u8=uint8_t;using u32=uint32_t;using u64=uint64_t;
enum class VMState {Running,Paused};
static VMState state=VMState::Paused;
static unsigned s_emucap_execution_depth=0,s_emucap_state_transition_depth=0,reads=0,dispatches=0;
static u64 g_FrameCount=99;
constexpr u32 EMUCAP_EE_RAM_SIZE=0x2000000;
constexpr u8 MsgEmuCapMemoryBatch=0x97;
struct MainRAM {u8 bytes[32];u8& operator[](size_t at){++reads;return bytes[at];}};
struct RAM {MainRAM Main;} ram;
static RAM *eeMem=&ram;
namespace VMManager {
VMState GetState(){return state;}
u64 GetEmucapMemoryEpoch(){return 7;}
bool IsEmucapExecutionParkedOnCPUThread();
}
namespace Host {void RunOnCPUThread(std::function<void()> fn,bool block){assert(block);++dispatches;fn();}}
template<class T>T FromSpan(std::span<u8> b,u32 at){T v;std::memcpy(&v,b.data()+at,sizeof v);return v;}
template<class T>void ToResultVector(std::vector<u8>& b,T v,u32 at){std::memcpy(b.data()+at,&v,sizeof v);}
bool SafetyChecks(u32 at,u32 n,u32 ret,u32 result,u32 size){return at+n<=size && ret+result<=65552;}
''' + predicate + r'''
static std::vector<u8> batch(std::vector<std::pair<u32,u32>> ranges){
 std::vector<u8> input(4+ranges.size()*8);
 ToResultVector(input,static_cast<u32>(ranges.size()),0);
 for(unsigned i=0;i<ranges.size();i++){ToResultVector(input,ranges[i].first,4+i*8);ToResultVector(input,ranges[i].second,8+i*8);}
 std::span<u8> buf(input);u32 buf_cnt=0,ret_cnt=0,buf_size=input.size();std::vector<u8> ret_buffer(65552);
 switch(MsgEmuCapMemoryBatch){
''' + handler + r'''
 default:error:return {};
 }
 ret_buffer.resize(ret_cnt);return ret_buffer;
}
int main(){
 for(unsigned i=0;i<32;i++)ram.Main.bytes[i]=i+3;
 for(auto [depth,transition]:{std::pair{1u,0u},{0u,1u},{1u,1u}}){
  state=VMState::Paused;s_emucap_execution_depth=depth;s_emucap_state_transition_depth=transition;reads=0;
  assert(batch({{0,4},{2,2}}).empty());assert(reads==0);
 }
 s_emucap_execution_depth=0;s_emucap_state_transition_depth=0;
 state=VMState::Running;assert(batch({{0,4}}).empty() && reads==0);
 state=VMState::Paused;auto result=batch({{0,4},{2,2}});
 assert(result.size()==22 && reads==2);
 assert(FromSpan<u64>(result,0)==7 && FromSpan<u64>(result,8)==99);
 assert(result[16]==3 && result[19]==6 && result[20]==5 && result[21]==6);
 reads=0;dispatches=0;
 assert(batch({{0,4},{EMUCAP_EE_RAM_SIZE-1,2}}).empty());assert(reads==0 && dispatches==0);
}
'''
with tempfile.TemporaryDirectory(prefix='ps2-batch-park-') as d:
    path=Path(d);(path/'test.cpp').write_text(code)
    subprocess.run(['clang++','-std=c++20','-fsanitize=address,undefined','-g',str(path/'test.cpp'),'-o',str(path/'test')],check=True)
    subprocess.run([str(path/'test')],check=True)
print('PASS PCSX2 actual batch rejects incomplete CPU/worker park with zero reads; admitted copy and last-range preflight pass')
