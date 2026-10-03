#!/usr/bin/env python3
"""Atomic checkpoint publication with real filesystem and injected I/O failures."""
from pathlib import Path
import subprocess
import tempfile

code = r'''
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <system_error>
#include <cerrno>
#include <cstdio>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#include <cassert>
#include <fstream>
#include <iterator>
#include <dirent.h>
static int fault=0, writes=0;
static bool collide=false;
static std::string collision;
std::string read(const std::string& p) {
 std::ifstream f(p,std::ios::binary);assert(f.good());
 return std::string(std::istreambuf_iterator<char>(f),{});
}
void seed(const std::string& p,const std::string& s) {std::ofstream f(p,std::ios::binary);f<<s;assert(f.good());}
int open_file(const char* p,int flags,mode_t mode) {
 if(collide) {collide=false;collision=p;seed(p,"owned elsewhere");errno=EEXIST;return -1;}
 return ::open(p,flags,mode);
}
ssize_t write_file(int fd,const void* p,size_t n) {
 ++writes;
 if(fault==1) {if(writes==1)return ::write(fd,p,std::min<size_t>(3,n));errno=ENOSPC;return -1;}
 if(fault==5 && writes==1) {errno=EINTR;return -1;}
 if(fault==6) return 0;
 return ::write(fd,p,std::min<size_t>(n,127));
}
int sync_file(int fd) {if(fault==2){errno=EIO;return -1;}return ::fsync(fd);}
int close_file(int fd) {int r=::close(fd);if(fault==3){errno=EIO;return -1;}return r;}
int rename_file(const char* a,const char* b) {if(fault==4){errno=EACCES;return -1;}return ::rename(a,b);}
#define open open_file
#define write write_file
#define fsync sync_file
#define close close_file
#define rename rename_file
#include "emucap_atomic_file.h"
#undef open
#undef write
#undef fsync
#undef close
#undef rename
// The method name also follows the write macro in this deliberately intercepted build.
void publish(const std::string& path,const std::string& bytes) {
 EmucapAtomicFile::write_file(path,bytes.data(),bytes.size());
}
void no_temporary(const std::string& root) {
 auto dir=opendir(root.c_str());assert(dir);
 while(auto item=readdir(dir))assert(std::string(item->d_name).find(".emucap-state-")!=0);
 closedir(dir);
}
int main(int argc,char** argv) {
 assert(argc==2);std::string root=argv[1], path=root+"/checkpoint.mcs";
 const std::string old="original checkpoint", next(8192,'N');
 for(fault=1;fault<=6;++fault) {
  seed(path,old);writes=0;
  try {publish(path,next);assert(fault==5);}catch(const std::exception&) {assert(fault!=5);}
  assert(read(path)==(fault==5?next:old));no_temporary(root);
 }
 fault=0;collide=true;publish(path,next);assert(read(collision)=="owned elsewhere");
 assert(read(path)==next);assert(!::unlink(collision.c_str()));no_temporary(root);
 publish(root+"/new.mcs",old);assert(read(root+"/new.mcs")==old);
 publish(root+"/상태.mcs",next);assert(read(root+"/상태.mcs")==next);
 publish(root+"/back\\slash.mcs",old);assert(read(root+"/back\\slash.mcs")==old);
 const auto long_path=root+"/"+std::string(250,'x');publish(long_path,next);assert(read(long_path)==next);
 const auto link=root+"/link.mcs";assert(!::symlink(path.c_str(),link.c_str()));
 try {publish(link,old);assert(false);}catch(const std::exception&) {}
 assert(read(path)==next);struct stat st;assert(!::lstat(link.c_str(),&st) && S_ISLNK(st.st_mode));
 try {publish(root,old);assert(false);}catch(const std::exception&) {}
 try {publish(root+"/missing/state.mcs",old);assert(false);}catch(const std::exception&) {}
 try {publish(path+std::string(1,'\0')+"suffix",old);assert(false);}catch(const std::exception&) {}
 no_temporary(root);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-atomic-file-') as temp:
    root = Path(temp)
    cpp, binary = root / 'test.cpp', root / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    '-Iadapters/mednafen', str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary), str(root)], check=True)
print('Atomic state publication: partial write/sync/close/rename failure, EINTR, short writes, collision ownership and path cases pass ASan/UBSan')
