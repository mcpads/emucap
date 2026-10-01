#!/usr/bin/env python3
"""Exercise the native batch handler with a GPU surface callback and coherent backing RAM."""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=ROOT / 'adapters/xemu/work/xemu/ui/xemu-emucap.c')
args = parser.parse_args()
source = args.source.read_text()
start = source.index('XemuEmucapMemoryBatch *qmp_xemu_emucap_read_memory_batch(')
end = source.index('\nvoid qmp_xemu_emucap_set_input', start)
preamble = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>
typedef int Error;
typedef struct { uint64_t address; uint32_t length; } XemuEmucapMemoryRange;
typedef struct RangeList { XemuEmucapMemoryRange *value; struct RangeList *next; } XemuEmucapMemoryRangeList;
typedef struct strList { char *value; struct strList *next; } strList;
typedef struct { strList *reads; uint64_t frame_boundary; int64_t virtual_ns; } XemuEmucapMemoryBatch;
static struct { uint64_t ram_size; } machine = {64}, *current_machine = &machine;
#define XEMU_EMUCAP_BATCH_MAX_RANGES 64
#define XEMU_EMUCAP_BATCH_MAX_BYTES 65536
#define QEMU_CLOCK_VIRTUAL 0
#define g_new0(T,n) ((T *)calloc(n,sizeof(T)))
#define g_malloc malloc
#define g_free free
#define QAPI_LIST_APPEND(tail,text) do { strList *n=calloc(1,sizeof(*n)); n->value=text; *tail=n; tail=&n->next; } while(0)
static bool held, halted, running, resume_in_barrier;
static int begins, ends, callbacks;
static uint8_t ram[64];
static bool runstate_is_running(void) { return running; }
static void error_setg(Error **error, const char *fmt, ...) { static Error e; (void)fmt; *error=&e; }
static uint64_t current_frame_boundary(void) { assert(held); return 7; }
static int64_t qemu_clock_get_ns(int clock) { (void)clock; assert(held); return 42; }
static const uint8_t *nv2a_emucap_begin_ram_read(bool *previous) {
    assert(!held); begins++; *previous=halted; halted=true;
    /* Existing rendered bytes become coherent before the final writer lock. */
    ram[4]=0xab; ram[5]=0xcd; held=true; running=resume_in_barrier; return ram;
}
static void nv2a_emucap_end_ram_read(bool previous) { assert(held); ends++; held=false; halted=previous; }
/* Old handler calls back into a non-recursive GPU lock it already owns. */
static void nv2a_emucap_hold_writers(bool hold) { held=hold; }
static void cpu_physical_memory_read(uint64_t addr, void *out, uint32_t size) {
    callbacks++; assert(!held); memcpy(out,ram+addr,size);
}
static void qapi_free_XemuEmucapMemoryBatch(XemuEmucapMemoryBatch *batch) {
    while(batch->reads) { strList *n=batch->reads; batch->reads=n->next; free(n->value); free(n); }
    free(batch);
}
'''
cases = r'''
int main(void) {
    XemuEmucapMemoryRange a={4,2}, b={5,1}, invalid={63,2};
    XemuEmucapMemoryRangeList second={&b,NULL}, first={&a,&second};
    Error *error=NULL;
    for(int previous=0;previous<2;previous++) {
        halted=previous;
        XemuEmucapMemoryBatch *out=qmp_xemu_emucap_read_memory_batch(&first,&error);
        assert(out && !error && !held && halted==previous);
        assert(!strcmp(out->reads->value,"abcd"));
        assert(!strcmp(out->reads->next->value,"cd"));
        assert(out->frame_boundary==7 && out->virtual_ns==42 && callbacks==0);
        qapi_free_XemuEmucapMemoryBatch(out);
    }
    int before=begins;
    second.value=&invalid;
    assert(!qmp_xemu_emucap_read_memory_batch(&first,&error));
    assert(error && begins==before && begins==ends);
    second.value=&b; error=NULL; resume_in_barrier=true;
    assert(!qmp_xemu_emucap_read_memory_batch(&first,&error));
    assert(error && !held && halted && begins==ends);
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-frozen-ram-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text(preamble + source[start:end] + cases)
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-Wno-unused-function',
                    str(temp / 'test.c'), '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('coherent batch bytes, callback avoidance, preflight and barrier cleanup passed')
