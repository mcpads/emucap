#!/usr/bin/env python3
"""GS archives determine their own bounded length, independently of the current renderer."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
text = (root / 'adapters/pcsx2/work/pcsx2/pcsx2/SaveState.cpp').read_text()
start = text.index('static bool SysState_GSFreezeIn(zip_file_t*')
end = text.index('\nclass SavestateEntry_GS', start)
code = r'''
#include <algorithm>
#include <array>
#include <cassert>
#include <cstdint>
#include <cstring>
#include <vector>
using u8=uint8_t;using u32=uint32_t;using zip_int64_t=int64_t;
struct GSDeinterlaceHistory{static constexpr uint64_t MaxPixels=16;};
struct zip_file_t{std::vector<u8> bytes;std::size_t cursor=0;bool fail=false;};
zip_int64_t zip_fread(zip_file_t* file,void* out,std::size_t size){
 if(file->fail)return -1;
 size=std::min(size,file->bytes.size()-file->cursor);
 std::memcpy(out,file->bytes.data()+file->cursor,size);file->cursor+=size;return size;
}
struct freezeData{int size;u8* data;};
enum class FreezeAction{Load};
static unsigned loads=0;static std::vector<u8> expected;
static int SysState_MTGSFreeze(FreezeAction action,freezeData* state){
 assert(action==FreezeAction::Load);++loads;
 assert(std::vector<u8>(state->data,state->data+state->size)==expected);return 0;
}
''' + text[start:end] + r'''
int main(){
 // Successive differently sized entries cannot inherit the current renderer's size.
 for(std::size_t size : {4u, 131077u, 17u}){
  zip_file_t file;file.bytes.resize(size);
  for(std::size_t i=0;i<size;i++)file.bytes[i]=static_cast<u8>(i);
  expected=file.bytes;assert(SysState_GSFreezeIn(&file));
 }
 assert(loads==3);
 zip_file_t empty;assert(!SysState_GSFreezeIn(&empty));
 zip_file_t error;error.fail=true;assert(!SysState_GSFreezeIn(&error));
 zip_file_t huge;huge.bytes.resize(16*1024*1024+65);assert(!SysState_GSFreezeIn(&huge));
 assert(loads==3);
}
'''
with tempfile.TemporaryDirectory() as directory:
    source, binary = Path(directory) / 'probe.cpp', Path(directory) / 'probe'
    source.write_text(code)
    subprocess.run(['clang++', '-std=c++20', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined', str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('PCSX2 GS archive reader: variable lengths, complete bytes, empty/read-error/oversize rejection passed')
