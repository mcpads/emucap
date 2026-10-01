#!/usr/bin/env python3
"""Native audio observation must be read-only and hash canonical, independently specified bytes."""
from pathlib import Path
import hashlib
import shlex
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
source = (ROOT / 'adapters/xemu/work/xemu/hw/xbox/mcpx/apu/apu.c').read_text()
start = source.index('bool mcpx_apu_clock_continuation(')
function = source[start:source.index('\n}', start) + 2]
header = (ROOT / 'adapters/xemu/work/xemu/hw/xbox/mcpx/apu/apu.h').read_text()
start = header.index('typedef struct MCPXAPUContinuation')
end = header.index('} MCPXAPUContinuation;', start) + len('} MCPXAPUContinuation;')
preamble = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <glib.h>
#define MCPX_HW_MAX_VOICES 256
#define NUM_SAMPLES_PER_FRAME 32
#define cpu_to_be32 GUINT32_TO_BE
#define cpu_to_be64 GUINT64_TO_BE
typedef struct { int voice, list; } VoiceWorkItem;
typedef struct { int queue_len; VoiceWorkItem queue[256]; } VoiceWorkDispatch;
typedef struct {
    int lock;
    bool guest_voice_pending;
    int64_t guest_deadline;
    int64_t *guest_timer;
    uint32_t guest_fraction;
    int32_t ep_frame_div;
    struct { VoiceWorkDispatch voice_work_dispatch; uint64_t voice_locked[4];
             float sample_buf[32][2]; } vp;
} MCPXAPUState;
static MCPXAPUState *g_state;
static bool running;
static bool runstate_is_running(void) { return running; }
static void qemu_mutex_lock(int *lock) { assert(*lock == 0); *lock = 1; }
static void qemu_mutex_unlock(int *lock) { assert(*lock == 1); *lock = 0; }
static int64_t timer_expire_time_ns(int64_t *timer) { return *timer; }
'''
queue = struct.pack('>IIII', 7, 1, 19, 2)
locks = struct.pack('>QQQQ', (1 << 7) | (1 << 19), 0, 0, 0)
samples = struct.pack('>f', 1.5) + bytes(248) + struct.pack('>f', -.25)
cases = r'''
int main(void) {
    int64_t retry = 1001000;
    MCPXAPUState d = { .guest_voice_pending = true, .guest_deadline = 1000000,
        .guest_timer = &retry, .guest_fraction = 16000, .ep_frame_div = 17 };
    d.vp.voice_work_dispatch.queue_len = 2;
    d.vp.voice_work_dispatch.queue[0] = (VoiceWorkItem){7, 1};
    d.vp.voice_work_dispatch.queue[1] = (VoiceWorkItem){19, 2};
    d.vp.voice_locked[0] = (1ULL << 7) | (1ULL << 19);
    d.vp.sample_buf[0][0] = 1.5f;
    d.vp.sample_buf[31][1] = -.25f;
    MCPXAPUState saved = d;
    MCPXAPUContinuation out;
    g_state = &d;
    assert(mcpx_apu_clock_continuation(&out));
    assert(memcmp(&saved, &d, sizeof(d)) == 0);
    assert(out.pending && out.queue_length == 2 && out.deadline == 1000000);
    assert(out.timer_deadline == 1001000 && out.fraction == 16000 && out.ep_frame_div == 17);
    assert(strcmp(out.queue_sha256, "QUEUE") == 0);
    assert(strcmp(out.locks_sha256, "LOCKS") == 0);
    assert(strcmp(out.samples_sha256, "SAMPLES") == 0);
    running = true;
    assert(!mcpx_apu_clock_continuation(&out));
    assert(memcmp(&saved, &d, sizeof(d)) == 0);
    running = false;
    d.vp.voice_work_dispatch.queue_len = 257;
    assert(!mcpx_apu_clock_continuation(&out) && d.lock == 0);
    g_state = NULL;
    assert(!mcpx_apu_clock_continuation(&out));
    return 0;
}
'''
for label, data in [('QUEUE', queue), ('LOCKS', locks), ('SAMPLES', samples)]:
    cases = cases.replace('"'+label+'"', '"'+hashlib.sha256(data).hexdigest()+'"')
with tempfile.TemporaryDirectory(prefix='xemu-audio-observation-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text(preamble + header[start:end] + function + cases)
    flags = shlex.split(subprocess.check_output(['pkg-config', '--cflags', '--libs', 'glib-2.0'], text=True))
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', str(temp / 'test.c'),
                    '-o', str(temp / 'test'), *flags], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('audio observation: canonical queue/lock/sample hashes, unchanged state, frozen-only admission')
