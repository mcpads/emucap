#!/usr/bin/env python3
"""Exercise stopped GPU coherence and repeated already-frozen device saves.

RAM must contain GPU writes before RAM serialization, which precedes device
pre_save. Model that ordering and execute the actual native callback bodies.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=ROOT / 'adapters/xemu/work/xemu/hw/xbox/nv2a/nv2a.c')
args = parser.parse_args()
source = args.source.read_text()
start = source.index('static void nv2a_vm_state_change(')
end = source.index('static int nv2a_pre_load(', start)
callbacks = source[start:end]
# Before the repair there was no pre_save hook. The negative control must still
# compile so it fails on observable coherence/ownership, not a missing symbol.
if 'static int nv2a_pre_save(' not in callbacks:
    callbacks += 'static int nv2a_pre_save(void *opaque) { (void)opaque; return 0; }\n'
preamble = r'''
#include <assert.h>
#include <stdbool.h>
typedef enum { RUN_STATE_RUNNING, RUN_STATE_PAUSED, RUN_STATE_DEBUG,
               RUN_STATE_SAVE_VM, RUN_STATE_RESTORE_VM, RUN_STATE_SHUTDOWN } RunState;
typedef struct { struct { bool halt; } pfifo; } NV2AState;
static bool locked, bql = true, pending;
static unsigned gpu, ram, guest_instructions, guest_frames;
#define qatomic_set(p,v) (*(p)=(v))
static void nv2a_lock_fifo(NV2AState *d) { (void)d; assert(bql && !locked); locked=true; }
static void nv2a_unlock_fifo(NV2AState *d) { (void)d; assert(bql && locked); locked=false; }
static void bql_unlock(void) { assert(bql); bql=false; }
static void bql_lock(void) { assert(!bql); bql=true; }
static void pgraph_pre_savevm_trigger(NV2AState *d) {
    assert(locked && bql && d->pfifo.halt); pending=true;
}
static void pgraph_pre_savevm_wait(NV2AState *d) {
    assert(!locked && !bql && d->pfifo.halt && pending);
    ram=gpu; pending=false;
}
static void pgraph_pre_shutdown_trigger(NV2AState *d) { (void)d; assert(locked); }
static void pgraph_pre_shutdown_wait(NV2AState *d) { (void)d; assert(!locked && !bql); }
'''
cases = r'''
int main(void) {
    RunState stops[] = {RUN_STATE_PAUSED, RUN_STATE_DEBUG, RUN_STATE_SAVE_VM, RUN_STATE_RESTORE_VM};
    NV2AState d={0};
    for (unsigned i=0;i<sizeof(stops)/sizeof(stops[0]);i++) {
        nv2a_vm_state_change(&d,true,RUN_STATE_RUNNING);
        assert(!d.pfifo.halt && !locked && bql);
        gpu=0x12340000+i; ram=0;
        nv2a_vm_state_change(&d,false,stops[i]);
        assert(d.pfifo.halt && !locked && bql && !pending);
        assert(ram==gpu && guest_instructions==0 && guest_frames==0);
        for (unsigned repeat=0;repeat<3;repeat++) {
            /* Already paused -> save emits no runstate notification. RAM first. */
            unsigned saved_ram=ram;
            assert(saved_ram==gpu);
            assert(nv2a_pre_save(&d)==0 && locked);
            /* Device serialization, including its error exit, calls post_save. */
            assert(nv2a_post_save(&d)==0 && !locked && bql && d.pfifo.halt);
        }
    }
    nv2a_vm_state_change(&d,false,RUN_STATE_SHUTDOWN);
    assert(!locked && bql);
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-frozen-snapshot-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text(preamble + callbacks + cases)
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-Wno-unused-parameter', str(temp/'test.c'), '-o', str(temp/'test')], check=True)
    subprocess.run([str(temp/'test')], check=True)
print('stopped GPU coherence, already-frozen save ownership, resume and shutdown passed')
