#!/usr/bin/env python3
"""Exercise the maintained Windows input publication and early rumble path."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = root / 'adapters/mesen2/work/mesen'
patch = root / 'adapters/mesen2/patches/0026-publish-windows-input-devices-before-use.patch'
commit = next(line.split('=', 1)[1] for line in (root / 'adapters/mesen2/upstream.lock').read_text().splitlines() if line.startswith('MESEN_COMMIT='))
with tempfile.TemporaryDirectory() as tmp:
    folder = Path(tmp)
    (folder / 'Windows').mkdir()
    for name in ('WindowsKeyManager.cpp', 'WindowsKeyManager.h'):
        (folder / 'Windows' / name).write_bytes(subprocess.check_output(['git', '-C', str(source), 'show', f'{commit}:Windows/{name}']))
    subprocess.run(['git', 'apply', str(patch)], cwd=folder, check=True)
    cpp = (folder / 'Windows/WindowsKeyManager.cpp').read_text()
    def function(name):
        start = cpp.index('void WindowsKeyManager::' + name)
        end = cpp.index('\n}\n', start) + 3
        return cpp[start:end]
    test = r'''
#include <atomic>
#include <cassert>
#include <cstdint>
#include <memory>
#include <thread>
using namespace std;
static atomic<bool> constructing{false}, allow_ready{false};
static atomic<int> calls{0};
struct XInputManager {
 explicit XInputManager(void*) {constructing.store(true); while(!allow_ready.load()) this_thread::yield();}
 bool NeedToUpdate() {return false;}
 void UpdateDeviceList() {}
 void SetForceFeedback(uint16_t, uint16_t) {++calls;}
};
struct DirectInputManager {DirectInputManager(void*, void*) {}}
;
struct Signal {void Wait(int) {this_thread::yield();}};
class WindowsKeyManager {
public:
 void* _emu=nullptr; void* _hWnd=nullptr;
 unique_ptr<XInputManager> _xInput;
 unique_ptr<DirectInputManager> _directInput;
 atomic<bool> _devicesReady{false}, _stopUpdateDeviceThread{false};
 thread _updateDeviceThread; Signal _stopSignal;
 void StartUpdateDeviceThread();
 void SetForceFeedback(uint16_t, uint16_t);
};
'''
    test += function('StartUpdateDeviceThread') + function('SetForceFeedback')
    test += r'''
int main() {
 WindowsKeyManager keys;
 keys.StartUpdateDeviceThread();
 while(!constructing.load()) this_thread::yield();
 for(int i=0;i<1000;++i) keys.SetForceFeedback(0,0);
 assert(calls.load()==0);
 allow_ready.store(true);
 while(!keys._devicesReady.load(memory_order_acquire)) this_thread::yield();
 keys.SetForceFeedback(123,456);
 assert(calls.load()==1);
 keys._stopUpdateDeviceThread.store(true);
 keys._updateDeviceThread.join();
}
'''
    file = folder / 'publication.cpp';file.write_text(test)
    binary = folder / 'publication'
    subprocess.run(['c++', '-std=c++17', '-pthread', '-fsanitize=address,undefined', str(file), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True, timeout=10)
print('Pinned patch replay and asynchronous Windows input publication passed')
