#!/usr/bin/env python3
"""Retained native pixels are immutable, bounded by their dimensions and explicitly invalidated."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
native = root / 'adapters/pcsx2/work/pcsx2/pcsx2/GS/Renderers/Common'
code = r'''
#include "GSRestoredSnapshot.h"
#include <cassert>
int main() {
 GSRestoredSnapshot image;std::uint32_t w=0,h=0;std::vector<std::uint32_t> pixels;
 auto read=[&]{return image.Copy(0,0,true,false,&w,&h,&pixels);};
 assert(!read());
 image.Set(2,1,{0xff123456,0xffabcdef});assert(read());
 assert(w==2&&h==1&&pixels==std::vector<std::uint32_t>({0xff123456,0xffabcdef}));
 pixels[0]=0;assert(read()&&pixels[0]==0xff123456);
 assert(!image.Copy(640,480,true,false,&w,&h,&pixels));
 assert(!image.Copy(0,0,false,false,&w,&h,&pixels));
 assert(!image.Copy(0,0,true,true,&w,&h,&pixels));
 image.Clear();assert(!read());
 image.Set(2,2,{1});assert(!read());
 image.Set(0xffffffff,0xffffffff,{1});assert(!read());
 image.Set(1,1,{5});assert(read()&&pixels[0]==5);
 image.Set(0,1,{});assert(!read());
}
'''
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / 'probe.cpp'
    binary = Path(directory) / 'probe'
    source.write_text(code)
    subprocess.run(['clang++', '-std=c++20', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined', '-I', str(native), str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('PCSX2 retained capture: exact copy, caller isolation, unsupported transforms, invalid dimensions and reset passed')
