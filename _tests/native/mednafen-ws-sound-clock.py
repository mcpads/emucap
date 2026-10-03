#!/usr/bin/env python3
"""Exercise native WonderSwan PSG time origins; sample output is a no-op sink."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--sound-source', type=Path,
                    default=Path('adapters/mednafen/work/mednafen/src/wswan/sound.cpp'))
parser.add_argument('--baseline', action='store_true')
parser.add_argument('--cpu-source', type=Path,
                    default=Path('adapters/mednafen/work/mednafen/src/wswan/v30mz.cpp'))
args = parser.parse_args()
source = args.sound_source.read_text()

def function(name, text=source):
    start = text.index('void ' + name + '(')
    opening = text.index('{', start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]

native = source[source.index('static uint16 period[4];'):source.index('void WSwan_SoundWrite(')]
code = r'''
#include <cassert>
#include <cstdint>
#include <cstring>
#include <map>
#include <string>
#include <vector>
using uint8=uint8_t; using int8=int8_t; using uint16=uint16_t;
using int16=int16_t; using uint32=uint32_t; using int32=int32_t;
uint32 v30mz_timestamp;
uint8 wsRAM[65536]{};
struct Buffer { void clear() {} } buffers[2];
Buffer* sbuf[2]={&buffers[0], &buffers[1]};
struct Synth { void offset_inline(int32,int32,Buffer*) {} } WaveSynth;
struct StateMem { std::map<std::string,std::vector<uint8>> fields; };
struct SFORMAT { void* pointer; size_t size; const char* name; };
#define SFVAR(v) {&v,sizeof(v),#v}
#define SFVARN(v,n) {&v,sizeof(v),n}
#define SFPTR16N(v,c,n) {v,2*c,n}
#define SFEND {nullptr,0,nullptr}
void MDFNSS_StateAction(StateMem* sm,unsigned load,bool,SFORMAT* fields,const char*) {
 for(auto f=fields; f->pointer; ++f) {
  if(load) {
   auto it=sm->fields.find(f->name);
   if(it!=sm->fields.end()) {
    assert(it->second.size()==f->size); memcpy(f->pointer,it->second.data(),f->size);
   }
  } else { auto p=static_cast<uint8*>(f->pointer); sm->fields[f->name]={p,p+f->size}; }
 }
}
struct CPU { uint16 pc; struct { uint16 w[8]; } regs; uint16 sregs[4]; } I{};
int32 v30mz_ICount, prefix_base, seg_prefix;
bool InHLT;
uint16 CompressFlags() { return 0; }
void ExpandFlags(uint16) {}
''' + function('v30mz_StateAction', args.cpu_source.read_text()) + '\n' + native + function('WSwan_SoundStateAction') + '\n' + function('WSwan_SoundReset') + r'''
StateMem capture() {
 StateMem sm; v30mz_StateAction(&sm,0,false); WSwan_SoundStateAction(&sm,0,false); return sm;
}
int main() {
 v30mz_timestamp=0; WSwan_SoundReset();
 control=0x8f; noise_control=0x10;
 for(unsigned ch=0;ch<4;ch++) period[ch]=2000;
 v30mz_timestamp=100; WSwan_SoundUpdate();
 StateMem saved=capture();
 std::vector<StateMem> reference;
 for(auto time : {800,900,1200}) {
  v30mz_timestamp=time; WSwan_SoundUpdate(); reference.push_back(capture());
 }
 for(auto destination : {0U,40U,700U}) {
  v30mz_timestamp=destination;
  last_ts=destination;
  v30mz_StateAction(&saved,1,false);
  WSwan_SoundStateAction(&saved,1,false);
  assert(v30mz_timestamp==100);
  unsigned i=0;
  for(auto time : {800,900,1200}) {
   v30mz_timestamp=time; WSwan_SoundUpdate();
   assert(capture().fields==reference[i++].fields);
  }
 }
 saved.fields.erase("last_ts"); saved.fields.erase("v30mz_timestamp");
 last_ts=700; v30mz_timestamp=700;
 v30mz_StateAction(&saved,1,false); assert(v30mz_timestamp==0);
 WSwan_SoundStateAction(&saved,1,false); assert(last_ts==0);
 last_ts=700; v30mz_timestamp=0; WSwan_SoundReset(); assert(last_ts==0);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-ws-sound-clock-') as temp:
    cpp = Path(temp) / 'test.cpp'; binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    '-fno-omit-frame-pointer', str(cpp), '-o', str(binary)], check=True)
    result = subprocess.run([str(binary)], capture_output=True, text=True,
                            env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1'))
    if args.baseline:
        assert result.returncode != 0 and 'Assertion' in result.stderr, result
        print('Original CPU/PSG clock continuation fails:', result.stderr.strip())
    else:
        assert result.returncode == 0, result.stderr
        print('Native CPU/PSG: three destination origins, tone/noise continuation, legacy/reset pass ASan/UBSan')
