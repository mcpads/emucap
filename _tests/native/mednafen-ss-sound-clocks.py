#!/usr/bin/env python3
"""Check native Saturn sound state and clock conversion across restoration."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source',type=Path,default=Path('adapters/mednafen/work/mednafen'))
args=parser.parse_args()
source=(args.source/'src/ss/sound.cpp').read_text()
start=source.index('void SOUND_StateAction(')
state=source[start:source.index('\n//\n//',start)]
start=source.index('sscpu_timestamp_t SOUND_Update(')
update=source[source.index('{',start)+1:source.index(' MDFN_setjmp(jbuf);',start)]
code=r'''
#include <cassert>
#include <cstdint>
#include <cstring>
#include <map>
#include <string>
#include <vector>
#include <stdexcept>
using int32=int32_t; using int64=int64_t; using uint32=uint32_t; using uint64=uint64_t;
using sscpu_timestamp_t=int32;
struct StateMem { std::map<std::string,std::vector<uint8_t>> fields; bool fail=false; };
struct SFORMAT { void* pointer; size_t size; const char* name; };
#define SFVAR(v) {&v,sizeof(v),#v}
#define SFVARN(v,n) {&v,sizeof(v),n}
#define SFEND {nullptr,0,nullptr}
static void MDFNSS_StateAction(StateMem* sm,unsigned load,bool,SFORMAT* fields,const char*) {
 for(auto f=fields; f->pointer; ++f) {
  if(sm->fail) throw std::runtime_error("injected writer failure");
  if(load) { auto it=sm->fields.find(f->name); if(it!=sm->fields.end()) {
   assert(it->second.size()==f->size); memcpy(f->pointer,it->second.data(),f->size);
  }} else {
   auto p=static_cast<uint8_t*>(f->pointer); sm->fields[f->name]={p,p+f->size};
  }
 }
}
struct Device {
 int32 timestamp=0;
 void StateAction(StateMem*,unsigned,bool,const char*) {}
} SoundCPU,SCSP;
static int64 run_until_time;
static int32 next_scsp_time,lastts;
static uint32 clock_ratio;
''' + state + '\nvoid update(int32 timestamp) {\n' + update + r'''
}
static void origin() {
 SoundCPU.timestamp=3000; run_until_time=(int64(3040)<<32)+7;
 next_scsp_time=3200; lastts=1000; clock_ratio=0x40000000;
}
int main() {
 StateMem saved; origin(); SOUND_StateAction(&saved,0,false);
 assert(SoundCPU.timestamp==3000 && run_until_time==(int64(3040)<<32)+7);
 assert(next_scsp_time==3200 && lastts==1000 && clock_ratio==0x40000000);
 update(1020); const int64 reference=run_until_time-(int64(SoundCPU.timestamp)<<32);
 for(auto destination_ratio : {0x40000000U,0x50000000U}) {
  SoundCPU.timestamp=9000; lastts=1500; clock_ratio=destination_ratio;
  SOUND_StateAction(&saved,1,false);
  assert(SoundCPU.timestamp==9000 && next_scsp_time==9200);
  assert(lastts==1000 && clock_ratio==0x40000000);
  update(1020);
  assert(run_until_time-(int64(SoundCPU.timestamp)<<32)==reference);
 }
 origin(); StateMem broken; broken.fail=true;
 try { SOUND_StateAction(&broken,0,false); assert(false); } catch(const std::runtime_error&) {}
 assert(run_until_time==(int64(3040)<<32)+7 && next_scsp_time==3200);
 assert(lastts==1000 && clock_ratio==0x40000000);
 saved.fields.erase("lastts"); saved.fields.erase("clock_ratio");
 lastts=123; clock_ratio=0x50000000; SOUND_StateAction(&saved,1,false);
 assert(lastts==0 && clock_ratio==0x50000000); // Legacy fallback is not exact history proof.
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-ss-clocks-') as tmp:
    cpp=Path(tmp)/'test.cpp';binary=Path(tmp)/'test';cpp.write_text(code)
    subprocess.run(['clang++','-std=c++11','-O1','-fsanitize=address,undefined',str(cpp),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)
print('Native Saturn sound: clock anchor/ratio, retained CPU origin, save failure and legacy fallback pass ASan/UBSan')
