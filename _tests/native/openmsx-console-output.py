#!/usr/bin/env python3
"""Inherited XML pipes and log files survive native Windows console setup."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
source = root / 'adapters/openmsx/work/openmsx-21.0/src/main.cc'
text = source.read_text()
start = text.index('static void EnableConsoleOutput()')
end = text.index('\n#endif', start)
function = text[start:end]
code = r'''
#include <cassert>
#include <cstdio>
using DWORD = unsigned;
constexpr DWORD STD_OUTPUT_HANDLE=1, STD_ERROR_HANDLE=2, ATTACH_PARENT_PROCESS=0;
constexpr DWORD FILE_TYPE_UNKNOWN=0, FILE_TYPE_DISK=1, FILE_TYPE_CHAR=2, FILE_TYPE_PIPE=3;
DWORD handles[3]{}; int attaches=0, reopens=0;
DWORD GetStdHandle(DWORD stream){return handles[stream];}
DWORD GetFileType(DWORD handle){return handle;}
bool AttachConsole(DWORD){++attaches;return true;}
FILE* reopen(const char*,const char*,FILE* f){++reopens;return f;}
#define freopen reopen
''' + function + r'''
int main(){
 for(DWORD out:{FILE_TYPE_UNKNOWN,FILE_TYPE_DISK,FILE_TYPE_CHAR,FILE_TYPE_PIPE})
  for(DWORD err:{FILE_TYPE_UNKNOWN,FILE_TYPE_DISK,FILE_TYPE_CHAR,FILE_TYPE_PIPE}){
   handles[STD_OUTPUT_HANDLE]=out;handles[STD_ERROR_HANDLE]=err;attaches=reopens=0;
   EnableConsoleOutput();
   bool redirected=out==FILE_TYPE_PIPE||out==FILE_TYPE_DISK||err==FILE_TYPE_PIPE||err==FILE_TYPE_DISK;
   assert(attaches==(redirected?0:1));assert(reopens==(redirected?0:2));
  }
}
'''
code = '#include <initializer_list>\n' + code
with tempfile.TemporaryDirectory() as directory:
    path = Path(directory) / 'probe.cpp'
    binary = Path(directory) / 'probe'
    path.write_text(code)
    subprocess.run(['clang++', '-std=c++20', '-Wall', '-Wextra', '-Werror', str(path), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('openMSX console setup preserves inherited pipe/file handles for both output streams')
