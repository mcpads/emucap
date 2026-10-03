#!/usr/bin/env python3
"""Run actual native batch handlers with counted/protected payload storage.

Checks preflight and publication; scheduler barriers and real device peek behavior
belong to separate runtime/owner witnesses.
"""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
common = r'''
#include <cassert>
#include <stdexcept>
#include <cstring>
#include <sys/mman.h>
#include <unistd.h>
#include "emucap_pacing.h"
#include "emucap_json.hpp"
using nlohmann::json;
using uint32=std::uint32_t;
using u8=unsigned char;
bool g_frozen=true, success=false, deny=false, fail_read=false, throw_read=false;
int payload_reads=0, sends=0;
std::uint64_t g_frame=17, g_boundary_seq=3;
json reply;
std::string error_code;
u8* memory;
std::size_t page_size;
std::string runtime_generation() { return "fixture-generation"; }
std::string json_escape(const std::string& s) { return s; }
void reply_ok(long,const std::string& value) { ++sends;success=true;reply=json::parse(value); }
void reply_err(long,const char* code,const char*) { ++sends;success=false;error_code=code;reply=nullptr; }
bool reject_ss_physical_read(long id,const std::string& type) {
 if(type=="device") {reply_err(id,"unsupported","");return true;}return false;
}
bool validate_aspace_range(const std::string& type,uint32 address,long length) {
 return type=="ram" && address<=16 && length<=16-address;
}
bool read_aspace_hex(const std::string&,uint32 address,long length,std::string& hex) {
 ++payload_reads;assert(!deny);
 if(throw_read && payload_reads==2) throw std::runtime_error("sector read failed");
 if(fail_read && payload_reads==2) return false;
 for(long i=0;i<length;i++) {char byte[3];std::snprintf(byte,3,"%02x",memory[address+i]);hex+=byte;}
 return true;
}
struct DCRegion { std::uint64_t size; } region{16};
const DCRegion* find_region(const std::string& type) { return type=="ram"?&region:nullptr; }
const u8* region_backing(const DCRegion&) {return memory;}
'''
checks = r'''
json range(std::uint64_t address,std::uint64_t length,const char* type="ram") {
 return {{"memory_type",type},{"address",address},{"length",length}};
}
void run(json ranges,bool accepted) {
 payload_reads=sends=0;success=false;reply=nullptr;
 const std::string request=json({{"params",{{"ranges",ranges}}}}).dump();
 handle_read_memory_batch(1,request);
 assert(sends==1 && success==accepted);
 assert(g_frame==17 && g_boundary_seq==3);
 if(!accepted) {assert(reply.is_null()); if(deny) assert(payload_reads==0);}
}
int main() {
 page_size=sysconf(_SC_PAGESIZE);
 memory=static_cast<u8*>(mmap(nullptr,page_size,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_ANON,-1,0));
 assert(memory!=MAP_FAILED);
 for(unsigned i=0;i<16;i++) memory[i]=i;
 deny=true; assert(mprotect(memory,page_size,PROT_NONE)==0);
 const auto first=range(0,4);
 for(const auto& last : {range(16,1),range(15,2),range(UINT64_MAX,2),
                         range(0,1,"device"),range(0,0),range(0,65537)}) {
  run(json::array({first,last}),false);
 }
 run(json::array(),false);
 json too_many=json::array();for(int i=0;i<65;i++) too_many.push_back(range(0,1));
 run(too_many,false);
 g_frozen=false;run(json::array({first}),false);g_frozen=true;
 deny=false;assert(mprotect(memory,page_size,PROT_READ|PROT_WRITE)==0);
 auto ranges=json::array({range(3,4),range(3,4),range(5,3),range(15,1)});
 run(ranges,true);
 assert(reply["total_bytes"]==12 && reply["reads"].size()==4);
 const char* expected[]={"03040506","03040506","050607","0f"};
 for(unsigned i=0;i<4;i++) {
  const auto& read=reply["reads"][i];assert(read["index"]==i);
  assert(read["address"]==ranges[i]["address"] && read["length"]==ranges[i]["length"]);
  assert(read["hex"]==expected[i]);
 }
 assert(reply["boundary"]["runtime_generation"]=="fixture-generation");
 assert(reply["boundary"]["stop_epoch"]=="f17#s3");
 for(unsigned i=0;i<16;i++) assert(memory[i]==i);
 FAILURE_CASE
 assert(munmap(memory,page_size)==0);
}
'''
for adapter in ['mednafen','flycast']:
    source=(root/f'adapters/{adapter}/emucap.cpp').read_text()
    start=source.index('void handle_read_memory_batch(')
    handler=source[start:source.index('\n}',start)+2]
    failure=r"""
    fail_read=true;run(json::array({first,first}),false);assert(payload_reads==2);
    fail_read=false;throw_read=true;bool caught=false;
    try{run(json::array({first,first}),true);}catch(const std::runtime_error&){caught=true;}
    assert(caught && payload_reads==2 && sends==0 && reply.is_null());
    assert(g_frozen && g_frame==17 && g_boundary_seq==3);
    throw_read=false;run(json::array({first}),true);
    """ if adapter=='mednafen' else ''
    code=common+handler+checks.replace('FAILURE_CASE',failure)
    with tempfile.TemporaryDirectory(prefix='native-batch-preflight-') as directory:
        cpp=Path(directory)/'check.cpp';binary=Path(directory)/'check';cpp.write_text(code)
        subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined',
            '-I'+str(root/f'adapters/{adapter}'),
            '-I'+str(root/'adapters/mednafen/work/mednafen/src/drivers'),
            str(cpp),'-o',str(binary)],check=True)
        subprocess.run([str(binary)],check=True)
    print(f'PASS {adapter}: unreadable payload preflight, ordered overlapping/duplicate/last-byte reads and single publication',flush=True)
