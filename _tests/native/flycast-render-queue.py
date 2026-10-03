#!/usr/bin/env python3
"""Exercise the patched native queue with controlled renderer callbacks."""
from pathlib import Path
import argparse, subprocess, tempfile
root = Path(__file__).resolve().parents[2]
p = argparse.ArgumentParser()
p.add_argument('--source', type=Path, required=True)
p.add_argument('--rtt-source', type=Path, help='Also exercise actual OpenGL RTT callback with controlled GL readback')
p.add_argument('--sanitizer', default='address,undefined', choices=['address,undefined', 'thread'])
a = p.parse_args()
s = a.source.read_text()
s = s[s.index('class PvrMessageQueue'):s.index('\nstatic PvrMessageQueue pvrQueue;')]
# Keep native submission, dequeue, dispatch, cancellation and completion code.
# Replace only graphics backend work with a deterministic guest-memory writer.
for signature in ['void render()', 'void renderFramebuffer(const FramebufferInfo& config)', 'void present()']:
    start = s.index(signature)
    opening = s.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (s[end] == '{') - (s[end] == '}')
        end += 1
    s = s[:opening] + '{ writer(); }' + s[end:]
code = r'''
#include "emucap_render_fence.h"
#include <atomic>
#include <cassert>
#include <deque>
#include <future>
#include <functional>
#include <stdexcept>
#include <thread>
using namespace std::chrono_literals;
std::atomic<unsigned> signals{0};
class cResetEvent {
 std::mutex m; std::condition_variable cv; bool ready=false;
 public:
 void Set(){std::lock_guard<std::mutex> l(m);ready=true;++signals;cv.notify_all();}
 bool Wait(int timeout=-1){std::unique_lock<std::mutex> l(m);
  if(timeout<0)cv.wait(l,[&]{return ready;});
  else if(!cv.wait_for(l,std::chrono::milliseconds(timeout),[&]{return ready;}))return false;
  ready=false;return true;}
};
namespace config { bool ThreadedRendering=true; }
struct FramebufferInfo {};
#define FC_PROFILE_SCOPE
void setDefaultRoundingMode(){}
struct {void restoreHostRoundingMode(){}} Sh4cntx;
std::function<void()> writer=[]{ };
int failure_reports=0;
void emucap_renderer_failed(const char* reason) noexcept {assert(reason && *reason);++failure_reports;}
''' + s + r'''
using Result=EmucapRenderFence::Result;
void await_submission(unsigned before){
 auto deadline=std::chrono::steady_clock::now()+2s;
 while(signals.load()==before){assert(std::chrono::steady_clock::now()<deadline);std::this_thread::yield();}
}
int main(){
 // Already executing writer: the observer cannot see its half-written state.
 for(int disposition : {0,1,2}) {
  PvrMessageQueue q; int values[2]={0,0};
  std::promise<void> entered, release;auto gate=release.get_future();
  writer=[&]{values[0]=1;entered.set_value();gate.wait();values[1]=2;};
  q.enqueue(PvrMessageQueue::Render);
  auto consumer=std::async(std::launch::async,[&]{return q.waitAndExecute();});
  entered.get_future().wait();
  auto before=signals.load();
  auto observer=std::async(std::launch::async,[&]{return q.waitForWriters(2000);});
  await_submission(before);
  assert(observer.wait_for(10ms)==std::future_status::timeout);
  if(disposition==1)q.cancelEnqueue();
  if(disposition==2)q.reset();
  release.set_value();assert(consumer.get());
  if(disposition==0)assert(q.waitAndExecute(0));
  assert(observer.get()==(disposition==1?Result::failed:disposition==2?Result::invalidated:Result::complete));
  assert(values[0]==1 && values[1]==2);
 }
 // Queued callback must execute before the fence acknowledgment.
 {
  PvrMessageQueue q;int writes=0;writer=[&]{++writes;};
  q.enqueue(PvrMessageQueue::Render);auto before=signals.load();
  auto observer=std::async(std::launch::async,[&]{return q.waitForWriters(2000);});
  await_submission(before);assert(writes==0);
  assert(q.waitAndExecute(0));assert(writes==1);
  assert(observer.wait_for(10ms)==std::future_status::timeout);
  assert(q.waitAndExecute(0));assert(observer.get()==Result::complete);
 }
 // Reset discards queued work and invalidates its observer; a new fence works.
 {
  PvrMessageQueue q;auto before=signals.load();
  auto observer=std::async(std::launch::async,[&]{return q.waitForWriters(2000);});
  await_submission(before);q.reset();assert(observer.get()==Result::invalidated);
  before=signals.load();
  auto fresh=std::async(std::launch::async,[&]{return q.waitForWriters(2000);});
  await_submission(before);assert(q.waitAndExecute(0));assert(fresh.get()==Result::complete);
 }
 // A failed render or stop cannot be mistaken for successful completion.
 for(bool stop : {false,true}) {
  PvrMessageQueue q;writer=[]{throw std::runtime_error("renderer failed");};
  q.enqueue(stop?PvrMessageQueue::Stop:PvrMessageQueue::Render);
  auto before=signals.load();
  auto observer=std::async(std::launch::async,[&]{return q.waitForWriters(2000);});
  await_submission(before);bool threw=false;
  try{assert(!q.waitAndExecute(0));}catch(const std::runtime_error&){threw=true;}
  assert(threw==!stop);assert(failure_reports==1);assert(observer.get()==Result::failed);
  assert(q.waitForWriters(100)==Result::failed);
 }
 {
  PvrMessageQueue q;assert(q.waitForWriters(1)==Result::timed_out);
  assert(q.waitAndExecute(0)); // Late acknowledgment cannot recover health.
  assert(q.waitForWriters(100)==Result::failed);
 }
 {
  PvrMessageQueue q;auto before=signals.load();
  auto observer=std::async(std::launch::async,[&]{return q.waitForWriters(2000);});
  await_submission(before);q.failWriters();assert(observer.get()==Result::failed);
  q.replaceGeneration();assert(q.waitForWriters(100)==Result::failed);
 }
 {
  PvrMessageQueue q;writer=[]{throw 7;};q.enqueue(PvrMessageQueue::Render);
  bool threw=false;try{q.waitAndExecute(0);}catch(int){threw=true;}
  assert(threw && failure_reports==2 && q.waitForWriters(100)==Result::failed);
 }
 config::ThreadedRendering=false;
 {PvrMessageQueue q;assert(q.waitForWriters(100)==Result::complete);}
 // A mode switch with pending work cannot be certified inline.
 {PvrMessageQueue q;config::ThreadedRendering=true;q.enqueue(PvrMessageQueue::Render);
  config::ThreadedRendering=false;assert(q.waitForWriters(100)==Result::failed);}
}
'''
if a.rtt_source:
    rtt = a.rtt_source.read_text()
    start = rtt.index('void ReadRTTBuffer()')
    opening = rtt.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (rtt[end] == '{') - (rtt[end] == '}')
        end += 1
    fixture = (Path(__file__).with_name('flycast-rtt-fixture.h')).read_text()
    fixture = fixture.replace('// ACTUAL_RTT_CALLBACK', rtt[start:end])
    code = code.replace('int main(){', fixture + '\nint main(){\n check_rtt_writer();', 1)
with tempfile.TemporaryDirectory(prefix='flycast-queue-') as d:
    d = Path(d)
    (d/'check.cpp').write_text(code)
    subprocess.run(['clang++', '-std=c++17', '-O1', '-g', '-pthread', '-fsanitize='+a.sanitizer,
                    '-I'+str(root/'adapters/flycast'), str(d/'check.cpp'), '-o', str(d/'check')], check=True)
    subprocess.run([str(d/'check')], check=True, timeout=30)
print('PASS native renderer queue: in-flight/queued writer, reset, cancellation, exception, stop, timeout, inline completion')

if a.rtt_source:
    print('PASS actual ReadRTTBuffer: direct readback and conversion finish before queue fence')
