#!/usr/bin/env python3
"""Native batch/epoch admission with controlled replacement effects and fixed VI.

Exercise both maintained dispatch admission fragments with the actual classifier
and batch handler. Socket framing and native serialization have separate owners.
"""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
native = root / 'adapters/dolphin/work/dolphin-src'
source = (root / 'adapters/dolphin/EmuCap.cpp').read_text()
owned = (root / 'adapters/dolphin/EmuCapOwned.inl').read_text()


def function(text, signature):
    start = text.index(signature)
    end = text.index('{', start) + 1
    depth = 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]


classifier = function(source, 'bool ObservationMethod(')
batch = function(source, 'picojson::object ReadMemoryBatch(')
# Keep the real admission expressions and their position before effect dispatch.
start = source.index('        if (!ObservationMethod(method))', source.index('void ServeSession('))
legacy = source[start:source.index('        const bool advance', start)]
start = owned.index('if (!ObservationMethod(request.method))')
end = owned.index(';', start) + 1
admitted = owned[start:end]
assert '++s_boundary_seq' in admitted

setup = r'''
#include <algorithm>
#include <atomic>
#include <cassert>
#include <cstdint>
#include <cstring>
#include <functional>
#include <stdexcept>
#include <string>
#include <vector>
#include "picojson.h"
using u64=uint64_t;using u32=uint32_t;
uint8_t ram[48]={};
int payload_reads=0, acquisitions=0, releases=0;
std::atomic<u64> s_boundary_seq{0};
constexpr u64 BATCH_MAX_RANGES=64,BATCH_MAX_BYTES=65536;
namespace Core {
enum class State{Paused};
struct Memory{void CopyFromEmu(void* dst,u32 addr,size_t size){++payload_reads;std::memcpy(dst,ram+addr,size);}};
struct System{Memory memory;Memory& GetMemory(){return memory;}};
State GetState(System&){return State::Paused;}
u64 native_epoch=7;bool parked=true,change_after_entry=false;
u64 EmucapWithParkedMemory(System&,u64 expected,const std::function<void()>&callback){
 if(!parked || (expected && expected!=native_epoch))return 0;
 auto epoch=native_epoch;if(expected)++acquisitions;callback();if(expected)++releases;
 if(!expected && change_after_entry)++native_epoch;
 return epoch;
}
}
struct SafeAccess{explicit SafeAccess(Core::System&) {++acquisitions;} ~SafeAccess(){++releases;}};
u64 CurrentFrame(Core::System&){return 42;}
std::string RuntimeGeneration(){return "fixed-generation";}
auto BatchWindows(Core::System&){return std::vector<std::pair<u64,u64>>{{0,16},{32,16}};}
bool GetU64(const picojson::object& p,const char* key,u64& out){out=p.at(key).get<double>();return true;}
picojson::object Fail(const char*,const std::string& message){throw std::runtime_error(message);}
std::string ToHex(const uint8_t* bytes,size_t size){std::string out;const char* hex="0123456789abcdef";for(size_t i=0;i<size;i++){out+=hex[bytes[i]>>4];out+=hex[bytes[i]&15];}return out;}
'''
checks = r'''
int main(){
 Core::System system;
 ram[1]=0xa5;ram[2]=0xc3;ram[15]=0xe7;ram[33]=0x59;ram[47]=0xd2;
 auto descriptor=[](double address,double length){return picojson::value(picojson::object{
  {"memory_type",picojson::value(std::string("main"))},{"address",picojson::value(address)},{"length",picojson::value(length)}});};
 picojson::object known{{"ranges",picojson::value(picojson::array{descriptor(1,2),descriptor(2,1),descriptor(1,2),descriptor(15,1),descriptor(33,1),descriptor(47,1)})}};
 auto reply=ReadMemoryBatch(system,known);
 auto reads=reply.at("reads").get<picojson::array>();
 const char* expected[]={"a5c3","c3","a5c3","e7","59","d2"};
 assert(reads.size()==6 && payload_reads==6 && acquisitions==1 && releases==1);
 for(size_t i=0;i<6;i++){
  auto read=reads[i].get<picojson::object>();
  assert(read.at("hex").get<std::string>()==expected[i]);
  assert(read.at("index").get<double>()==i);
 }
 for(auto bad:{descriptor(15,2),descriptor(31,2),descriptor(47,2),descriptor(0,0)}){
  auto calls=payload_reads, parks=acquisitions;
  try{ReadMemoryBatch(system,{{"ranges",picojson::value(picojson::array{descriptor(1,2),bad})}});assert(false);}
  catch(const std::runtime_error&){}
  assert(payload_reads==calls && acquisitions==parks && releases==parks);
 }
 for(bool change:{false,true}){
  auto calls=payload_reads,parks=acquisitions;
  Core::parked=change;Core::change_after_entry=change;
  try{ReadMemoryBatch(system,known);assert(false);}catch(const std::runtime_error&){}
  assert(payload_reads==calls && acquisitions==parks);
 }
 Core::parked=true;Core::change_after_entry=false;
 picojson::object range{{"memory_type",picojson::value(std::string("main"))},{"address",picojson::value(0.)},{"length",picojson::value(1.)}};
 picojson::object params{{"ranges",picojson::value(picojson::array{picojson::value(range)})}};
 for(bool owned:{false,true}){
  auto snapshot=[&]{admit("read_memory_batch",owned);auto result=ReadMemoryBatch(system,params);return result.at("boundary").serialize();};
  auto before=snapshot();assert(snapshot()==before);
  for(const char* method:{"load_state","reset","write_memory"})for(bool failure:{false,true}){
   auto seq=s_boundary_seq.load();
   admit(method,owned);
   // Replacement seam: retirement must precede a successful or partial effect.
   assert(s_boundary_seq>seq);ram[0]++;
   try{if(failure)throw std::runtime_error("partial restore");}catch(const std::exception&){}
   auto after=snapshot();assert(before!=after);assert(snapshot()==after);before=after;
  }
  for(const char* method:{"save_state","execution_speed"}){admit(method,owned);assert(snapshot()==before);}
 }
}
'''
wrapper = ('void admit(const std::string& method,bool owned){if(owned){'
           'struct Request{std::string method;} request{method};' + admitted
           + '}else{' + legacy + '}}\n')
header = next(native.rglob('picojson.h'))
with tempfile.TemporaryDirectory() as directory:
    cpp = Path(directory) / 'check.cpp'
    binary = Path(directory) / 'check'
    cpp.write_text(setup + classifier + batch + wrapper + checks)
    subprocess.run(['clang++', '-std=c++17', '-O1', '-fsanitize=address,undefined',
                    '-I' + str(header.parent), str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Dolphin native legacy/owned admission + batch: same-VI load/reset/write retire epochs before effects; observations stable')
