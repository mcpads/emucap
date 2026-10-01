#!/usr/bin/env python3
"""Check cumulative native device deadlines against independent rational periods."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3] / 'adapters/xemu/work/xemu'


def extract(path, signature):
    source = (ROOT / path).read_text()
    start = source.index(signature)
    return source[start:source.index('\n}', start) + 2]


source = r'''
#include <assert.h>
#include <stdint.h>
#define NANOSECONDS_PER_SECOND 1000000000LL
#define NV2A_VBLANK_HZ 60
#define NUM_SAMPLES_PER_FRAME 32
struct Timer { int64_t deadline; };
typedef struct { int64_t vblank_deadline; uint32_t vblank_fraction;
                 struct Timer *vblank_timer; } NV2AState;
typedef struct { int64_t guest_deadline; uint32_t guest_fraction;
                 struct Timer *guest_timer; } MCPXAPUState;
static void timer_mod_ns(struct Timer *timer, int64_t deadline) {
    timer->deadline = deadline;
}
'''
source += extract('hw/xbox/nv2a/nv2a.c', 'static void nv2a_vblank_arm(')
source += extract('hw/xbox/mcpx/apu/apu.c', 'static void mcpx_apu_guest_arm(')
source += r'''
int main(void) {
    struct Timer video_timer = {0}, audio_timer = {0};
    NV2AState video = { .vblank_timer = &video_timer };
    MCPXAPUState audio = { .guest_timer = &audio_timer };
    const int64_t origin = 123456789;
    video.vblank_deadline = audio.guest_deadline = origin;
    /* One hour, every boundary: no accumulated truncation or host rebasing. */
    for (int64_t n = 1; n <= 60 * 3600; n++) {
        nv2a_vblank_arm(&video);
        assert(video_timer.deadline == origin + n * 1000000000LL / 60);
        assert(video.vblank_fraction == n * 1000000000LL % 60);
    }
    for (int64_t n = 1; n <= 1500 * 3600; n++) {
        mcpx_apu_guest_arm(&audio);
        assert(audio_timer.deadline == origin + n * 32000000000LL / 48000);
        assert(audio.guest_fraction == n * 32000000000LL % 48000);
    }
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-device-periods-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text(source)
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', str(temp / 'test.c'),
                    '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('one hour of vblank and APU deadlines: exact cumulative rational periods')
