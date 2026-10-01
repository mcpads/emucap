#!/usr/bin/env python3
"""Compile actual native state-I/O methods against controlled scheduler/file failures."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,default=Path('adapters/mame-neogeo/work/mame-src'))
a=p.parse_args()
source=(a.source/'src/emu/machine.cpp').read_text()
methods=source[source.index('bool running_machine::state_io_boundary() const'):source.index('void running_machine::schedule_save(')]
scheduler=(a.source/'src/emu/schedule.cpp').read_text()
scope=scheduler[scheduler.index('\tstruct execution_scope',scheduler.index('void device_scheduler::timeslice()')):scheduler.index('\tbool call_debugger',scheduler.index('void device_scheduler::timeslice()'))]
header=(a.source/'src/emu/schedule.h').read_text()
start=header.index('\tbool at_state_io_boundary() const')
boundary=header[start:header.index('\n\t}',start)+3]
code=r'''
#include <cassert>
#include <stdexcept>
#include <string>
#include <vector>
using u8 = unsigned char;
enum { STATERR_NONE, STATERR_FAILED };
enum { OPEN_FLAG_READ=1, OPEN_FLAG_WRITE=2, OPEN_FLAG_CREATE=4, OPEN_FLAG_CREATE_PATHS=8 };
enum class machine_phase { INIT, RUNNING };
enum class saveload_schedule { NONE, SAVE };
struct emu_file {
    static inline int opens=0, closes=0, removals=0;
    static inline bool open_failure=false;
    bool opened=false;
    emu_file(const char*,int) {}
    bool open(std::string const&) { ++opens; opened=!open_failure; return open_failure; }
    void close() { if(opened) { ++closes; opened=false; } }
    void remove_on_close() { ++removals; }
    ~emu_file() { close(); }
};
struct Scheduler {
    bool m_in_timeslice=false, timers_clear=true;
    void* m_executing_device=nullptr;
    void* m_callback_timer=nullptr;
'''+boundary+r'''
    bool can_save() const { return timers_clear; }
};
struct Save {
    int guest=17, reads=0, writes=0, captures=0, restores=0;
    bool write_failure=false, read_failure=false, capture_failure=false, restore_failure=false;
    bool throw_read=false;
    int write_file(emu_file&) { ++writes; return write_failure; }
    int read_file(emu_file&) { ++reads; guest=99; if(throw_read) throw std::runtime_error("read"); return read_failure; }
    int write_buffer(void* data,size_t) { ++captures; *static_cast<u8*>(data)=guest; return capture_failure; }
    int read_buffer(const void* data,size_t) { ++restores; if(restore_failure) return STATERR_FAILED; guest=*static_cast<const u8*>(data); return STATERR_NONE; }
};
struct ram_state { static size_t get_size(Save&) { return 1; } };
struct running_machine {
    machine_phase m_current_phase=machine_phase::RUNNING;
    bool m_paused=true, m_state_file_io_active=false;
    saveload_schedule m_saveload_schedule=saveload_schedule::NONE;
    Scheduler m_scheduler;
    Save m_save;
    bool state_io_boundary() const;
    bool state_io_ready() const;
    std::string state_file_io(bool,std::string const&);
};
'''+methods+r'''
void timeslice_scope(bool& active, bool fail) {
    bool& m_in_timeslice=active;
'''+scope+r'''
    assert(active);
    if(fail) throw std::runtime_error("unwind");
}
int main() {
    bool active=false; timeslice_scope(active,false); assert(!active);
    try { timeslice_scope(active,true); } catch(...) {} assert(!active);
    for(int mask=0;mask<8;++mask) {
        Scheduler scheduler;
        scheduler.m_in_timeslice=bool(mask&1);
        scheduler.m_executing_device=(mask&2)?&scheduler:nullptr;
        scheduler.m_callback_timer=(mask&4)?&scheduler:nullptr;
        assert(scheduler.at_state_io_boundary()==(mask==0));
    }
    for(int mask=0;mask<64;++mask) {
        running_machine m;
        m.m_current_phase=(mask&1)?machine_phase::INIT:machine_phase::RUNNING;
        m.m_paused=!(mask&2); m.m_scheduler.m_in_timeslice=bool(mask&4);
        m.m_scheduler.timers_clear=!(mask&8); m.m_state_file_io_active=bool(mask&16);
        m.m_saveload_schedule=(mask&32)?saveload_schedule::SAVE:saveload_schedule::NONE;
        assert(m.state_io_ready()==(mask==0));
        if(mask) { int before=emu_file::opens; assert(m.state_file_io(false,"x")=="unsafe_halt"); assert(emu_file::opens==before); assert(m.m_save.writes==0 && m.m_save.guest==17); }
    }
    running_machine m;
    m.m_scheduler.timers_clear=false;
    assert(m.state_io_boundary() && !m.state_io_ready());
    int unopened=emu_file::opens;
    assert(m.state_file_io(false,"x")=="unsafe_halt");
    assert(emu_file::opens==unopened && m.m_save.writes==0);
    m.m_scheduler.timers_clear=true;
    assert(m.state_file_io(false,"")=="invalid_path");
    emu_file::open_failure=true; assert(m.state_file_io(false,"x")=="open_failed");
    emu_file::open_failure=false; assert(!m.m_state_file_io_active);
    assert(m.state_file_io(false,"x")=="completed" && m.m_save.guest==17);
    m.m_save.write_failure=true; assert(m.state_file_io(false,"x")=="save_failed"); assert(emu_file::removals==1);
    m.m_save.capture_failure=true; assert(m.state_file_io(true,"x")=="rollback_capture_failed"); assert(m.m_save.reads==0);
    m.m_save.capture_failure=false; m.m_save.read_failure=true;
    assert(m.state_file_io(true,"x")=="load_failed_rolled_back" && m.m_save.guest==17);
    m.m_save.throw_read=true; assert(m.state_file_io(true,"x")=="load_failed_rolled_back" && m.m_save.guest==17);
    m.m_save.restore_failure=true; assert(m.state_file_io(true,"x")=="load_failed_unverified" && m.m_save.guest==99);
    m.m_save.throw_read=false; m.m_save.read_failure=false;
    assert(m.state_file_io(true,"x")=="completed");
    assert(!m.m_state_file_io_active);
    assert(emu_file::closes==emu_file::opens-1); // Only the failed open has no close.
}
'''
with tempfile.TemporaryDirectory(prefix='mame-state-io-') as temp:
    root=Path(temp); cpp=root/'test.cpp'; exe=root/'test'; cpp.write_text(code)
    subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined','-fno-omit-frame-pointer',str(cpp),'-o',str(exe)],check=True)
    subprocess.run([str(exe)],check=True)
print('Native state-I/O admission, error classification, rollback and scope unwind pass')
