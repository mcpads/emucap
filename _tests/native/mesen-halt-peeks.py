#!/usr/bin/env python3
"""Actual NES halt catch-up precedes side-effect-free halted APU observations."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2];core=root/'adapters/mesen2/work/mesen/Core'
def function(path,signature):
    text=(core/path).read_text();start=text.index(signature);end=text.index('{',start)+1;depth=1
    while depth:
        depth+=(text[end]=='{')-(text[end]=='}');end+=1
    return text[start:end]
# Native script service is published after the main debugger's pre-break drain.
debugger=(core/'Debugger/Debugger.cpp').read_text();halt=debugger[debugger.index('_executionStopped = true;'):]
assert halt.index('GetMainDebugger()->OnBeforeBreak(sourceCpu)') < halt.index('EventType::CodeBreakIdle')
setup=r'''
#include <cstdint>
#include <cassert>
#include <cstdio>
enum class CpuType{Nes};enum class IRQSource{DMC};
struct NesApu;
struct Emulator{bool IsEmulationThread(){return true;}};
struct Cpu{bool HasIrqSource(IRQSource){return true;}};
struct Console{Emulator emulator;Cpu cpu;NesApu* apu;Emulator*GetEmulator(){return &emulator;}Cpu*GetCpu(){return &cpu;}NesApu*GetApu(){return apu;}};
struct Channel{unsigned runs=0,reloads=0;void ReloadLengthCounter(){reloads++;}void Run(int32_t){runs++;}bool GetStatus(){return true;}};
struct FrameCounter{bool irq=true;unsigned clears=0;int32_t Run(int32_t& remaining){auto consumed=remaining;remaining=0;return consumed;}bool PeekIrqFlag(){return irq;}bool GetIrqFlag(){clears++;irq=false;return true;}};
struct NesApu{
 Console*_console;FrameCounter*_frameCounter;Channel*_square1,*_square2,*_noise,*_triangle,*_dmc;
 int32_t _currentCycle=9,_previousCycle=2;
 void Run();uint8_t PeekRam(uint16_t);template<bool isPeek>uint8_t GetStatus();
};
struct NesDebugger{Console*_console;void OnBeforeBreak(CpuType);};
'''
code=setup+'\n'.join(function(path,sig) for path,sig in [
('NES/APU/NesApu.cpp','void NesApu::Run('),
('NES/APU/NesApu.cpp','template<bool isPeek>\nuint8_t NesApu::GetStatus('),
('NES/APU/NesApu.cpp','uint8_t NesApu::PeekRam('),
('NES/Debugger/NesDebugger.cpp','void NesDebugger::OnBeforeBreak(')])
checks=r'''
int main(){Console console;FrameCounter frame;Channel channel;
 NesApu apu{&console,&frame,&channel,&channel,&channel,&channel,&channel};console.apu=&apu;
 NesDebugger debugger{&console};debugger.OnBeforeBreak(CpuType::Nes);
 assert(apu._previousCycle==apu._currentCycle && channel.runs==5 && channel.reloads==4);
 // The native halt is now published; no scheduler work runs between observations.
 for(unsigned i=0;i<8;i++)assert(apu.PeekRam(0x4015)==0xdf);
 assert(apu._previousCycle==9 && apu._currentCycle==9 && channel.runs==5 && channel.reloads==4);
 assert(frame.irq && frame.clears==0);
 puts("Mesen actual NES halt/APU functions: catch-up precedes publication; repeated peek has zero writer commits and IRQ clears");}
'''
with tempfile.TemporaryDirectory(prefix='mesen-halt-peeks-') as directory:
 p=Path(directory)/'check.cpp';p.write_text(code+checks);b=Path(directory)/'check'
 subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(p),'-o',str(b)],check=True)
 subprocess.run([str(b)],check=True)
