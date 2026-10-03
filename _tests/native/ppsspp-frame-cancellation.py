#!/usr/bin/env python3
"""Exercise applied PSP deferred-stop code with controlled native stop evidence.

Real WebSocket dispatch/CPU scheduling remains a separate runtime qualification.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    args = parser.parse_args()
    source = args.source.read_text()
    stop = source[source.index('void WebSocketSteppingState::RequestFrameStop('):source.index('void WebSocketSteppingState::CancelFrame(')]
    terminal = source[source.index('void WebSocketSteppingState::Broadcast('):source.index('// Read or set agent pacing')]
    support = r'''
#include <cassert>
#include <chrono>
#include <cstdlib>
#include <map>
#include <string>
#include <thread>
#include <sys/wait.h>
#include <unistd.h>
using u32 = unsigned int;
enum class EmucapFrameStepStatus { Running, TimedOut, Interrupted, Completed };
struct EmucapFrameStepResult {
 EmucapFrameStepStatus status = EmucapFrameStepStatus::TimedOut;
 unsigned requested = 120, completed = 3, startVblank = 10, endVblank = 13;
} native;
enum class BreakReason { DebugBreak };
enum { CORE_RUNNING_CPU, CORE_STEPPING_CPU, CORE_POWERDOWN };
int coreState = CORE_RUNNING_CPU, breaks = 0, cancels = 0;
bool inactive = false;
bool Core_EmucapMemoryParked() { return inactive; }
auto Core_EmucapWaitFrameStep(unsigned ms) { assert(ms == 0); return native; }
void Core_Break(BreakReason, int) { ++breaks; }
auto Core_EmucapCancelFrameStep() { ++cancels; native.status=EmucapFrameStepStatus::Interrupted; return native; }
struct JsonWriter {
 static std::map<std::string,std::string> last;
 void begin() { last.clear(); }
 void end() {}
 void writeString(std::string k, std::string v) { last[k]=v; }
 void writeRaw(std::string k, std::string v) { last[k]=v; }
 void writeUint(std::string k, unsigned v) { last[k]=std::to_string(v); }
 std::string str() { return "terminal"; }
};
std::map<std::string,std::string> JsonWriter::last;
namespace net { struct WebSocketServer { int sent=0; void Send(std::string) { ++sent; } }; }
struct WebSocketSteppingState {
 std::string frameTicket_="ticket", frameOperation_="operation", frameStopReason_;
 std::chrono::steady_clock::time_point frameDeadline_=std::chrono::steady_clock::now()+std::chrono::seconds(240), frameStopDeadline_;
 bool frameStopping_=false;
 void RequestFrameStop(const char *);
 void Broadcast(net::WebSocketServer *);
 void CloseFrame();
};
WebSocketSteppingState *emucapFrameOwner=nullptr;
'''
    tests = r'''
int main() {
 net::WebSocketServer ws;
 WebSocketSteppingState owner, other;
 emucapFrameOwner=&owner;
 other.Broadcast(&ws); other.CloseFrame();
 assert(ws.sent==0 && breaks==0 && emucapFrameOwner==&owner);
 owner.RequestFrameStop("cancelled"); owner.RequestFrameStop("connection_lost");
 assert(breaks==1 && owner.frameStopReason_=="cancelled");
 owner.Broadcast(&ws);
 assert(ws.sent==0 && emucapFrameOwner==&owner); // Intent alone is not a frozen reply.
 coreState=CORE_STEPPING_CPU; inactive=true;
 owner.Broadcast(&ws);
 assert(ws.sent==1 && !emucapFrameOwner && cancels==1);
 assert(JsonWriter::last["completed"]=="3" && JsonWriter::last["state"]=="frozen");
 assert(JsonWriter::last["operation_id"]=="operation" && JsonWriter::last["ticket"]=="ticket");
 WebSocketSteppingState completed;
 emucapFrameOwner=&completed; native.status=EmucapFrameStepStatus::Completed;
 completed.RequestFrameStop("cancelled"); completed.Broadcast(&ws);
 assert(breaks==1 && JsonWriter::last["status"]=="completed" && !JsonWriter::last.count("reason"));
 WebSocketSteppingState breakpoint;
 emucapFrameOwner=&breakpoint; native.status=EmucapFrameStepStatus::Interrupted;
 breakpoint.RequestFrameStop("cancelled"); breakpoint.Broadcast(&ws);
 assert(breaks==1 && JsonWriter::last["status"]=="interrupted" && !JsonWriter::last.count("reason"));
 WebSocketSteppingState eof;
 emucapFrameOwner=&eof; eof.CloseFrame();
 assert(!emucapFrameOwner && breaks==1);
 // A stale native paused flag with unprocessed execution cannot certify cleanup.
 for (int path=0;path<2;++path) {
  pid_t child=fork(); assert(child>=0);
  if (!child) {
   WebSocketSteppingState stuck; emucapFrameOwner=&stuck;
   inactive=false; coreState=CORE_STEPPING_CPU;
   stuck.frameStopping_=true; stuck.frameStopDeadline_=std::chrono::steady_clock::now()-std::chrono::seconds(1);
   if (path) stuck.CloseFrame(); else stuck.Broadcast(&ws);
   _exit(7);
  }
  int status=0; assert(waitpid(child,&status,0)==child);
  assert(WIFSIGNALED(status) && WTERMSIG(status)==SIGABRT);
 }
}
'''
    with tempfile.TemporaryDirectory(prefix='psp-frame-cancel-') as temp:
        cpp = Path(temp)/'test.cpp'; exe=Path(temp)/'test'
        cpp.write_text(support+stop+terminal+tests)
        subprocess.run(['c++','-std=c++17','-pthread','-fsanitize=address,undefined',str(cpp),'-o',str(exe)],check=True)
        subprocess.run([str(exe)],check=True)
    print('PSP deferred stop: intent, partial progress, completed/breakpoint race, foreign owner, EOF, and two unverified-stop paths passed')


if __name__ == '__main__':
    main()
