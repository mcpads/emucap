#!/usr/bin/env python3
"""Exercise actual low-rate wait loops with controlled host clocks/control wakeups."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def extract(path, signature):
    source = path.read_text()
    start = source.index(signature)
    return source[start:source.index('\n}', start) + 2]


xemu_source = ROOT / 'adapters/xemu/work/xemu/ui/xemu-emucap-pacing.c'
xemu = r'''
#include <assert.h>
#include <stdint.h>
#include <stdbool.h>
#define MAX(a,b) ((a)>(b)?(a):(b))
#define MIN(a,b) ((a)<(b)?(a):(b))
#define DIV_ROUND_UP(a,b) (((a)+(b)-1)/(b))
#define SCALE_MS 1000000
#define QEMU_CLOCK_VIRTUAL 1
typedef struct { void *halt_cond; } CPUState;
static struct { uint32_t percent; int64_t host,guest; } pacing;
static int64_t host_now,guest_now;
static bool enabled,running,empty;
static int calls,action;
static bool icount_enabled(void) { return enabled; }
static bool cpu_can_run(CPUState *cpu) { (void)cpu; return running; }
static bool cpu_work_list_empty(CPUState *cpu) { (void)cpu; return empty; }
static int64_t pacing_host_now(void) { return host_now; }
static int64_t qemu_clock_get_ns(int domain) {
    assert(domain==QEMU_CLOCK_VIRTUAL); return guest_now;
}
static uint64_t muldiv64(uint64_t a,uint32_t b,uint32_t c) { return a*b/c; }
static void init(void) {}
static void anchor(void) { pacing.host=host_now; pacing.guest=guest_now; }
static void qemu_cond_timedwait_bql(void *cond,int64_t ms) {
    (void)cond;
    assert(ms>=1 && ms<=10);
    host_now+=ms*SCALE_MS;
    assert(++calls<200);
    /* Simulate control serviced while the native conditional wait releases BQL. */
    if(action==1) running=false;
    if(action==2) empty=false;
    if(action==3) pacing.percent=0;
}
''' + extract(xemu_source, 'static int64_t wait_for(') + '\n' + extract(
    xemu_source, 'void xemu_emucap_pacing_wait(') + r'''
int main(void) {
    CPUState cpu={0};
    for(action=0;action<=5;action++) {
        enabled=action!=4; running=action!=5; empty=true;
        calls=0; host_now=0; guest_now=10000000;
        pacing.percent=1; pacing.host=pacing.guest=0;
        xemu_emucap_pacing_wait(&cpu);
        assert(guest_now==10000000 && pacing.host==0 && pacing.guest==0);
        if(action==0) assert(calls==100 && host_now==1000000000);
        else if(action<=3) assert(calls==1 && host_now==10000000);
        else assert(calls==0 && host_now==0);
    }
    return 0;
}
'''

flycast = r'''
#include <algorithm>
#include <cassert>
#include <stdexcept>
#include <unistd.h>
#include "emucap_pacing.h"
static EmucapSamplePacer g_pacer;
static bool g_pace_unlimited,g_in_pacing_wait;
static uint32_t g_pace_percent=1;
static int64_t now;
static int sleeps,services,action,contained;
static int64_t monotonic_ns() { return now; }
static int controlled_sleep(useconds_t us) {
    assert(us<=10000); assert(++sleeps<200);
    // Host time continues even for a sub-microsecond final remainder.
    now+=std::max<int64_t>(us*1000,1000); return 0;
}
#define usleep controlled_sleep
static bool pacing_idle() {
    assert(g_in_pacing_wait); services++;
    if(action==2) throw std::runtime_error("control service failure");
    if(action==3) throw 42;
    return action==1;
}
static void contain_service_exception(const char *site,const char *) {
    assert(std::string(site)=="pacing_wait"); contained++;
}
''' + extract(ROOT / 'adapters/flycast/emucap.cpp', 'void emucap_pace_samples(') + r'''
int main() {
    for(action=0;action<=4;action++) {
        now=0; sleeps=services=contained=0; g_in_pacing_wait=false;
        g_pace_unlimited=action==4; g_pacer.reanchor();
        emucap_pace_samples(512);
        assert(!g_in_pacing_wait && g_pace_percent==1);
        assert(sleeps==services);
        if(action==0) {
            const int64_t target=512LL*1000000000*100/44100;
            assert(now>=target && now-target<=1000 && services>100);
        } else if(action<=3) assert(services==1 && now==10000000);
        else assert(services==0 && now==0);
        assert(contained==(action==2 || action==3));
        if(action==1 || action==4) {
            assert(g_pacer.deadline_after(441,100,now)==now+10000000);
        }
    }
}
'''

with tempfile.TemporaryDirectory(prefix='pacing-control-waits-') as directory:
    temp = Path(directory)
    for name, source, compiler, extension in (
        ('xemu', xemu, 'cc', 'c'), ('flycast', flycast, 'c++', 'cpp')
    ):
        path = temp / f'{name}.{extension}'
        path.write_text(source)
        command = [compiler, '-O1', '-g', '-Wall', '-Wextra', '-Werror',
                   '-fsanitize=address,undefined', '-fno-omit-frame-pointer']
        if extension == 'cpp':
            command += ['-std=c++17', '-I', str(ROOT / 'adapters/flycast')]
        subprocess.run(command + [str(path), '-o', str(temp / name)], check=True)
        subprocess.run([str(temp / name)], check=True)
        print(f'{name}: actual 1-percent wait loop, bounded slices and interruption PASS')
