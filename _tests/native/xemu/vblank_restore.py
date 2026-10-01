#!/usr/bin/env python3
"""Validate restored vblank admission and renderer invalidation under caller ownership."""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=ROOT/'adapters/xemu/work/xemu/hw/xbox/nv2a/nv2a.c')
args = parser.parse_args()
source = args.source.read_text()
start = source.index('static int nv2a_post_load(')
callback = source[start:source.index('\n}', start)+2]
preamble = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#define bitmap_fill(p,n) memset((p),255,(n)/8)
#define QEMU_CLOCK_VIRTUAL 0
#define NV2A_VBLANK_HZ 60
#define NV2A_VBLANK_MAX_PERIOD_NS 16666667
#define qatomic_set(p,v) (*(p)=(v))
typedef struct { bool pending; int64_t deadline; } Timer;
typedef struct { uint32_t vblank_fraction; int64_t vblank_deadline;
                 Timer *vblank_timer; struct { bool flush_pending,program_data_dirty;
                 unsigned char regs_dirty[256];
                 bool vsh_constants_dirty[192],ltctxa_dirty[26],ltctxb_dirty[52],ltc1_dirty[20],texture_dirty[4];
                 } pgraph; } NV2AState;
static bool locked;
static unsigned arms;
static int64_t now=1000000000;
static int64_t qemu_clock_get_ns(int clock) { (void)clock; return now; }
static bool timer_pending(Timer *t) { return t->pending; }
static int64_t timer_expire_time_ns(Timer *t) { return t->deadline; }
static void nv2a_vblank_arm(NV2AState *d) {
    assert(locked); arms++; d->vblank_timer->pending=true;
    d->vblank_timer->deadline=d->vblank_deadline+NV2A_VBLANK_MAX_PERIOD_NS;
}
'''
cases = r'''
int main(void) {
    Timer timer={true,1000000100};
    NV2AState d={.vblank_timer=&timer,.vblank_deadline=999999999};
    for(unsigned phase=0;phase<60;phase++) {
        d.vblank_fraction=phase; locked=true; arms=0; d.pgraph.flush_pending=false;
        assert(nv2a_post_load(&d,3)==0 && locked && arms==0);
        assert(d.vblank_fraction==phase && timer.deadline==1000000100);
        assert(d.pgraph.flush_pending && d.pgraph.program_data_dirty);
        for(unsigned j=0;j<sizeof(d.pgraph.regs_dirty);j++)assert(d.pgraph.regs_dirty[j]==255);
        assert(d.pgraph.vsh_constants_dirty[191] && d.pgraph.ltctxa_dirty[25] &&
               d.pgraph.ltctxb_dirty[51] && d.pgraph.ltc1_dirty[19] && d.pgraph.texture_dirty[3]);
    }
    uint32_t invalid[]={60,61,UINT32_MAX};
    for(unsigned i=0;i<3;i++) {
        d.vblank_fraction=invalid[i]; locked=true; d.pgraph.flush_pending=false;
        assert(nv2a_post_load(&d,3)==-EINVAL);
        assert(locked && !d.pgraph.flush_pending && arms==0);
        assert(d.vblank_fraction==invalid[i] && timer.deadline==1000000100);
    }
    for(int pending=0;pending<2;pending++) {
        d.vblank_fraction=17; locked=true; arms=0;
        timer.pending=pending; timer.deadline=now+NV2A_VBLANK_MAX_PERIOD_NS+1;
        assert(nv2a_post_load(&d,3)==0 && locked && arms==1);
        assert(d.vblank_fraction==0 && d.vblank_deadline==now && d.pgraph.flush_pending);
    }
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-vblank-restore-') as directory:
    temp=Path(directory)
    (temp/'test.c').write_text(preamble+callback+cases)
    subprocess.run(['cc','-O2','-Wall','-Wextra','-Werror','-Wno-unused-parameter',str(temp/'test.c'),'-o',str(temp/'test')],check=True)
    subprocess.run([str(temp/'test')],check=True)
print('vblank phase rejection, renderer invalidation and caller lock ownership passed')
