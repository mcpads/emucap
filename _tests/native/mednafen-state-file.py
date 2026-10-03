#!/usr/bin/env python3
"""Native state framing admission, including empty records and late corruption."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--state', type=Path, action='append', default=[])
args = parser.parse_args()
code = r'''
#include "emucap_state_file.h"
#include <cassert>
#include <fstream>
#include <iterator>
#include <vector>
using Bytes=std::vector<uint8_t>;
void put(Bytes& b,uint32_t n) { for(int i=0;i<4;++i)b.push_back(n>>(8*i)); }
void set(Bytes& b,size_t p,uint32_t n) { for(int i=0;i<4;++i)b[p+i]=n>>(8*i); }
Bytes field(std::string name,Bytes data) {
 Bytes b{uint8_t(name.size())};b.insert(b.end(),name.begin(),name.end());
 put(b,data.size());b.insert(b.end(),data.begin(),data.end());return b;
}
void section(Bytes& b,std::string name,Bytes data) {
 assert(name.size()<=32);const auto p=b.size();b.resize(p+32,0);
 std::copy(name.begin(),name.end(),b.begin()+p);put(b,data.size());
 b.insert(b.end(),data.begin(),data.end());
}
Bytes header() {
 Bytes b(32,0);std::memcpy(b.data(),"MDFNSVST",8);set(b,16,0x00103201);return b;
}
void finish(Bytes& b) {set(b,20,b.size());}
void accept(const Bytes& b) {EmucapStateFile::validate(b.data(),b.size());}
void reject(const Bytes& b) {try {accept(b);assert(false);}catch(const std::runtime_error&) {}}
int main(int argc,char** argv) {
 auto valid=header();auto payload=field("empty",{});auto data=field("later",{1,2,3});
 payload.insert(payload.end(),data.begin(),data.end());
 section(valid,"MAIN",payload);section(valid,"EMPTY",{});finish(valid);accept(valid);
 auto alternate=valid;std::memcpy(alternate.data(),"MEDNAFENSVESTATE",16);accept(alternate);
 set(alternate,20,alternate.size()|0x80000000U);accept(alternate);
 for(size_t i=0;i<valid.size();++i)reject(Bytes(valid.begin(),valid.begin()+i));
 for(int fault=0;fault<8;++fault) {
  auto bad=valid;
  switch(fault) {
   case 0:bad[0]='?';break;
   case 1:set(bad,16,0x8ff);break;
   case 2:set(bad,16,0x80000000);break;
   case 3:set(bad,24,0xffffffff);set(bad,28,0xffffffff);break;
   case 4:set(bad,64,0xffffffff);break;
   case 5:bad[68]=255;break;
   case 6:set(bad,68+1+5,0xffffffff);break;
   case 7:bad.push_back(0);finish(bad);break;
  }
  reject(bad);
 }
 auto duplicate=valid;section(duplicate,"MAIN",{});finish(duplicate);reject(duplicate);
 auto duplicate_field=header();auto f=field("same",{});const auto copy=f;f.insert(f.end(),copy.begin(),copy.end());
 section(duplicate_field,"MAIN",f);finish(duplicate_field);reject(duplicate_field);
 auto full_name=header();section(full_name,std::string(32,'x'),{});finish(full_name);accept(full_name);
 auto preview=header();set(preview,24,2);set(preview,28,1);preview.resize(38);
 section(preview,"MAIN",{});finish(preview);accept(preview);
 auto many=header();for(int i=0;i<4097;++i)section(many,std::to_string(i),{});
 finish(many);reject(many);
 auto many_fields=header();Bytes records;
 for(int i=0;i<65537;++i) {auto item=field(std::to_string(i),{});records.insert(records.end(),item.begin(),item.end());}
 section(many_fields,"MAIN",records);finish(many_fields);reject(many_fields);
 // Reject oversized input before touching beyond the known header allocation.
 try {EmucapStateFile::validate(valid.data(),EmucapStateFile::max_bytes+1);assert(false);}
 catch(const std::runtime_error&) {}
 for(int i=1;i<argc;++i) {
  std::ifstream file(argv[i],std::ios::binary);assert(file.good());
  Bytes b((std::istreambuf_iterator<char>(file)),std::istreambuf_iterator<char>());accept(b);
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-state-file-') as temp:
    cpp = Path(temp) / 'test.cpp'
    binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    '-Iadapters/mednafen', str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary), *[str(path.resolve()) for path in args.state]], check=True)
print('Native state framing: signatures, endian flag, empty records, late corruption, duplicates and bounds pass ASan/UBSan')
