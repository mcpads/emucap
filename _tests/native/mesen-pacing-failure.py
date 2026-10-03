#!/usr/bin/env python3
"""Execute Mesen pacing handler and both dispatchers with native setter faults."""
from pathlib import Path
import shutil
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/mesen2/emucap-core.lua').read_text()
handler = source[source.index('local function observed_pacing()'):source.index('function handlers.read_memory_batch(p)')]
dispatch = source[source.index('local function dispatch(line)'):source.index('-- step(n)을 청크')]
setup = r'''
local Pacing = dofile("adapters/mesen2/emucap_pacing.lua")
local pacing_state = Pacing.new_state()
local pacing_control_unverified = false
local HAS_AGENT_PACING, HOST_AUDIO = true, false
local JSON_NULL = {}
local function as_array(v) return v end
local STATE, frame = initial_state, 123
local function native(speed)
  return {emulationSpeed=speed, maximumSpeed=false, turbo=false, rewind=false}
end
local value, writes = native(100), 0
emu = {
  getPacing=function() return value end,
  setPacing=function(speed, maximum)
    writes = writes + 1
    value = native(speed)
    if fault == "apply_exception" then error("apply failed after mutation") end
    if writes == 1 and fault ~= "success" then value = native(150) end
    if writes == 2 then
      if fault == "restore_exception" then error("restore failed") end
      if fault == "restore_mismatch" then value = native(175) end
    end
    return value
  end
}
local observed = 0
local handlers = {hello=function() observed=observed+1; return true, {} end}
local response
local function reply_ok(id, data) response={ok=true, data=data} end
local function reply_err(id, kind, message) response={ok=false, kind=kind, message=message} end
local function parse_request(r) return 1, r.method, r.params or {} end
local function normalize_debug_selection() return nil end
'''
check = r'''
local request = {method="execution_speed",params={mode="limited",percent=200}}
entry(request)
assert(STATE == initial_state and frame == 123)
if fault == "success" then
  assert(response.ok)
  assert(response.data.state == initial_state and response.data.frame == 123)
else assert(not response.ok) end
local healthy = fault == "success" or fault == "restored"
response = nil
entry({method="hello"})
assert(response and response.ok == healthy, "post-failure health must be explicit")
assert(observed == (healthy and 1 or 0), "unverified session dispatched a handler")
if not healthy then
  for _, method in ipairs({"resume", "step", "load_state", "press_buttons", "execution_speed"}) do
    response = nil
    assert(entry({method=method}) == nil, "unverified session returned execution action")
    assert(response and not response.ok and response.message:match("unverified"))
  end
end
'''
for initial_state, entry in [('running', 'dispatch'), ('frozen', 'dispatch'), ('frozen', 'handle_in_freeze')]:
    for fault in ['success', 'restored', 'restore_mismatch', 'apply_exception', 'restore_exception']:
        code = f'local fault = "{fault}"\nlocal initial_state = "{initial_state}"\n' + setup + handler + dispatch + f'\nlocal entry = {entry}\n' + check
        with tempfile.TemporaryDirectory(prefix='emucap-mesen-policy-') as temp:
            path = Path(temp)/'test.lua'
            path.write_text(code)
            run = subprocess.run([shutil.which('lua'), str(path)], cwd=root, capture_output=True, text=True)
            if run.returncode:
                raise AssertionError(f'{fault}/{initial_state}/{entry}: {run.stderr}')
        print(f'PASS {fault}/{initial_state}/{entry}')
