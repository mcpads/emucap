#!/usr/bin/env python3
"""Run the native FIFO loop and RAM fence against a latched guest writer."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
NATIVE = ROOT / 'adapters/xemu/work/xemu/hw/xbox/nv2a'


def function(file, signature):
    source = (NATIVE / file).read_text()
    start = source.index(signature)
    return source[start:source.index('\n}', start) + 2]


code = r'''
#include <cassert>
#include <condition_variable>
#include <mutex>
#include <thread>
using QemuMutex=std::mutex;
using QemuCond=std::condition_variable;
struct NV2AState {
    struct { QemuMutex lock; QemuCond fifo_cond,fifo_idle_cond;
             bool fifo_kick=false,halt=false; } pfifo;
    struct { QemuMutex lock; } pgraph;
    bool exiting=false;
};
struct Gate {
    std::mutex mutex; std::condition_variable cv; bool ready=false;
    void set() { std::lock_guard lock(mutex); ready=true; cv.notify_all(); }
    void wait() { std::unique_lock lock(mutex); cv.wait(lock,[&]{return ready;}); }
};
static Gate writer_entered,writer_release,copy_entered,copy_release;
static QemuMutex bql;
static unsigned guest_word=0,writer_commits=0;
static bool pending=true;
static void qemu_mutex_lock(QemuMutex *m) { m->lock(); }
static void qemu_mutex_unlock(QemuMutex *m) { m->unlock(); }
static void qemu_cond_broadcast(QemuCond *c) { c->notify_all(); }
static void qemu_cond_wait(QemuCond *c,QemuMutex *m) {
    std::unique_lock lock(*m,std::adopt_lock); c->wait(lock); lock.release();
}
static void bql_lock() { bql.lock(); }
static void bql_unlock() { bql.unlock(); }
static void pgraph_init_thread(NV2AState *) {}
static void rcu_register_thread() {}
static void rcu_unregister_thread() {}
static void pgraph_process_pending(NV2AState *) {}
static void pgraph_process_pending_reports(NV2AState *) {}
static void pfifo_run_pusher(NV2AState *d) {
    if(!pending) return;
    std::lock_guard graph(d->pgraph.lock);
    writer_entered.set(); writer_release.wait();
    guest_word=0xa5c3; writer_commits++; pending=false;
}
static void pfifo_kick(NV2AState *d) {
    d->pfifo.fifo_kick=true; qemu_cond_broadcast(&d->pfifo.fifo_cond);
}
''' + '\n'.join([
    function('pfifo.c', 'void *pfifo_thread('),
    function('nv2a.c', 'static void nv2a_lock_fifo('),
    function('nv2a.c', 'static void nv2a_unlock_fifo('),
]) + r'''
int main() {
    NV2AState d;
    std::thread gpu([&]{pfifo_thread(&d);});
    writer_entered.wait();
    // Actual FIFO loop retains submission ownership around the guest writer.
    assert(!d.pfifo.lock.try_lock());
    assert(!d.pgraph.lock.try_lock());
    std::thread reader([&]{
        bql_lock(); nv2a_lock_fifo(&d);
        assert(guest_word==0xa5c3 && writer_commits==1);
        copy_entered.set(); copy_release.wait();
        assert(guest_word==0xa5c3 && writer_commits==1);
        d.exiting=true;
        nv2a_unlock_fifo(&d); bql_unlock();
    });
    writer_release.set(); copy_entered.wait();
    // The native fence owns both locks for the whole copy interval.
    assert(!d.pfifo.lock.try_lock());
    assert(!d.pgraph.lock.try_lock());
    copy_release.set(); reader.join(); gpu.join();
    assert(writer_commits==1 && guest_word==0xa5c3);
    assert(d.pfifo.lock.try_lock()); d.pfifo.lock.unlock();
    assert(d.pgraph.lock.try_lock()); d.pgraph.lock.unlock();
}
'''

with tempfile.TemporaryDirectory(prefix='xemu-ram-fence-') as directory:
    temp = Path(directory)
    (temp / 'test.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++17', '-O1', '-g', '-Wall', '-Wextra', '-Werror',
                    '-pthread', '-fsanitize=address,undefined',
                    str(temp / 'test.cpp'), '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True, timeout=10)
print('actual FIFO loop/fence: writer completes before copy; copy excludes writers; locks released PASS')
