#!/usr/bin/env python3
"""Check native N64 pause/limiter integration with controlled SDL host time."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True, help='patched core src directory')
a = parser.parse_args()
main = (a.source / 'main/main.c').read_text()
debug = (a.source / 'debugger/dbg_debugger.c').read_text()
api = (a.source / 'api/debugger.c').read_text()
def body(text, signature):
    start = text.index(signature)
    i = text.index('{', start) + 1
    depth = 1
    while depth:
        depth += (text[i] == '{') - (text[i] == '}')
        i += 1
    return text[start:i]
reset = ''
if 'void main_reset_speed_limiter(' in main:
    reset = 'static int l_SpeedLimiterReset = 0;\nstatic unsigned int l_SpeedLimiterResumeTime = 0;\n' + body(main, 'void main_reset_speed_limiter(')
header = (a.source / 'api/m64p_emucap_pacing.h').read_text()
preamble = header + r'''
#include <stdint.h>
#include <stdio.h>
#include <assert.h>
#define DBG 1
#define M64P_DBG_RUNSTATE_PAUSED 0
#define M64P_DBG_RUNSTATE_RUNNING 2
#define DEBUG_UI_UPDATE 0
#define DEBUG_UI_VI 1
#define M64P_BKP_FLAG_EXEC 1
#define M64P_BKP_FLAG_LOG 2
#define BPT_CHECK_FLAG(a,b) 0
static int g_DebuggerActive=1,g_dbg_runstate=2,g_rom_pause=0;
static int l_MainSpeedLimit=1,l_SpeedFactor=100;
static uint64_t policy_revision=1;
static int change_during_wait;
static m64p_emucap_policy main_pacing_read(void) {
 m64p_emucap_policy p={0}; p.factor=l_SpeedFactor; p.limiter=l_MainSpeedLimit; p.revision=policy_revision; return p;
}
static struct {struct {double expected_refresh_rate;} vi;} g_dev={{60}};
static unsigned now=100,waited=0,park_ms=0,previousPC,breakpointAccessed,breakpointFlag;
static int sem_pending_steps,g_Breakpoints[1];
unsigned SDL_GetTicks(void){return now;}
void SDL_Delay(unsigned ms){now+=ms?ms:1;waited+=ms?ms:1; if(change_during_wait){++policy_revision;change_during_wait=0;}}
void SDL_SemWait(int ignored){now+=park_ms;g_dbg_runstate=M64P_DBG_RUNSTATE_RUNNING;}
void DebuggerCallback(int kind,unsigned pc){}
int check_breakpoints(unsigned pc){return -1;}
void log_breakpoint(unsigned pc,int flag,int value){}
void osd_render(void){}
void VidExt_GL_SwapBuffers(void){}
void main_check_inputs(void){g_rom_pause=0;}
'''
hook = body(api, 'EXPORT void CALL DebugFrameResume(').replace('EXPORT void CALL', 'void')
code = preamble + reset + '\n' + hook + '\n' + body(main,'static int emucap_limiter_wait_interrupted(') + '\n' + body(main,'static void apply_speed_limiter(') + '\n' + body(main,'static void pause_loop(') + '\n' + body(debug,'void update_debugger(') + r'''
int main(void){
 int failures=0;
 for(int rate=0;rate<4;rate++)for(int kind=0;kind<3;kind++)for(int park=0;park<3;park++){
  int rates[]={1,100,400,1000};unsigned parks[]={1,30,10000};
  l_SpeedFactor=rates[rate];park_ms=parks[park];
  for(int i=0;i<10;i++)apply_speed_limiter();
  if(kind==0){g_dbg_runstate=M64P_DBG_RUNSTATE_PAUSED;update_debugger(123);}
  else if(kind==1) {g_rom_pause=1;now+=park_ms;pause_loop();}
  else {now+=park_ms;DebugFrameResume();}
  double expected=1000.0/60*100/l_SpeedFactor;
  now+=1; // Active CPU time after release counts toward the first interval.
  waited=0;apply_speed_limiter();
  assert(waited >= expected-2 && waited <= expected);
  waited=0;apply_speed_limiter();
  if(waited < expected-1 || waited > expected+1){
   fprintf(stderr,"rate=%d kind=%d park=%u wait=%u expected=%f\n",l_SpeedFactor,kind,park_ms,waited,expected);failures++;
  }
  assert(l_MainSpeedLimit==1 && l_SpeedFactor==rates[rate]);
  // No stop occurred: neither observer call may discard active elapsed time.
  now+=1;update_debugger(124);pause_loop();waited=0;apply_speed_limiter();
  assert(waited >= expected-2 && waited <= expected);
  // A slow running host also resets without converting a negative VI count to unsigned.
  now+=10000;apply_speed_limiter();apply_speed_limiter();
  waited=0;apply_speed_limiter();assert(waited >= expected-1 && waited <= expected+1);
 }
 // An intervening policy change ending at the same tuple ends the old wait.
 l_SpeedFactor=1; ++policy_revision; main_reset_speed_limiter();
 waited=0; change_during_wait=1; apply_speed_limiter(); assert(waited<=10);
 if(failures)return 1;
 puts("N64 debugger/core pause reanchor and no-park deadline preservation passed");
}
'''
with tempfile.TemporaryDirectory(prefix='n64-anchor-') as d:
    source = Path(d)/'test.c'
    source.write_text(code)
    subprocess.run(['cc','-std=c99','-fsanitize=address,undefined','-fno-sanitize-recover=all',str(source),'-o',d+'/test'],check=True)
    subprocess.run([d+'/test'],check=True)
