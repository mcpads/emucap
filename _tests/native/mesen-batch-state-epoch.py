#!/usr/bin/env python3
"""Actual frozen state-I/O and batch handlers: equal-clock replacement epochs."""
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/mesen2/emucap-core.lua').read_text()
batch = source[source.index('function handlers.read_memory_batch(p)'):source.index('function handlers.hello()')]
state = source[source.index('local function frozen_state_io('):source.index('-- Recording event bytes')]
setup = r'''
local handlers, response = {}, nil
local STATE, frame, boundary_seq, PORT = "frozen", 42, 0, 1234
local halt_savestate_safe, freeze_state = true, {frame_boundary_proven=false}
local freeze_snapshot, deferred, step_operation = nil,nil,nil
local BATCH_MAX_RANGES, BATCH_MAX_BYTES, DEBUG_READ = 64,65536,0
local SYS, CPU = {}, 0
local Step={active=function() return false end}
local function as_array(v) return v end
local Memory={range=function() return {memory_type=0} end}
local function bounded_sync_count(n) return n end
local function reply_ok(id,data) response={ok=true,data=data} end
local function reply_err(id,kind,message) response={ok=false,kind=kind} end
local function complete_probe() response={ok=true} end
local function start_deferred() error("zero-frame probe must not advance") end
local ram, load_count, partial_failure = 1,0,false
local emu={read=function() return ram end, getState=function() return {pc=123,frame=42} end}
local StateIo={save=function() return 1 end,load=function()
 load_count=load_count+1;ram=ram+1
 if partial_failure then error("failure after native mutation") end
 return 1
end}
'''
checks = r'''
local function snapshot()
 local ok,value=handlers.read_memory_batch({ranges={{memory_type="ram",address=0,length=1}}})
 assert(ok);return value
end
local previous=snapshot()
for _,method in ipairs({"load_state","probe"}) do
 frozen_state_io(method,1,{path="state",state="state",frame=0,length=1,memory_type="ram",address=0})
 assert(response.ok)
 local current=snapshot()
 assert(current.boundary.clocks[1].value==previous.boundary.clocks[1].value)
 assert(current.boundary.stop_epoch~=previous.boundary.stop_epoch,"same-clock native replacement reused stop epoch")
 assert(current.boundary.memory_mapping_epoch~=previous.boundary.memory_mapping_epoch)
 assert(current.reads[1].hex~=previous.reads[1].hex)
 previous=current
end
partial_failure=true
frozen_state_io("load_state",1,{path="state"})
assert(not response.ok and freeze_snapshot==nil)
local current=snapshot()
assert(current.boundary.stop_epoch~=previous.boundary.stop_epoch,"partial native failure reused stop epoch")
previous=current
frozen_state_io("save_state",1,{path="state"})
assert(response.ok and snapshot().boundary.stop_epoch==previous.boundary.stop_epoch)
local calls=load_count
halt_savestate_safe=false
frozen_state_io("load_state",1,{path="state"})
assert(not response.ok and load_count==calls)
assert(snapshot().boundary.stop_epoch==previous.boundary.stop_epoch)
halt_savestate_safe=true
frozen_state_io("load_state",1,{})
assert(not response.ok and load_count==calls)
assert(snapshot().boundary.stop_epoch==previous.boundary.stop_epoch)
'''
with tempfile.TemporaryDirectory(prefix='mesen-state-epoch-') as tmp:
    path=Path(tmp)/'probe.lua';path.write_text(setup+batch+state+checks)
    subprocess.run([shutil.which('lua'),str(path)],cwd=root,check=True)
print('Mesen actual handlers: same-clock load/probe and partial-failure epochs; save/rejected admission unchanged passed')
