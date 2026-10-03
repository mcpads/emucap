#!/usr/bin/env python3
"""Exercise the maintained PCSX2 native operation owner with a controlled CPU host."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, required=True, help='patched EmuCapFrameOperation.h')
a = p.parse_args()
code = r'''
#include "EmuCapFrameOperation.h"
#include <cassert>
#include <condition_variable>
#include <cstdio>
#include <functional>
#include <thread>
using namespace EmuCap;
using Clock = FrameOperations::Clock;
struct Host {
 bool valid=true, running=false, active=false;
 uint64_t frame=17, epoch=1;
 uint32_t remaining=0;
 unsigned starts=0, pauses=0;
 std::function<void()> on_pause, on_start, on_valid;
 Clock::time_point clock;
 Clock::time_point Now(){return clock;}
 bool Valid(){if(on_valid)on_valid();return valid;} bool Paused(){return !running;}
 bool Running(){return running;} bool Parked(){return !running && !active;}
 uint64_t Frame(){return frame;} uint64_t Epoch(){return epoch;}
 uint32_t Remaining(){return remaining;} void ClearTarget(){remaining=0;}
 void Start(uint32_t n){++starts;remaining=n;running=true;++epoch;if(on_start)on_start();}
 void Pause(){++pauses;running=false;++epoch;if(on_pause)on_pause();}
 void Tick(){assert(running && remaining);++frame;if(!--remaining)Pause();}
};
void service(FrameOperations& owner, Host& host, Clock::time_point now, const FrameOperations::Ticket& ticket={}){host.clock=now;owner.Service(host,ticket);}
FrameReceipt read(FrameOperations& ops, const FrameOperations::Ticket& t){
 FrameReceipt r;assert(ops.Read(t,r));return r;
}
int main(){
 const auto now=Clock::now();
 // Native admission exposes the broker's full frame range, with no hidden chunk cap.
 {
  FrameOperations owner;Host h;
  assert(!owner.Begin(0,100,now) && !owner.Begin(5001,100,now));
  assert(!owner.Begin(2,0,now) && !owner.Begin(2,250001,now));
  auto a=owner.Begin(5000,250000,now);assert(a);service(owner,h,now,a);
  for(unsigned i=0;i<5000;++i)h.Tick();service(owner,h,now);
  auto r=read(owner,a);assert(r.phase==FramePhase::Terminal && r.count==5000 && r.reason==FrameReason::Completed);
 }
 // Cleanup can prove a stop after input-only work without executing a frame.
 {
  FrameOperations owner;Host h;h.running=true;h.active=true;h.remaining=9;
  auto a=owner.BeginPause(100,now);assert(a);service(owner,h,now,a);
  assert(h.pauses==1 && h.starts==0 && read(owner,a).phase==FramePhase::Queued);
  h.active=false;service(owner,h,now);
  auto r=read(owner,a);assert(r.phase==FramePhase::Terminal && r.requested==0 && r.count==0 && r.start==r.end);
  assert(h.remaining==0 && h.starts==0);assert(owner.Finish(a));service(owner,h,now,a);
  assert(read(owner,a).phase==FramePhase::Finished);
 }
 // An expired queued pause callback has no late native effect.
 {
  FrameOperations owner;Host h;h.running=true;auto a=owner.BeginPause(1,now);
  service(owner,h,now+std::chrono::milliseconds(2),a);
  assert(h.running && h.pauses==0 && read(owner,a).phase==FramePhase::Fault);
 }
 // Pre-entry cancellation and expired queued callbacks never start execution.
 for(bool cancel:{false,true}){
  FrameOperations ops;Host host;auto t=ops.Begin(120,100,now);assert(t);
  if(cancel)assert(ops.Cancel(t));
  service(ops,host,now+std::chrono::milliseconds(101),t);
  auto r=read(ops,t);assert(r.phase==FramePhase::Terminal && r.count==0 && host.starts==0);
  assert(r.reason==(cancel?FrameReason::Cancelled:FrameReason::Deadline));
 }
 // Deadline admission samples fresh time after native preconditions, not an old dispatch timestamp.
 {
  FrameOperations owner;Host h;auto a=owner.Begin(12,100,now);
  h.on_valid=[&]{h.clock=now+std::chrono::milliseconds(101);};
  service(owner,h,now,a);auto r=read(owner,a);
  assert(h.starts==0 && r.phase==FramePhase::Terminal && r.reason==FrameReason::Deadline);
 }
 // Actual frame progress is authoritative, and a paused CPU stack is pending.
 FrameOperations ops;Host host;auto t=ops.Begin(120,250000,now);service(ops,host,now,t);
 assert(!ops.Finish(t));assert(!ops.Begin(1,100,now));assert(!ops.Find(t->receipt.id+1));
 host.active=true;host.Tick();host.Tick();assert(ops.Cancel(t));service(ops,host,now,t);
 assert(!host.running && host.pauses==1);
 auto r=read(ops,t);assert(r.phase==FramePhase::Stopping && r.count==2);
 host.active=false;service(ops,host,now);r=read(ops,t);
 assert(r.phase==FramePhase::Terminal && r.reason==FrameReason::Cancelled && r.end-r.start==2);
 assert(ops.Finish(t));assert(read(ops,t).phase==FramePhase::Finishing);
 service(ops,host,now,t);assert(read(ops,t).phase==FramePhase::Finished);
 auto next=ops.Begin(2,250000,now);assert(next && next->receipt.id!=t->receipt.id);
 assert(!ops.Cancel(t) && !ops.Finish(t));
 service(ops,host,now,t);assert(host.starts==1); // old queued callback has no effect
 service(ops,host,now,next);assert(host.starts==2);
 host.Tick();host.Tick();ops.Cancel(next);service(ops,host,now,next);
 r=read(ops,next);assert(r.phase==FramePhase::Terminal && r.reason==FrameReason::Completed && r.count==2);
 // Finish revalidates the native stop epoch, even with unchanged frame count.
 host.epoch++;assert(ops.Finish(next));service(ops,host,now,next);
 assert(read(ops,next).phase==FramePhase::Fault && ops.OwnsExecution());
 // Breakpoint/external pause retains exact partial progress.
 {
  FrameOperations owner;Host h;auto a=owner.Begin(12,100,now);service(owner,h,now,a);
  h.Tick();h.Pause();service(owner,h,now);
  auto r=read(owner,a);assert(r.reason==FrameReason::ExternalStop && r.count==1);
 }
 // Native scheduler service, without polling, applies the operation deadline.
 {
  FrameOperations owner;Host h;auto a=owner.Begin(12,1,now);service(owner,h,now,a);
  service(owner,h,now+std::chrono::milliseconds(2));
  auto r=read(owner,a);assert(r.phase==FramePhase::Terminal && r.reason==FrameReason::Deadline && !h.running);
 }
 // VM replacement invalidates queued and running operations before further effects.
 for(bool started:{false,true}){
  FrameOperations owner;Host h;auto a=owner.Begin(12,100,now);
  if(started)service(owner,h,now,a);
  owner.Invalidate();service(owner,h,now,a);
  assert(read(owner,a).phase==FramePhase::Fault && h.starts==unsigned(started) && h.pauses==0);
 }
 // Native progress divergence cannot be represented as a successful terminal.
 {
  FrameOperations owner;Host h;auto a=owner.Begin(12,100,now);service(owner,h,now,a);
  h.frame++;service(owner,h,now);assert(read(owner,a).phase==FramePhase::Fault);
 }
 // A blocking native pause cannot hold the receipt/cancellation mutex.
 {
  FrameOperations owner;Host h;auto a=owner.Begin(12,100,now);service(owner,h,now,a);
  std::mutex mutex;std::condition_variable cv;bool entered=false, release=false;
  h.on_pause=[&]{std::unique_lock lock(mutex);entered=true;cv.notify_all();cv.wait(lock,[&]{return release;});};
  owner.Cancel(a);
  std::thread cpu([&]{service(owner,h,now,a);});
  {std::unique_lock lock(mutex);cv.wait(lock,[&]{return entered;});}
  assert(read(owner,a).phase==FramePhase::Running);assert(owner.Cancel(a));assert(!owner.Finish(a));
  {std::lock_guard lock(mutex);release=true;}cv.notify_all();cpu.join();
  assert(read(owner,a).phase==FramePhase::Terminal);
 }
 // Reentrant lifecycle invalidation inside a host callback wins publication.
 {
  FrameOperations owner;Host h;auto a=owner.Begin(12,100,now);
  h.on_start=[&]{owner.Invalidate();service(owner,h,now,a);};
  service(owner,h,now,a);assert(read(owner,a).phase==FramePhase::Fault);
 }
 // Disconnect fences queued work and retains the stop until it is verified.
 {
  FrameOperations owner;Host h;auto a=owner.Begin(12,100,now);owner.Detach();
  service(owner,h,now,a);FrameReceipt r;assert(!owner.Read(a,r));assert(!h.starts && !owner.OwnsExecution());
  auto b=owner.Begin(2,100,now);service(owner,h,now,a);assert(!h.starts);
  service(owner,h,now,b);assert(h.starts==1);
 }
 // An active detached stack remains owned until the first actual unwind tick.
 {
  FrameOperations owner;Host h;auto a=owner.Begin(12,100,now);service(owner,h,now,a);
  h.active=true;owner.Detach();service(owner,h,now,a);
  assert(read(owner,a).phase==FramePhase::Stopping && owner.OwnsExecution());
  h.active=false;service(owner,h,now);FrameReceipt r;
  assert(!owner.Read(a,r) && !owner.OwnsExecution());
 }
 puts("PS2 native owned frames: identity, deadline, cancellation, unwind, races and terminal revalidation passed");
}
'''
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    (root/'EmuCapFrameOperation.h').write_bytes(a.source.read_bytes())
    (root/'probe.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++17', '-fno-exceptions', '-pthread', '-fsanitize=address,undefined',
                    str(root/'probe.cpp'), '-o', str(root/'probe')], check=True)
    subprocess.run([str(root/'probe')], check=True, timeout=15)
