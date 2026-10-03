#!/usr/bin/env python3
import argparse
from pathlib import Path
import subprocess
import tempfile
parser=argparse.ArgumentParser(description='Check Mesen native pacing reanchor with controlled time')
parser.add_argument('--source',type=Path,required=True)
a=parser.parse_args()
header=a.source.read_text()
code='''
#include <cstdio>
#include <cassert>
#include <cmath>
static double now=0, waited=0;
class Timer { double origin=0; public: double GetElapsedMS(){return now-origin;} void Reset(){origin=now;} void WaitUntil(double t){double d=t-GetElapsedMS();if(d>0){now+=d;waited+=d;}} };
'''+header.replace('#pragma once','').replace('#include "Utilities/Timer.h"','')+'''
int main(){
 for(int rate:{1,50,100,400,1000})for(double park:{1.,50.,10000.}){
  now=0;double delay=1000./60*100/rate;FrameLimiter limiter(delay);
  for(int i=0;i<3;++i){limiter.ProcessFrame();while(limiter.WaitForNextFrame()) {}}
  now+=park;limiter.ResumeFromPause();now+=delay/4;waited=0;limiter.ProcessFrame();while(limiter.WaitForNextFrame()) {}
  assert(std::abs(waited-delay*.75)<1e-7);
  printf("rate=%d park_ms=%.0f expected_ms=%.6f actual_ms=%.6f\\n",rate,park,delay*.75,waited);
  now+=delay/4;waited=0;limiter.ProcessFrame();while(limiter.WaitForNextFrame()) {}
  assert(std::abs(waited-delay*.75)<1e-7);
 }
}
'''
code='#include <initializer_list>\n'+code
with tempfile.TemporaryDirectory() as d:
 p=Path(d);(p/'probe.cpp').write_text(code)
 subprocess.run(['c++','-std=c++17','-fsanitize=address,undefined',str(p/'probe.cpp'),'-o',str(p/'probe')],check=True)
 subprocess.run([str(p/'probe')],check=True)
