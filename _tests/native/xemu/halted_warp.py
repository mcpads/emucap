#!/usr/bin/env python3
"""Verify clock admission for guest HLT, control stop, missing timers and a closed pacing gate."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
source = (ROOT / 'adapters/xemu/work/xemu/accel/tcg/icount-common.c').read_text()
start = source.index('void icount_start_warp_timer(void)')
function = source[start:source.index('\n}', start) + 2]
preamble = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#define REPLAY_MODE_PLAY 1
#define CHECKPOINT_CLOCK_WARP_START 1
#define QEMU_CLOCK_VIRTUAL 1
#define QEMU_CLOCK_VIRTUAL_RT 2
#define QEMU_TIMER_ATTR_EXTERNAL 1
#define CPU_FOREACH(cpu) for ((cpu) = &guest_cpu; (cpu); (cpu) = 0)
#define warn_report_once(...) ((void)0)
typedef struct { bool stopped, stop; } CPUState;
static CPUState guest_cpu;
static bool running, idle, gate_open, icount_sleep;
static int replay_mode, notifications, host_timers;
static int64_t next_deadline;
static struct {
    int vm_clock_seqlock, vm_clock_lock;
    int64_t qemu_icount_bias, vm_clock_warp_start;
    void *icount_warp_timer;
} timers_state;
static int icount_enabled(void) { return 1; }
static bool runstate_is_running(void) { return running; }
static bool all_cpu_threads_idle(void) { return idle; }
static bool xemu_emucap_clock_supported(void) { return true; }
static bool cpu_is_stopped(CPUState *cpu) { return cpu->stopped; }
static bool qtest_enabled(void) { return false; }
static bool replay_checkpoint(int checkpoint) { (void)checkpoint; return true; }
static bool replay_has_event(void) { return false; }
static void qemu_clock_notify(int clock) { assert(clock == QEMU_CLOCK_VIRTUAL); notifications++; }
static int64_t qemu_clock_get_ns(int clock) { (void)clock; return 100000; }
static int64_t qemu_clock_deadline_ns_all(int clock, int flags) {
    (void)clock; (void)flags; return next_deadline;
}
static bool xemu_emucap_pacing_warp_ready(int64_t deadline) {
    assert(deadline == next_deadline); return gate_open;
}
static void seqlock_write_lock(int *seq, int *lock) { (void)seq; (void)lock; }
static void seqlock_write_unlock(int *seq, int *lock) { (void)seq; (void)lock; }
static void qatomic_set_i64(int64_t *value, int64_t next) { *value = next; }
static void timer_mod_anticipate(void *timer, int64_t when) {
    (void)timer; (void)when; host_timers++;
}
'''
cases = r'''
int main(void) {
    const int64_t deadlines[] = {-1, 0, 5000};
    for (int bits = 0; bits < 32; bits++) {
        running = bits & 1;
        idle = bits & 2;
        guest_cpu.stopped = bits & 4;
        guest_cpu.stop = bits & 8;
        gate_open = bits & 16;
        for (unsigned i=0; i<sizeof(deadlines)/sizeof(deadlines[0]); i++) {
            next_deadline = deadlines[i];
            timers_state.qemu_icount_bias = 12345;
            notifications = host_timers = 0;
            icount_start_warp_timer();
            bool admitted = running && idle && !guest_cpu.stopped && !guest_cpu.stop;
            bool due = admitted && next_deadline == 0;
            bool advance = admitted && next_deadline > 0 && gate_open;
            assert(timers_state.qemu_icount_bias == 12345 + (advance ? next_deadline : 0));
            assert(notifications == (due || advance ? 1 : 0));
            assert(host_timers == 0);
        }
    }
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-halted-warp-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text(preamble + function + cases)
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', str(temp / 'test.c'),
                    '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('96 native halt/stop/deadline/gate combinations preserve clock admission')
