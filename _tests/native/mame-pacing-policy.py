#!/usr/bin/env python3
"""Run the shipped MAME setpacing branch against controlled native properties."""
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/mame-pc98/plugins/emucap_gdbstub/init.lua').read_text()
owners = source[source.index('  local function native_pacing()'):source.index('  local function handle_emucap(payload)')]
branch = source.split('    elseif name == "setpacing" then', 1)[1].split('    elseif name == "restorepacing" then', 1)[0]
setup = r'''
local state = { throttled=true, speed_factor=1000, throttle_rate=0.75, fastforward=false }
local restoring = false
local video = setmetatable({}, {
  __index=function(_, key)
    if restoring and fault == "read_error" and key == "speed_factor" then error("readback failed") end
    return state[key]
  end,
  __newindex=function(_, key, value)
    if key == "speed_factor" and value == 2000 then
      state[key] = value
      restoring = true
      error("apply failed after mutation")
    end
    if restoring and key == fault then
      if key == "throttled" then value = false
      elseif key == "speed_factor" then value = 1500
      elseif key == "throttle_rate" then value = 0.5 end
    end
    if restoring and fault == "setter_error" then error("restore setter failed") end
    state[key] = value
  end
})
manager = {machine={video=video, options={entries={refreshspeed={value=function() return false end}}}}}
local pacing_revision, pacing_last_key = 0, nil
local function first_screen() return nil end
local function boundary_token() return "1@2.0|9" end
local function hex_to_string(v) return v end
local function string_to_hex(v) return v end
local socket = {}
local reply
local function ack_packet(_, value) reply = value end
'''
lua = shutil.which('lua')
assert lua, 'lua is required'
for fault in ['verified', 'speed_factor', 'throttle_rate', 'throttled', 'read_error', 'setter_error']:
    expected = 'restored' if fault == 'verified' else 'unrestored'
    code = ('local fault = "' + fault + '"\n' + setup + owners +
            '\nlocal function invoke(rest)\n' + branch + '\nend\n' +
            'invoke("limited|2000")\n' +
            f'assert(reply:match("^E1D:{expected}:"), reply)\n')
    with tempfile.TemporaryDirectory(prefix='emucap-mame-policy-') as temp:
        path = Path(temp) / 'test.lua'
        path.write_text(code)
        run = subprocess.run([lua, str(path)], capture_output=True, text=True)
        if run.returncode:
            raise AssertionError(f'{fault}: {run.stderr}')
        print(f'PASS {fault}')
