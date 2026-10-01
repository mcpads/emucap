#!/usr/bin/env python3
"""A restored pre-dispatch audio quantum resumes with its queue and an empty sample buffer."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
source = (ROOT / 'adapters/xemu/work/xemu/hw/xbox/mcpx/apu/apu.c').read_text()
start = source.index('static int apu_guest_clock_post_load(')
function = source[start:source.index('\n}', start) + 2]
preamble = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#define MCPX_HW_MAX_VOICES 256
typedef struct { int voice, list; } VoiceWorkItem;
typedef struct { int queue_len; VoiceWorkItem queue[MCPX_HW_MAX_VOICES]; } VoiceWorkDispatch;
typedef struct {
    int ep_frame_div;
    bool guest_voice_pending;
    uint32_t guest_fraction;
    struct { VoiceWorkDispatch voice_work_dispatch; float sample_buf[32][2]; } vp;
} MCPXAPUState;
'''
cases = r'''
int main(void) {
    MCPXAPUState d = { .guest_voice_pending = true, .guest_fraction = 16000 };
    d.vp.voice_work_dispatch.queue_len = 2;
    d.vp.voice_work_dispatch.queue[0] = (VoiceWorkItem){7, 1};
    d.vp.voice_work_dispatch.queue[1] = (VoiceWorkItem){19, 2};
    VoiceWorkDispatch saved = d.vp.voice_work_dispatch;
    for (int i=0; i<32; i++) d.vp.sample_buf[i][0] = d.vp.sample_buf[i][1] = 0.75f;
    assert(apu_guest_clock_post_load(&d, 1) == 0);
    assert(d.guest_voice_pending && d.guest_fraction == 16000);
    assert(memcmp(&saved, &d.vp.voice_work_dispatch, sizeof(saved)) == 0);
    for (int i=0; i<32; i++) {
        assert(d.vp.sample_buf[i][0] == 0 && d.vp.sample_buf[i][1] == 0);
    }
    for (int phase = 0; phase < 8; phase++) {
        d.ep_frame_div = phase;
        assert(apu_guest_clock_post_load(&d, 1) == 0);
        assert(d.ep_frame_div == phase);
    }
    int invalid[] = {-1, 8, INT32_MAX};
    for (unsigned i = 0; i < sizeof(invalid)/sizeof(invalid[0]); i++) {
        d.ep_frame_div = invalid[i];
        d.vp.sample_buf[0][0] = 0.75f;
        assert(apu_guest_clock_post_load(&d, 1) == -EINVAL);
        assert(d.vp.sample_buf[0][0] == 0.75f);
    }
    d.ep_frame_div = 0;
    d.vp.voice_work_dispatch.queue[0].voice = MCPX_HW_MAX_VOICES;
    assert(apu_guest_clock_post_load(&d, 1) == -EINVAL);
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-apu-restore-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text(preamble + function + cases)
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-Wno-unused-parameter',
                    str(temp / 'test.c'), '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('pending audio restore: queue and phase preserved; stale samples cleared')
