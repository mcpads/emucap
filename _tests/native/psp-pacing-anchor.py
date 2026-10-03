#!/usr/bin/env python3
"""Check PPSSPP's actual display timing after a pause with a controlled clock."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', required=True, type=Path, help='patched sceDisplay.cpp')
args = parser.parse_args()
source = args.source.read_text()
def function(signature, source=source):
    start = source.index(signature)
    brace = source.index('{', start)
    depth, end = 1, brace + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

timing = (args.source.parents[1] / 'FrameTiming.cpp').read_text()
code = r'''
#include <algorithm>
#include <cassert>
#include <cmath>
#include <cstdio>
#include <cstdint>
#define _dbg_assert_(x) assert(x)
#define PROFILE_THIS_SCOPE(x) ((void)0)
static double now = 100, lastFrameTime, nextFrameTime, curFrameTime, waited;
static int coreState, interrupt = 0;
constexpr int CORE_RUNNING_CPU = 0;
static uint32_t generation;
uint32_t __DisplayEmucapPacingGeneration() { return generation; }
void sleep_precise(double seconds, const char *) { assert(seconds <= .01); now += seconds; waited += seconds; if(interrupt == 1) coreState = 1; if(interrupt == 2) ++generation; }
static bool wasPaused;
static int numSkippedFrames;
struct { int iFrameSkip = 0; bool bLogFrameDrops = false; } g_Config;
bool UseAutoFrameSkip() { return false; }
double time_now_d() { return now; }
void DoFrameDropLogging(float) {}
struct FrameTiming {
 double waitUntil_ = 0; double *curTimePtr_ = nullptr;
 void DeferWaitUntil(double, double *); void PostSubmit();
} g_frameTiming;
''' + function('void WaitUntil(', timing) + '\n' + function('void FrameTiming::DeferWaitUntil(', timing) + '\n' + function('void FrameTiming::PostSubmit()', timing) + '\n' + function('void __DisplaySetWasPaused()') + '\n' + function('static void DoFrameTiming(') + r'''
void equal(double a, double b) { assert(std::abs(a-b) < 1e-8); }
int main() {
 for (int rate : {1, 50, 100, 400, 1000})
 for (double park : {0.001, 0.050, 10.0})
 for (bool end : {false, true}) {
  float interval = float(100.0 / (60.0 * rate));
  now = 100; lastFrameTime = now; wasPaused = false;
  now += park; __DisplaySetWasPaused();
  double released = now; waited = 0; g_frameTiming = {}; bool skip = true;
  DoFrameTiming(true, &skip, interval, end);
  assert(!skip && !wasPaused); equal(lastFrameTime, released + interval);
  if (end) { equal(waited, 0); equal(g_frameTiming.waitUntil_, released + interval); g_frameTiming.PostSubmit(); equal(waited, interval); assert(!g_frameTiming.curTimePtr_ && g_frameTiming.waitUntil_ == 0); }
  else equal(waited, interval);
  // Ordinary active time must count toward the next interval.
  now += interval / 4.0; waited = 0; g_frameTiming = {};
  DoFrameTiming(true, &skip, interval, false); equal(waited, interval * .75);
 }
 // Unlimited without frame skipping neither waits nor schedules a deferred wait.
 wasPaused = true; waited = 0; g_frameTiming = {}; bool skip = true;
 DoFrameTiming(false, &skip, 1.0f / 60.0f, true);
 assert(!skip); equal(waited, 0); equal(g_frameTiming.waitUntil_, 0);
 // Both debugger stop and policy-generation changes interrupt within one slice.
 for (int reason : {1, 2}) {
  interrupt = reason; coreState = CORE_RUNNING_CPU; waited = 0;
  g_frameTiming.DeferWaitUntil(now + 2, &curFrameTime);
  g_frameTiming.PostSubmit(); equal(waited, .01);
  assert(!g_frameTiming.curTimePtr_ && g_frameTiming.waitUntil_ == 0);
  waited = 0; g_frameTiming.PostSubmit(); equal(waited, 0);
 }
 puts("PSP display timing: fresh interval, deferred wait, active-time accounting and unlimited passed");
}
'''
with tempfile.TemporaryDirectory() as temp:
    path = Path(temp)
    (path / 'probe.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++17', '-fsanitize=address,undefined', str(path / 'probe.cpp'), '-o', str(path / 'probe')], check=True)
    subprocess.run([str(path / 'probe')], check=True)
