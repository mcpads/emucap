#!/usr/bin/env python3
"""Execute native SNES/GBA skip predicates against controlled clocks and callback ownership."""
import argparse
from pathlib import Path
import subprocess


def expression(text, start):
    begin = text.index(start)
    return text[begin:text.index(';',begin)+1]


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--source',type=Path,default=Path('adapters/mesen2/work/mesen'))
    p.add_argument('--ppu-source',type=Path,help='Optional old PPU tree for the negative control')
    p.add_argument('--output',type=Path,required=True)
    args=p.parse_args();args.output.mkdir(parents=True,exist_ok=True)
    root=args.ppu_source or args.source
    snes=(root/'Core/SNES/SnesPpu.cpp').read_text()
    gba=(root/'Core/GBA/GbaPpu.cpp').read_text()
    debugger=(args.source/'Core/Debugger/Debugger.cpp').read_text()
    start=debugger.index('bool Debugger::HasNativeHaltService()')
    helper=debugger[start:debugger.index('\n}',start)+2]
    s=expression(snes,'bool preserveRendererObservation =')+'\n'+expression(snes,'_skipRender =\n\t\t\t\t!preserveRendererObservation')
    g=(expression(gba,'bool preserveRendererObservation =')+'\n' if 'bool preserveRendererObservation =' in gba else '')+expression(gba,'_skipRender =\n\t\t\t')
    cpp=r'''
#include <cassert>
#include <cstdio>
#include <initializer_list>
enum class EventType { SnesPpuBgChrFetch, SnesPpuCgramLookup, CodeBreakIdle, CodeBreakIdleSavestate };
struct Debugger { int service=0; bool deep=false; bool HasEventCallback(EventType e) {
 return (service==1 && e==EventType::CodeBreakIdle) || (service==2 && e==EventType::CodeBreakIdleSavestate) || (deep && e==EventType::SnesPpuCgramLookup);
} bool HasNativeHaltService(); };
HELPER
struct Config { bool DisableFrameSkipping=false; };
struct Settings { Config cfg; int speed=100; Config& GetSnesConfig(){return cfg;} int GetEmulationSpeed(){return speed;} };
struct Timer { int elapsed=1; int GetElapsedMS(){return elapsed;} };
struct Rewind { bool IsRewinding(){return false;} };
struct Renderer { bool IsRecording(){return false;} };
struct Emu { Rewind rw; Renderer renderer; Rewind* GetRewindManager(){return &rw;} Renderer* GetVideoRenderer(){return &renderer;} };
int main(){ Settings setting; Settings* settings=&setting; Settings* _settings=&setting; auto& cfg=setting.cfg; Emu emu; Emu* _emu=&emu; Debugger debug; Debugger* debugger=&debug; Debugger* _bgChrFetchDebugger=nullptr; Timer _frameSkipTimer; bool _interlacedFrame=false; int _frameCount=0; bool _skipRender=false;
 for(int service: {0,1,2}) for(int speed: {1,100,150,151,400,10000,0}) for(int elapsed: {1,10,15,20}) for(bool disabled: {false,true}) {
  debug.service=service; setting.speed=speed; _frameSkipTimer.elapsed=elapsed; cfg.DisableFrameSkipping=disabled;
  { SNES
    assert(_skipRender == (service==0 && !disabled && (speed==0 || speed>150) && elapsed<10)); }
  { GBA
    assert(_skipRender == (service==0 && !disabled && (speed==0 || speed>150) && elapsed<15)); }
 }
 //Unregistering the service returns control to the native policy; it is not a sticky config write.
 debug.service=0; debug.deep=true; setting.speed=0; cfg.DisableFrameSkipping=false; _frameSkipTimer.elapsed=1;
 { SNES
   assert(!_skipRender); }
 puts("Native render predicates preserve halt-service frames and retain unowned native policy");
}
'''.replace('HELPER',helper).replace('SNES\n',s+'\n').replace('GBA\n',g+'\n')
    path=args.output/'guard.cpp';path.write_text(cpp)
    binary=args.output/'guard'
    subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(path),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)


if __name__=='__main__':main()
