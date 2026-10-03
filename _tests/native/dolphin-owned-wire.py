#!/usr/bin/env python3
"""Exercise Dolphin's maintained nonblocking NDJSON I/O over real local sockets."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/dolphin/EmuCapOwned.inl').read_text()
code = r'''
#include "EmuCapWire.h"
#include <cassert>
#include <chrono>
#include <cerrno>
#include <fcntl.h>
#include <sys/socket.h>
#include <unistd.h>
#include <thread>
using SOCKET = int;
namespace Temporal = EmuCap::Temporal;
''' + source[source.index('using OwnedJson'):source.index('struct OwnedSession')] + r'''
int main() {
 static_assert(OWNED_STOP_BUDGET == std::chrono::seconds(5));
 int fd[2]; assert(socketpair(AF_UNIX, SOCK_STREAM, 0, fd)==0);
 for (int sock : fd) {
  assert(fcntl(sock,F_SETFL,fcntl(sock,F_GETFL,0)|O_NONBLOCK)==0);
#ifdef __APPLE__
  int one=1; assert(setsockopt(sock,SOL_SOCKET,SO_NOSIGPIPE,&one,sizeof(one))==0);
#endif
 }
 std::string pending; OwnedJson value;
 assert(ReadOwned(fd[0],pending,value)==OwnedRead::Idle);
 assert(send(fd[1],"{\"id\":",6,0)==6);
 assert(ReadOwned(fd[0],pending,value)==OwnedRead::Idle && pending=="{\"id\":");
 const std::string tail="18446744073709551615}\n{}\n";
 assert(send(fd[1],tail.data(),tail.size(),0)==static_cast<int>(tail.size()));
 assert(ReadOwned(fd[0],pending,value)==OwnedRead::Ready);
 assert(value["id"].get<uint64_t>()==UINT64_MAX);
 assert(ReadOwned(fd[0],pending,value)==OwnedRead::Ready && value.is_object());
 assert(OwnedWrite(fd[0],{{"id",UINT64_MAX},{"ok",true}}));
 std::string received;
 assert(ReadOwned(fd[1],received,value)==OwnedRead::Ready && value["id"].get<uint64_t>()==UINT64_MAX);
 int buffer=1024; setsockopt(fd[0],SOL_SOCKET,SO_SNDBUF,&buffer,sizeof(buffer));
 const auto before=OwnedClock::now();
 assert(!OwnedWrite(fd[0],{{"blob",std::string(1024*1024,'x')}}));
 assert(OwnedClock::now()-before<std::chrono::milliseconds(250));
 close(fd[1]);
 assert(!OwnedWrite(fd[0],{{"ok",true}}));
 assert(ReadOwned(fd[0],pending,value)==OwnedRead::Closed);
 close(fd[0]);
 assert(socketpair(AF_UNIX,SOCK_STREAM,0,fd)==0);
 pending.assign(OWNED_MAX_FRAME+1,'x');
 assert(ReadOwned(fd[0],pending,value)==OwnedRead::Closed);
 close(fd[0]);close(fd[1]);
 assert(!OwnedWrite(-1,{{"ok",true}}));
}
'''
with tempfile.TemporaryDirectory(prefix='dolphin-wire-') as directory:
    path=Path(directory); (path/'test.cpp').write_text(code)
    subprocess.run(['c++','-std=c++20','-fno-exceptions','-pthread','-Wall','-Wextra','-Werror',
        '-I'+str(root/'adapters/dolphin'), '-isystem',str(root/'adapters/dolphin/work/dolphin-src/Externals/tinygltf/tinygltf'),
        str(path/'test.cpp'),'-o',str(path/'test')],check=True)
    subprocess.run([str(path/'test')],check=True)
print('Dolphin owned wire: exact identities, partial frames, bounded writes and EOF passed')
