#!/usr/bin/env python3
"""Actual plugin request guard and peekbatch branch under constant native clocks."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
s=(root/'adapters/mame-pc98/plugins/emucap_gdbstub/init.lua').read_text()
guard=s[s.index('  local function request_operation('):s.index('  -- Read-only requests answered inside a throttle wait.')]
boundary=s[s.index('  local function time_string()'):s.index('  -- Effective native governor.')]
branch=s.split('    elseif name == "peekbatch" then',1)[1].split('    elseif name == "mediastatus" then',1)[0]
code=r'''
local boundary_seq,packet_sequence,ram=0,0,1
local injected_failure_request,injected_failure_consumed='',false
local debugger,running,hold_requested,frame_wait_target={},false,true,nil
local manager={machine={paused=true,time={seconds=7,attoseconds=0}}}
local function current_frame() return 42 end
local function clear_frame_wait() end
local socket,reply={},nil
local function ack_packet(_,v) packet_sequence=packet_sequence+1;reply=v end
local function hex_to_string(s) return s end
local function string_to_hex(s) return (s:gsub('.',function(c)return string.format('%02x',string.byte(c)) end)) end
local cpu={spaces={program={validate_peek_block=function()return true end,read_peek_block=function()return string.char(ram) end}}}
'''+boundary+'\nlocal function batch(rest)\n'+branch+'\nend\n'+r'''
local handle_payload_safely
local fail=false
local function handle(payload)
 if payload=='qEmucap,peekbatch,0:1' then batch('0:1')
 elseif payload=='qEmucap,pacing' then ack_packet(socket,'100')
 else ram=ram+1;if fail then error('partial native replacement') end;ack_packet(socket,'OK') end
end
'''+guard+r'''
local function snapshot()
 assert(handle_payload_safely('qEmucap,peekbatch,0:1'))
 assert(reply:match('^OK|'));return reply:match('^OK|([^|]+)|')
end
local old=snapshot();assert(snapshot()==old)
for _,op in ipairs({'stateload','load','loadsync','loaditems','loadpixels','finishload','regload','reset','resetsync','mediachange','M','G','s','c'}) do
 local payload=(#op==1) and op or ('qEmucap,'..op..',state')
 for _,fault in ipairs({false,true}) do
  fail=fault;assert(handle_payload_safely(payload));local current=snapshot()
  assert(current~=old,op..' reused same-time epoch');assert(snapshot()==current);old=current
 end
end
assert(handle_payload_safely('qEmucap,pacing'));assert(snapshot()==old)
print('MAME shared plugin: native/custom loads, reset, mapping writes and partial failure retire same-time epochs; repeated batch/pacing stable')
'''
with tempfile.TemporaryDirectory() as d:
 p=Path(d)/'check.lua';p.write_text(code);subprocess.run(['lua',str(p)],cwd=root,check=True)
