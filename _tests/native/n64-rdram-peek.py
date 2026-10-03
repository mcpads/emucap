#!/usr/bin/env python3
"""Actual native RDRAM getter, watchpoint counterexample and no-effect regression."""
from pathlib import Path
import subprocess, tempfile, os
root=Path(__file__).resolve().parents[2]; core=root/'adapters/mupen64plus/work/mupen64plus-bundle-src-2.6.0/source/mupen64plus-core/src'
def fn(path,signature):
 s=(core/path).read_text(); start=s.index(signature); opening=s.index('{',start); depth=1; end=opening+1
 while depth:
  depth+=(s[end]=='{')-(s[end]=='}'); end+=1
 return s[start:end]
s='''#include <stdint.h>
#include <assert.h>
#include <stdio.h>
#define EXPORT
#define CALL
typedef int m64p_error;
#define M64ERR_INVALID_STATE 1
#define M64ERR_INPUT_INVALID 2
#define M64ERR_SUCCESS 0
#define M64P_DBG_RUNSTATE_RUNNING 2
#define M64P_DBG_RUNSTATE_PAUSED 0
#define M64P_BKP_FLAG_ENABLED 1
#define M64P_BKP_FLAG_READ 4
#define M64P_BKP_FLAG_LOG 8
#define BPT_CHECK_FLAG(x,y) 0
#define M64P_MEM_INVALID 0xffffffff
#define BP_CHECK_READ 1
struct handler { void* opaque; void (*read32)(void*,uint32_t,uint32_t*); };
struct memory { struct handler handlers[1],saved_handlers[1]; unsigned char bp_checks[1]; };
struct r4300_core { struct memory* mem; uint32_t pc; };
struct rdram { uint32_t* dram; uint32_t dram_size; };
struct device { struct r4300_core r4300; struct rdram rdram; };
static struct device g_dev;
static int g_dbg_runstate, breakpointAccessed, breakpointFlag, g_Breakpoints[1], updates;
static int lookup_breakpoint(uint32_t a,uint32_t n,uint32_t f) { return a==0 ? 0:-1; }
static void update_debugger(uint32_t pc) { updates++; }
static void log_breakpoint(uint32_t p,uint32_t f,uint32_t a) {}
static uint32_t* r4300_pc(struct r4300_core* c) { return &c->pc; }
static void mem_read32(const struct handler* h,uint32_t a,uint32_t* v) {h->read32(h->opaque,a,v);}
static struct handler* mem_get_handler(struct memory* m,uint32_t a) {assert(a==0);return m->handlers;}
static uint32_t virtual_to_physical_address(struct r4300_core* c,uint32_t a,int w){ assert(0);return 0;}
static void ram_read(void* p,uint32_t a,uint32_t* v){*v=0x12345678;}
'''
for p,sig in [('debugger/dbg_breakpoints.c','int check_breakpoints_on_mem_access('),('device/memory/memory.c','void read_with_bp_checks('),('device/r4300/r4300_core.c','int r4300_read_aligned_word('),('debugger/dbg_memory.c','uint32_t read_memory_32('),('debugger/dbg_memory.c','uint8_t read_memory_8('),('api/debugger.c','EXPORT m64p_error CALL DebugMemReadRdram(')]: s+=fn(p,sig)+'\n'
s+=r'''int main(){struct memory mem={0};struct device dev={{&mem,0x80001000},{0}};
mem.handlers[0]=(struct handler){&dev.r4300,read_with_bp_checks}; mem.saved_handlers[0]=(struct handler){0,ram_read};mem.bp_checks[0]=BP_CHECK_READ;
g_dbg_runstate=0; assert(read_memory_8(&dev,0x80000000)==0x12);assert(updates==0);
g_dbg_runstate=2; assert(read_memory_8(&dev,0x80000000)==0x12);assert(updates==1);assert(g_dbg_runstate==0);
/* Direct big-endian byte oracle, independent of old getter and host byte order. */
uint32_t words[]={0x12345678,0x9abcdef0}; const uint8_t expected[]={0x34,0x56,0x78,0x9a,0xbc,0xde};
g_dev.rdram=(struct rdram){words,8};
for(int running=0;running<=2;running+=2){
 g_dbg_runstate=running; updates=0; breakpointFlag=0x55; breakpointAccessed=0x1234;
 uint8_t out[8]={0xa5,0,0,0,0,0,0,0x5a};
 assert(DebugMemReadRdram(1,out+1,6)==M64ERR_SUCCESS);
 for(int i=0;i<6;i++)assert(out[i+1]==expected[i]);
 assert(out[0]==0xa5 && out[7]==0x5a);
 assert(g_dbg_runstate==running && updates==0 && breakpointFlag==0x55 && breakpointAccessed==0x1234);
 uint8_t tail=0;assert(DebugMemReadRdram(7,&tail,1)==0 && tail==0xf0);
 assert(DebugMemReadRdram(8,0,0)==0);
 uint8_t sentinel=0x77;
 assert(DebugMemReadRdram(7,&sentinel,2)==M64ERR_INPUT_INVALID && sentinel==0x77);
 assert(DebugMemReadRdram(UINT32_MAX,&sentinel,2)==M64ERR_INPUT_INVALID && sentinel==0x77);
 assert(DebugMemReadRdram(9,&sentinel,0)==M64ERR_INPUT_INVALID && sentinel==0x77);
 assert(DebugMemReadRdram(0,0,1)==M64ERR_INPUT_INVALID);
 g_dev.rdram.dram=0; assert(DebugMemReadRdram(0,&sentinel,1)==M64ERR_INVALID_STATE && sentinel==0x77);g_dev.rdram.dram=words;
}
puts("N64 actual native getter: prior watchpoint side effect reproduced; pure storage copy preserves state/latches, byte order, guards and invalid zero-write passed");}
'''
with tempfile.TemporaryDirectory(prefix='n64-rdram-peek-') as directory:
 p=Path(directory)/'check.c';p.write_text(s);binary=Path(directory)/'check'
 subprocess.run(['clang','-fsanitize='+os.environ.get('EMUCAP_SANITIZERS','address,undefined'),'-g',str(p),'-o',str(binary)],check=True)
 subprocess.run([str(binary)],check=True)
