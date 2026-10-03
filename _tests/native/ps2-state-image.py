#!/usr/bin/env python3
"""Decode the maintained state PNG reader, including rejected and truncated images."""
from pathlib import Path
import shlex
import struct
import subprocess
import tempfile
import zlib
root = Path(__file__).resolve().parents[2]
text = (root / 'adapters/pcsx2/work/pcsx2/pcsx2/SaveState.cpp').read_text()
start = text.index('static bool SaveState_ReadScreenshot(zip_t*')
end = text.index('\n// ---', start)
function = text[start:end]
code = r'''
#include <png.h>
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <iterator>
#include <memory>
#include <vector>
using u8=uint8_t;using u32=uint32_t;using u64=uint64_t;using zip_int64_t=int64_t;
struct zip_t{std::vector<u8> data;};struct zip_file_t{zip_t* zip;size_t cursor=0;};
auto zip_fopen_managed(zip_t* zip,const char*,int){return std::make_unique<zip_file_t>(zip);}
zip_int64_t zip_fread(zip_file_t* file,void* output,size_t size){
 size=std::min(size,file->zip->data.size()-file->cursor);
 std::memcpy(output,file->zip->data.data()+file->cursor,size);file->cursor+=size;return size;
}
constexpr auto EntryFilename_Screenshot="Screenshot.png";
template<class F>struct ScopedGuard{F f;explicit ScopedGuard(F action):f(action){}~ScopedGuard(){f();}};
''' + function + r'''
int main(int argc,char** argv){
 assert(argc>1);
 for(int i=1;i<argc;i++){
  std::ifstream file(argv[i],std::ios::binary);assert(file.good());
  zip_t archive{{std::istreambuf_iterator<char>(file),std::istreambuf_iterator<char>()}};
  u32 w=0,h=0;std::vector<u32> pixels;
  bool ok=SaveState_ReadScreenshot(&archive,&w,&h,&pixels);
  bool expected=std::string(argv[i]).find("good-")!=std::string::npos;
  assert(ok==expected);
  if(ok){assert(w==2&&h==1);assert(pixels==std::vector<u32>({0xff332211,0xff665544}));}
 }
}
'''
def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
def png(color, row, width=2, depth=8):
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, 1, depth, color, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(b'\0' + row)) + chunk(b'IEND', b''))
with tempfile.TemporaryDirectory() as directory:
    d = Path(directory)
    source, binary = d / 'probe.cpp', d / 'probe'
    source.write_text(code)
    flags = shlex.split(subprocess.check_output(['pkg-config', '--cflags', '--libs', 'libpng'], text=True))
    subprocess.run(['clang++', '-std=c++20', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined', str(source), '-o', str(binary), *flags], check=True)
    good = png(6, bytes.fromhex('112233ff445566ff'))
    corrupt = bytearray(good);corrupt[-5] ^= 1
    fixtures = {'good-rgba': good, 'good-rgb': png(2, bytes.fromhex('112233445566')),
                'bad-gray': png(0, b'\0\0'), 'bad-wide': png(6, b'', 20000),
                'bad-depth': png(6, b'\0' * 16, depth=16), 'bad-crc': bytes(corrupt),
                'bad-truncated': good[:-5]}
    files = []
    for name, data in fixtures.items():
        path = d / (name + '.png');path.write_bytes(data);files.append(str(path))
    subprocess.run([str(binary), *files], check=True)
print('PCSX2 native state-image reader: RGB/RGBA exact pixels; bounds, formats, CRC and truncation reject')
