#!/usr/bin/env python3
"""Actual Flycast owner receive function with deterministic fragmented socket input."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
s=(root/'adapters/flycast/emucap.cpp').read_text()
function=s[s.index('bool receive_request_bytes()'):s.index('void serve_socket_once()')]
code=r'''
#include <algorithm>
#include <cassert>
#include <cstring>
#include <string>
#include <sys/types.h>
int g_fd=1;std::string g_rx,wire;size_t calls=0,offset=0;bool disconnected=false;
void emucap_disconnect(){disconnected=true;g_fd=-1;g_rx.clear();}
bool emucap_sock_wouldblock(){return true;}
bool emucap_sock_eintr(){return false;}
ssize_t recv(int,char* out,size_t count,int){
 ++calls;auto n=std::min(count,wire.size()-offset);
 if(!n)return -1;
 memcpy(out,wire.data()+offset,n);offset+=n;return n;
}
''' + function + r'''
void reset(std::string bytes){wire=std::move(bytes);offset=calls=0;g_rx.clear();g_fd=1;disconnected=false;}
int main(){
 const size_t cap=8*1024*1024;
 for(bool newline : {false,true}){
  reset(std::string(cap+1,'x')+(newline?"\n":""));
  while(!disconnected){receive_request_bytes();assert(g_rx.size()<=cap+1);}
  assert(offset==cap+1 && g_fd==-1);
 }
 reset(std::string(cap,'x')+"\n{}\n");
 while(g_rx.find('\n')==std::string::npos)assert(receive_request_bytes());
 assert(g_rx.size()==cap+1);auto before=calls;
 assert(receive_request_bytes() && calls==before); // Drain before another receive.
 g_rx.erase(0,cap+1);assert(receive_request_bytes());assert(g_rx=="{}\n");
 reset("{}\n{}\n");assert(receive_request_bytes());before=calls;
 g_rx.erase(0,3);assert(receive_request_bytes() && calls==before && g_rx=="{}\n");
 reset("");g_rx="{\"method\":";assert(receive_request_bytes());assert(!disconnected&&g_rx=="{\"method\":");
 reset("");g_rx=std::string(cap+1,'x')+"\n";assert(!receive_request_bytes()&&calls==0);
}
'''
with tempfile.TemporaryDirectory(prefix='flycast-rx-') as d:
 p=Path(d);(p/'test.cpp').write_text(code)
 subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(p/'test.cpp'),'-o',str(p/'test')],check=True)
 subprocess.run([str(p/'test')],check=True,timeout=30)
print('PASS Flycast bounded owner receive: exact limit, overflow, queued requests and fragmented input')
