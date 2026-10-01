#!/usr/bin/env python3
"""Check the built host's JIT transition wrappers across threads and translation units.

Run after adapters/xemu/build.sh. Native game execution separately exercises OS permissions.
"""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / 'adapters/xemu/work/xemu'
header = (SOURCE / 'include/qemu/osdep.h').read_text()
start = header.index('extern __thread int qemu_thread_jit_mode;')
end = header.index('#else', start)
wrappers = header[start:end]
definition = next(line for line in (SOURCE / 'util/osdep.c').read_text().splitlines()
                  if line.startswith('__thread int qemu_thread_jit_mode ='))
with tempfile.TemporaryDirectory(prefix='xemu-jit-') as directory:
    temp = Path(directory)
    (temp / 'jit.h').write_text('''#include <stdbool.h>
void observe_transition(int execute);
#define pthread_jit_write_protect_np observe_transition
''' + wrappers + '\nvoid other_unit_execute(void);\n')
    (temp / 'other.c').write_text('#include "jit.h"\nvoid other_unit_execute(void) { qemu_thread_jit_execute(); }\n')
    (temp / 'test.c').write_text('''#include <assert.h>
#include <pthread.h>
#include "jit.h"
''' + definition + '''
static __thread int actual_mode = -1;
static __thread int transitions;
void observe_transition(int execute) {
    actual_mode = execute;
    transitions++;
}
static void *worker(void *first_execute) {
    assert(qemu_thread_jit_mode == -1);
    if (first_execute) {
        other_unit_execute();
        qemu_thread_jit_execute();
        assert(actual_mode == 1 && transitions == 1);
    } else {
        qemu_thread_jit_write();
        qemu_thread_jit_write();
        assert(actual_mode == 0 && transitions == 1);
    }
    qemu_thread_jit_write();
    assert(actual_mode == 0);
    int previous = transitions;
    other_unit_execute();
    assert(actual_mode == 1 && transitions == previous + 1);
    qemu_thread_jit_execute();
    assert(transitions == previous + 1);
    qemu_thread_jit_write();
    assert(actual_mode == 0 && transitions == previous + 2);
    return NULL;
}
int main(void) {
    pthread_t threads[2];
    assert(pthread_create(&threads[0], NULL, worker, NULL) == 0);
    assert(pthread_create(&threads[1], NULL, worker, &threads[1]) == 0);
    for (int i = 0; i < 2; i++) assert(pthread_join(threads[i], NULL) == 0);
    assert(qemu_thread_jit_mode == -1 && transitions == 0);
    worker(NULL);
    return 0;
}
''')
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-pthread',
                    str(temp / 'test.c'), str(temp / 'other.c'), '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('JIT first-use, repeat, alternating, cross-unit and per-thread transitions passed')
