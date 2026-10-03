#!/usr/bin/env python3
"""Count payload reads in the shipped Lua batch branch, including late rejection."""
from pathlib import Path
import argparse, subprocess, tempfile
root=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--source',type=Path);a=p.parse_args()
s=(a.source or root/'adapters/mame-pc98/plugins/emucap_gdbstub/init.lua').read_text()
branch=s.split('    elseif name == "peekbatch" then',1)[1].split('    elseif name == "mediastatus" then',1)[0]
setup='''
local reads, validations, reply = 0, 0, nil
local memory_limit = 256
local space = {}
function space:validate_peek_block(address, length)
  validations = validations + 1
  return address + length <= memory_limit
end
function space:read_peek_block(address, length)
  local bytes = {}
  for i = 0, length - 1 do
    if address + i >= memory_limit then return nil end
    reads = reads + 1
    bytes[#bytes + 1] = string.char((address + i) % 256)
  end
  return table.concat(bytes)
end
local cpu = { spaces = { program = space } }
local debugger, running, frame_wait_target = true, false, nil
local manager = { machine = { paused = true } }
local socket = {}
local function hex_to_string(s) return s end
local function string_to_hex(s) return (s:gsub('.', function(c) return string.format('%02x',string.byte(c)) end)) end
local function boundary_token() return 'frozen' end
local function ack_packet(_, v) reply = v end
'''
tests='''
for _, spec in ipairs({'0:2,ff:2','0:2,100:1','0:2,garbage','0:2,','0:2,,1:1','', '0:0', 'ffffffff:2', '10000000000000000:1', 'ffffffffffffffff:1', string.rep('0:1,',64)..'0:1', '0:10000,0:1'}) do
  reads, validations, reply = 0, 0, nil
  invoke(spec)
  assert(reply == 'E1C', spec .. ': ' .. tostring(reply))
  assert(reads == 0, spec .. ': payload read before rejection: ' .. reads)
end
reads = 0
invoke('fe:2,fe:2,0:1')
assert(reply == 'OK|frozen|feff,feff,00', tostring(reply))
assert(reads == 5)
memory_limit = 65536
reads = 0
invoke('0:10000')
assert(reads == 65536 and #reply == #('OK|frozen|') + 131072)
reads = 0
invoke(string.rep('0:1,',63)..'0:1')
assert(reads == 64)
for _, state in ipairs({'running','frame_wait','unpaused'}) do
  running = state == 'running'
  frame_wait_target = state == 'frame_wait' and 1 or nil
  manager.machine.paused = state ~= 'unpaused'
  reads = 0
  invoke('0:1')
  assert(reply == 'E1E' and reads == 0)
end
running, frame_wait_target, manager.machine.paused = false, nil, true
space.validate_peek_block = nil
reads = 0
invoke('0:1')
assert(reply == 'E1F' and reads == 0)
print('MAME batch preflight: late invalid ranges read zero payload; duplicates/order/end boundary preserved; old native rejected')
'''
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'test.lua';f.write_text(setup+'\nlocal function invoke(rest)\n'+branch+'\nend\n'+tests)
 subprocess.run(['lua',str(f)],check=True)
