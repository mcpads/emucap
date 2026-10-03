#!/usr/bin/env python3
"""Exercise maintained ordinary-session framing with controlled request dispatch."""
from pathlib import Path
import argparse
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
parser=argparse.ArgumentParser()
parser.add_argument('--source',type=Path,default=root/'adapters/dolphin/EmuCap.cpp')
args=parser.parse_args()
s = args.source.read_text()
writer = s[s.index('bool SendLine('):s.index('#include "Core/EmuCapOwned.inl"')]
reader = s[s.index('void ServeSession('):s.index('void ThreadMain(')]
# Keep socket acquisition, line framing and limit checks, replacing only dispatch.
start = reader.index('      picojson::value req;')
end = reader.index('      if (!sent || s_control_retired.load())')
end = reader.index('        return;', end) + len('        return;')
reader = reader[:start] + '      ++dispatches;' + reader[end:]
code = r'''
#include "EmuCapWire.h"
#include <atomic>
#include <cassert>
#include <cerrno>
#include <sys/socket.h>
#include <unistd.h>
#include <thread>
using SOCKET=int;
namespace Core {struct System{};}
std::atomic<bool> s_stop{false};
std::atomic<unsigned> dispatches{0};
''' + writer + reader + r'''
int main(){
 for(int scenario=0;scenario<4;++scenario){
  int fd[2];assert(socketpair(AF_UNIX,SOCK_STREAM,0,fd)==0);
#ifdef __APPLE__
  int one=1;setsockopt(fd[1],SOL_SOCKET,SO_NOSIGPIPE,&one,sizeof(one));
#endif
  dispatches=0;Core::System system;
  std::thread server([&]{ServeSession(system,fd[0]);close(fd[0]);});
  const auto cap=EmuCap::MAX_NDJSON_FRAME_BYTES;
  std::string data;
  if(scenario==0)data=std::string(cap,'x')+"\n{}\n"; // Exact limit + next frame.
  if(scenario==1)data=std::string(cap+1,'x')+"\n";  // Over limit, terminated.
  if(scenario==2)data=std::string(cap+4096,'x');    // Over limit, unterminated.
  if(scenario==3)data="{}\n{}\n";                 // Multiple short frames.
  size_t offset=0;
  while(offset<data.size()){
#ifdef MSG_NOSIGNAL
   int flags=MSG_NOSIGNAL;
#else
   int flags=0;
#endif
   auto n=send(fd[1],data.data()+offset,std::min(size_t(4096),data.size()-offset),flags);
   if(n<=0)break;
   offset+=size_t(n);
  }
  if(scenario==2){
   fd_set readable;FD_ZERO(&readable);FD_SET(fd[1],&readable);timeval wait{2,0};
   assert(select(fd[1]+1,&readable,nullptr,nullptr,&wait)==1);
   char byte;assert(recv(fd[1],&byte,1,0)==0); // Peer closes before client EOF.
  }
  shutdown(fd[1],SHUT_WR);server.join();close(fd[1]);
  assert(dispatches==((scenario==0||scenario==3)?2:0));
 }
 // Reject output before any write; an oversized response cannot be truncated success.
 int fd[2];assert(socketpair(AF_UNIX,SOCK_STREAM,0,fd)==0);
 assert(!SendLine(fd[0],std::string(EmuCap::MAX_NDJSON_FRAME_BYTES+1,'x')));
 char byte;assert(recv(fd[1],&byte,1,MSG_DONTWAIT)==-1 && (errno==EAGAIN || errno==EWOULDBLOCK));
 close(fd[0]);close(fd[1]);
}
'''
with tempfile.TemporaryDirectory(prefix='dolphin-session-bounds-') as d:
    d=Path(d);(d/'check.cpp').write_text(code)
    subprocess.run(['clang++','-std=c++20','-pthread','-fsanitize=address,undefined',
                    '-I'+str(root/'adapters/dolphin'),
                    '-I'+str(root/'adapters/dolphin/work/dolphin-src/Externals/tinygltf/tinygltf'),
                    str(d/'check.cpp'),'-o',str(d/'check')],check=True)
    subprocess.run([str(d/'check')],check=True,timeout=30)
print('PASS ordinary Dolphin socket framing: exact limit, terminated/unterminated overflow, consecutive frames')
