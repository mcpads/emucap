-- Exercise production interrupt/frozen-service callbacks with a controlled native pause.
local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
assert(os.getenv("EMUCAP_MAME_PROFILE") == "pc98")
local callbacks, input, writes = {}, {}, {}
local pauses, resumes, goes = 0, 0, 0
local debugger = { execution_state = "run" }
local cpu = { shortname = "i386", debug = {} }
function cpu.debug:go() goes = goes + 1; debugger.execution_state = "run" end
local screen = { frame_number = 7 }
local machine = { paused = false, screens = { at = function() return screen end }, video = {} }
manager = { machine = machine }
local socket = {
  open = function() end,
  write = function(_, value)
    if value:match("^%$S05#") then assert(machine.paused, "stop published before native pause") end
    writes[#writes + 1] = value
    return #value
  end,
  read = function() return table.remove(input, 1) or "" end,
}
emu = { osd_ticks = function() return 100 end, osd_ticks_per_second = function() return 1000 end, file = function() return socket end,
  pause = function() machine.paused = true; pauses = pauses + 1 end,
  unpause = function() machine.paused = false; resumes = resumes + 1 end,
  register_periodic = function(f) callbacks.periodic = f end,
}
for _, name in ipairs({"reset", "stop", "pre_save", "post_load", "frame"}) do
  emu["add_machine_" .. name .. "_notifier"] = function(f) callbacks[name] = f; return f end
end
dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua").startplugin()
local function upvalue(f, name, value, write)
  for i = 1, 100 do
    local found, old = debug.getupvalue(f, i)
    if found == name then
      if write then debug.setupvalue(f, i, value) end
      return old
    end
    if not found then break end
  end
  error("missing callback capture: " .. name)
end
local function set(f, name, value) upvalue(f, name, value, true) end
set(callbacks.periodic, "cpu", cpu)
set(callbacks.periodic, "debugger", debugger)
local service = upvalue(callbacks.periodic, "service_frozen_socket")
local handle = upvalue(service, "handle")

local function request(name, arg)
  writes = {}
  local encoded = arg and ("," .. arg:gsub(".", function(c) return string.format("%02x", c:byte()) end)) or ""
  handle("qEmucap," .. name .. encoded)
  local reply = writes[#writes]
  assert(reply and reply:match("^%$"), name .. " did not reply")
  return reply:match("^%$(.*)#%x%x$")
end
set(handle, "service_frozen_socket", function() end)
set(service, "running", false)
machine.paused = true
debugger.execution_state = "stop"
assert(request("haltstate") == "frozen")
assert(request("framebegin", "one:120:1000") == "FRAME|one|running|0|120|none")
machine.paused = false
local throttle_hook = upvalue(callbacks.reset, "throttle_wait_hook")
local function cancel_wire(id)
  return "$qEmucap,framecancel," .. id:gsub(".", function(c) return string.format("%02x", c:byte()) end) .. "#00"
end
writes = {}
set(throttle_hook, "rxbuf", cancel_wire("wrong"))
assert(not throttle_hook(), "stale cancel ended the native pacing wait")
assert(writes[#writes]:match("^%$E00#"))
writes = {}
set(throttle_hook, "rxbuf", cancel_wire("one"))
assert(throttle_hook(), "matching cancel was not scheduled outside the pacing wait")
assert(#writes == 0)
set(throttle_hook, "rxbuf", "")
assert(request("framebegin", "two:10:1000") == "E09")
assert(request("framecancel", "wrong") == "E00")
assert(upvalue(callbacks.frame, "frame_wait_target") == 120)
screen.frame_number = 10
assert(request("framepoll", "one") == "FRAME|one|running|3|120|none")
assert(request("framefinish", "one") == "E09")
assert(request("framecancel", "one") == "FRAME|one|stopping|3|120|cancelled")
assert(upvalue(callbacks.frame, "frame_wait_target") == nil)
assert(request("haltstate") == "running")
assert(request("framepoll", "one") == "FRAME|one|stopping|3|120|cancelled")
machine.paused = true
assert(request("framepoll", "one") == "FRAME|one|interrupted|3|120|cancelled")
assert(request("framefinish", "wrong") == "E00")
assert(request("framefinish", "one") == "FRAME|one|interrupted|3|120|cancelled")
assert(request("framepoll", "one") == "E00")
assert(request("haltstate") == "frozen")
assert(request("framebegin", "two:2:1000") == "FRAME|two|running|0|2|none")
machine.paused = false
screen.frame_number = 12
writes = {}
callbacks.frame()
assert(#writes == 0, "owned completion leaked a legacy untagged reply")
assert(request("framepoll", "two") == "FRAME|two|stopping|2|2|none")
machine.paused = true
assert(request("framecancel", "two") == "FRAME|two|completed|2|2|none")
assert(request("framefinish", "two") == "FRAME|two|completed|2|2|none")
-- A non-pausing native breakpoint remains observable without consuming the frame target.
assert(request("framebegin", "trace:120:1000") == "FRAME|trace|running|0|120|none")
machine.paused = false
local note_bp = upvalue(callbacks.periodic, "note_breakpoint")
set(note_bp, "consolelog", {"Stopped at breakpoint 1"})
set(note_bp, "consolelast", 0)
set(note_bp, "breaks", { pause={[1]=false}, byidx={[1]=0x1234}, byaddr={[0x1234]=1} })
set(note_bp, "run_debugger_command", function() end)
cpu.state = setmetatable({}, {__index=function() return {value=0} end})
debugger.execution_state = "stop"
writes = {}
assert(note_bp())
assert(writes[1]:match("^%$T05hwbreak:"))
assert(upvalue(callbacks.frame, "frame_wait_target") == 120)
assert(request("framepoll", "trace") == "FRAME|trace|running|0|120|none")
-- An unrelated mutation and an obsolete operation key cannot affect the current owner.
assert(request("setpacing", "unlimited") == "E09")
assert(request("framecancel", "one") == "E00")
assert(request("framecancel", "trace") == "FRAME|trace|stopping|0|120|cancelled")
machine.paused = true
assert(request("framefinish", "trace") == "FRAME|trace|interrupted|0|120|cancelled")
-- Identity text cannot be confused with a terminal-state field.
assert(request("framebegin", "completed:10:100") == "FRAME|completed|running|0|10|none")
machine.paused = false
assert(request("framefinish", "completed") == "E09")
-- The existing host deadline reports the exact reached interval and still needs native pause.
screen.frame_number = screen.frame_number + 1
emu.osd_ticks = function() return 1000 end
callbacks.frame()
assert(request("framepoll", "completed") == "FRAME|completed|stopping|1|10|host_deadline")
machine.paused = true
assert(request("framefinish", "completed") == "FRAME|completed|interrupted|1|10|host_deadline")
print("PC-98 owned frames: identity, partial progress, stop proof, completion race and tracepoint preservation passed")

-- Failed release remains tracked and cannot be reported as clean native control.
local commands = upvalue(handle, "handle_emucap")
local set_inputs = upvalue(commands, "set_inputs")
local clear_inputs = upvalue(set_inputs, "clear_inputs")
local fail_release = true
local field = {set_value=function() end, clear_value=function()
  if fail_release then error("native release failed") end
end}
set(set_inputs, "input_fields", {enter=field})
assert(set_inputs({"enter"}))
assert(not clear_inputs())
assert(upvalue(clear_inputs, "active_input_fields")[field])
assert(upvalue(handle, "control_unverified"))
assert(request("inputstatus") == "STATE:load_failed_unverified")
fail_release = false
assert(clear_inputs())
set(handle, "control_unverified", false)
assert(request("inputstatus") == "0")
-- Setter failure after applying an override is included in rollback.
local held = false
field.set_value = function() held = true; error("partial native set") end
field.clear_value = function() held = false end
assert(not set_inputs({"enter"}))
assert(not held)
assert(request("inputstatus") == "0")
print("PC-98 halt proof and input rollback checks passed")
