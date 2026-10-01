#!/usr/bin/env python3
"""Exercise pacing debt and suspend-excluding anchors independently of wake-timer time."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
source = (ROOT / 'adapters/xemu/work/xemu/ui/xemu-emucap-pacing.c').read_text()


def function(signature):
    start = source.index(signature)
    return source[start:source.index('\n}', start) + 2]


with tempfile.TemporaryDirectory(prefix='xemu-pacing-anchor-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text('''#include <assert.h>
#include <stdint.h>
#include <stdbool.h>
#define __APPLE__ 1
#define QEMU_CLOCK_REALTIME 0
#define QEMU_CLOCK_VIRTUAL 1
#define CLOCK_UPTIME_RAW 8
#define MAX(a,b) ((a) > (b) ? (a) : (b))
static struct { uint32_t percent; int64_t host, guest; void *wake; } pacing;
static int64_t active_now, wall_now, guest_now, wake_deadline;
static uint64_t clock_gettime_nsec_np(int clock) {
    assert(clock == CLOCK_UPTIME_RAW); return active_now;
}
static int64_t qemu_clock_get_ns(int clock) {
    return clock == QEMU_CLOCK_REALTIME ? wall_now : guest_now;
}
static void timer_mod_ns(void *timer, int64_t deadline) {
    (void)timer; wake_deadline = deadline;
}
static uint64_t muldiv64(uint64_t a, uint32_t b, uint32_t c) { return a*b/c; }
static void init(void) {}
''' + '\n'.join(function(s) for s in (
        'static int64_t pacing_host_now(', 'static void anchor(',
        'static int64_t wait_for(', 'bool xemu_emucap_pacing_warp_ready(')) + '''
int main(void) {
    pacing.percent = 100;
    anchor();
    guest_now = 10000000;
    active_now = wall_now = 500000000;
    assert(wait_for(guest_now) == 0);
    /* Ordinary CPU lag cannot erase debt by silently moving an anchor. */
    assert(pacing.host == 0 && pacing.guest == 0);
    active_now = wall_now = 510000000;
    guest_now = 600000000;
    assert(wait_for(guest_now) == 90000000);
    /* A one-hour suspend changes the wake clock, not active host time. */
    wall_now += 3600000000000LL;
    assert(wait_for(guest_now) == 90000000);
    assert(!xemu_emucap_pacing_warp_ready(0));
    assert(wake_deadline == wall_now + 90000000);
    assert(pacing.host == 0 && pacing.guest == 0);
    active_now += 89999999;
    assert(!xemu_emucap_pacing_warp_ready(0));
    active_now++;
    assert(xemu_emucap_pacing_warp_ready(0));
    pacing.percent = 50;
    assert(wait_for(guest_now) == 600000000);
    pacing.percent = 0;
    assert(wait_for(guest_now) == 0);
    return 0;
}
''')
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', str(temp / 'test.c'),
                    '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('host lag retained; suspend excluded; wake deadlines retain their own clock domain')
