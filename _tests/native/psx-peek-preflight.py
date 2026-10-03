#!/usr/bin/env python3
"""Actual Mednafen admission rejects PSX device aliases before any batch reads."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2];source=(root/'adapters/mednafen/emucap.cpp').read_text()
def function(signature):
 start=source.index(signature);return source[start:source.index('\n}',start)+2]+'\n'
code=r'''
#include <cassert>
#include "emucap_psx.h"
#include "emucap_pacing.h"
#include "emucap_json.hpp"
using uint32=std::uint32_t;using uint64=std::uint64_t;using nlohmann::json;
struct AddressSpaceType{uint64 size=0x100000000ULL;} space;
AddressSpaceType* find_aspace(const std::string& s){return s=="cpu"?&space:nullptr;}
bool is_pcfx(){return false;}bool is_psx(){return true;}
bool emucap_pcfx_cpu_peek_range(uint64,uint64){assert(false);return false;}
constexpr long MAX_READ_LEN=65536;
bool g_frozen=true;uint64 g_frame=17,g_boundary_seq=3;
int reads=0,sends=0;bool success=false;
void reply_err(long,const char*,const char*){++sends;success=false;}
void reply_ok(long,const std::string&){++sends;success=true;}
bool reject_ss_physical_read(long,const std::string&){return false;}
bool read_aspace_hex(const std::string&,uint32,long length,std::string& hex){++reads;hex.assign(length*2,'1');return true;}
std::string json_escape(const std::string& s){return s;}
std::string runtime_generation(){return "fixture";}
'''+function('bool validate_aspace_range(')+function('void handle_read_memory_batch(')+r'''
int main(){
 auto first=json{{"memory_type","cpu"},{"address",0},{"length",1}};
 for(uint64 alias:{0ULL,0x80000000ULL,0xa0000000ULL}){
  for(uint64 address:{0x1f801024ULL,0x1f801810ULL,0x1f802fffULL}){
   reads=sends=0;auto last=first;last["address"]=alias+address;
   handle_read_memory_batch(1,json{{"params",{{"ranges",json::array({first,last})}}}}.dump());
   assert(sends==1 && !success && reads==0 && g_frozen && g_frame==17 && g_boundary_seq==3);
  }
 }
 reads=sends=0;
 handle_read_memory_batch(2,json{{"params",{{"ranges",json::array({first,first})}}}}.dump());
 assert(sends==1 && success && reads==2);
}
'''
with tempfile.TemporaryDirectory(prefix='psx-peek-preflight-') as d:
 p=Path(d);(p/'check.cpp').write_text(code)
 subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined','-I'+str(root/'adapters/mednafen'),'-I'+str(root/'adapters/mednafen/work/mednafen/src/drivers'),str(p/'check.cpp'),'-o',str(p/'check')],check=True)
 subprocess.run([str(p/'check')],check=True)
print('PASS native validation and batch preflight reject PSX device aliases before payload reads')
