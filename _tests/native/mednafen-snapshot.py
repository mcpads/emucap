#!/usr/bin/env python3
"""Check external history framing before any native guest mutation."""
from pathlib import Path
import os
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[2]
source = root / 'adapters/mednafen/work/mednafen'
adapter = root / 'adapters/mednafen'
code = r'''
#include "emucap_snapshot.h"
#include <cassert>
using Bytes=std::vector<uint8_t>;
void put(Bytes& b,size_t p,uint64_t n,unsigned count=4) {
 for(unsigned i=0;i<count;++i)b[p+i]=n>>(8*i);
}
void sign(Bytes& b) {auto h=Mednafen::sha256(b.data(),b.size()-32);std::copy(h.begin(),h.end(),b.end()-32);}
int main() {
 Bytes native(32,0);std::memcpy(native.data(),"MDFNSVST",8);
 put(native,16,0x103201);put(native,20,native.size());
 uint8_t content[16]={9};auto id=EmucapSnapshot::identity("md",content);
 EmucapSnapshot::Blocks blocks{{{1,2},{3},{4,5,6},{}}};
 auto reject=[&](const Bytes& b) {try {EmucapSnapshot::decode(b.data(),b.size(),id);assert(false);}catch(const std::runtime_error&) {}};
 for(bool active : {false,true}) {
  blocks[3]=active ? Bytes{7,8,9} : Bytes{};
  auto encoded=EmucapSnapshot::encode(native.data(),native.size(),id,blocks);
  auto result=EmucapSnapshot::decode(encoded.data(),encoded.size(),id);
  assert(result.history && result.native_size==native.size() && result.blocks==blocks);
  // Native bytes remain usable independently by the legacy native reader.
  EmucapStateFile::validate(encoded.data(),result.native_size);
  for(size_t size=0;size<encoded.size();++size) {
   if(size==native.size()) continue; // Explicit native-only compatibility.
   reject(Bytes(encoded.begin(),encoded.begin()+size));
  }
  for(size_t at=0;at<encoded.size();++at) {auto b=encoded;b[at]^=1;reject(b);}
  for(unsigned i=0;i<4;++i) {
   auto b=encoded;put(b,native.size()+40+i*8,UINT64_MAX,8);sign(b);reject(b);
  }
  auto extra=encoded;extra.insert(extra.end()-32,0);sign(extra);reject(extra);
  auto identity=encoded;identity[native.size()+24]^=1;sign(identity);reject(identity);
  auto missing=encoded;put(missing,native.size()+40,0,8);sign(missing);reject(missing);
 }
 auto old=EmucapSnapshot::decode(native.data(),native.size(),id);assert(!old.history);
 try {EmucapSnapshot::decode(native.data(),EmucapSnapshot::max_bytes+1,id);assert(false);}catch(const std::runtime_error&){}
 try {auto bad=blocks;bad[1].clear();EmucapSnapshot::encode(native.data(),native.size(),id,bad);assert(false);}catch(const std::runtime_error&){}
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-snapshot-') as temp:
    cpp = Path(temp) / 'test.cpp'; binary = Path(temp) / 'test'
    cpp.write_text(code)
    libs = ['-liconv', '-framework', 'CoreFoundation'] if sys.platform == 'darwin' else []
    subprocess.run(['clang++', '-std=c++11', '-DHAVE_CONFIG_H', '-O1',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I' + str(source / 'include'), '-I' + str(source / 'intl'), '-I' + str(adapter),
        str(cpp), str(source / 'src/hash/sha256.cpp'), str(source / 'src/error.cpp'),
        str(source / 'src/libtrio.a'), str(source / 'intl/libintl.a'), *libs,
        '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True,
        env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1'))
print('Snapshot: native prefix, active/idle blocks, legacy admission, identity, digest, truncation and length rejection pass ASan/UBSan')
