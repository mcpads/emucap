#!/usr/bin/env python3
"""Exercise the actual HuC6280 replacement branch before guest continuation."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('adapters/mednafen/work/mednafen'))
args = parser.parse_args()
source = (args.source / 'src/pce/huc6280.cpp').read_text()
start = source.index('          if(CPUHook(PC))')
end = source.index('\n         }\n\n\t if(IRQSample', start)
branch = source[start:end]
code = r'''
#include <cassert>
#include <vector>
static unsigned PC, old_PC;
static int runrunrun;
static bool in_block_move, repeated, dispatched_block, executed_opcode;
static bool changed, restart_completed;
static unsigned hooks;
static std::vector<bool> readiness;
bool CPUHook(unsigned) { ++hooks; const bool result=changed; changed=false; return result; }
bool PCE_ResumeCompletedFrame() { return restart_completed; }
bool emucap_native_restore_service(bool ready) {
 assert(old_PC==PC); // Abandoned debugger PC must be reconciled before ack.
 readiness.push_back(ready);
 if(repeated) {
  repeated=false; PC=0x3456; in_block_move=false; runrunrun=1; return true;
 }
 return false;
}
void enter() {
 for(;;) {
''' + branch + r'''
 executed_opcode=true;
 return;
 }
 IBM_Dispatch:
 dispatched_block=true;
}
void clear(int running, bool block) {
 PC=0x2345; old_PC=0x1234; runrunrun=running; in_block_move=block;
 repeated=dispatched_block=executed_opcode=restart_completed=false; changed=true; hooks=0; readiness.clear();
}
int main() {
 clear(1,false); enter();
 assert((readiness==std::vector<bool>{true}) && executed_opcode && !dispatched_block);
 clear(1,true); enter();
 assert((readiness==std::vector<bool>{false}) && !executed_opcode && dispatched_block);
 clear(0,false); enter();
 assert((readiness==std::vector<bool>{false}) && !executed_opcode && !dispatched_block);
 clear(0,true); enter();
 assert((readiness==std::vector<bool>{false}) && !executed_opcode && !dispatched_block);
 clear(0,false); restart_completed=true; enter();
 assert((readiness==std::vector<bool>{false}) && executed_opcode && hooks==2);
 clear(0,true); restart_completed=true; enter();
 assert((readiness==std::vector<bool>{false}) && dispatched_block && hooks==1);
 clear(1,true); repeated=true; enter();
 assert((readiness==std::vector<bool>{false,true}) && old_PC==0x3456 && executed_opcode);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-pce-restore-') as temp:
    cpp = Path(temp) / 'test.cpp'; binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Native HuC6280 replacement: instruction, block transfer, exhausted frame, '
      'and repeated replacement pass ASan/UBSan')

# Exercise the production frame restart with only output/device seams replaced.
source = (args.source / 'src/pce/pce.cpp').read_text()
start = source.index('static EmulateSpecStruct *es;')
end = source.index('static void Emulate(EmulateSpecStruct *espec)', start)
restart = source[start:end]
code = r'''
#include <cassert>
struct Surface { int format=0; };
struct EmulateSpecStruct {
 int MasterCycles=9, SoundBufSize=8;
 int SoundBufSize_InternalProcessed=7, SoundBufSize_DriverProcessed=6;
 int MasterCycles_InternalProcessed=5, MasterCycles_DriverProcessed=4;
 bool VideoFormatChanged=true, SoundFormatChanged=true;
 Surface* surface; int CustomPalette=0, CustomPaletteNumEntries=0;
 int SoundRate=48000, DisplayRect=0, LineWidths=0, skip=0;
};
static int formats, rates, starts, inputs, cheats;
static bool IsHES=false;
struct VCE {
 bool completed=false;
 bool FrameCompleted() { return completed; }
 void SetPixelFormat(int,int,int) { ++formats; }
 void StartFrame(Surface*,int*,int,int) { ++starts; completed=false; }
} device;
static VCE* vce=&device;
void SetSoundRate(int) { ++rates; }
void INPUT_Frame() { ++inputs; }
void MDFNMP_ApplyPeriodicCheats() { ++cheats; }
''' + restart + r'''
int main() {
 Surface surface; EmulateSpecStruct spec; spec.surface=&surface;
 BeginFrame(&spec);
 assert(formats==1 && rates==1 && starts==1 && cheats==1);
 restored_clock=true;
 assert(!PCE_ResumeCompletedFrame());
 assert(restored_clock && spec.SoundBufSize_InternalProcessed==7 && starts==1 && inputs==0);
 device.completed=true;
 assert(PCE_ResumeCompletedFrame());
 assert(!restored_clock && !device.completed && starts==2 && inputs==1 && cheats==2);
 assert(spec.MasterCycles==0 && spec.SoundBufSize==0);
 assert(spec.SoundBufSize_InternalProcessed==0 && spec.SoundBufSize_DriverProcessed==0);
 assert(spec.MasterCycles_InternalProcessed==0 && spec.MasterCycles_DriverProcessed==0);
 assert(formats==1 && rates==1); // Already consumed by the suspended native frame.
 assert(spec.VideoFormatChanged && spec.SoundFormatChanged); // Preserve caller metadata.
 assert(!PCE_ResumeCompletedFrame() && starts==2 && inputs==1);
}
'''
with tempfile.TemporaryDirectory(prefix='mednafen-pce-frame-restart-') as temp:
    cpp = Path(temp) / 'test.cpp'; binary = Path(temp) / 'test'
    cpp.write_text(code)
    subprocess.run(['clang++', '-std=c++11', '-O1', '-fsanitize=address,undefined',
                    str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Native PCE frame restart: completed/partial frame, output accounting, '
      'and single format consumption pass ASan/UBSan')
