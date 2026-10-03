-- Exercise the registered production callbacks with a controlled scheduler/socket.
-- A stop must publish after scheduler unwind; only the pause owner may resume.
local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
assert(os.getenv("EMUCAP_MAME_PROFILE") == "neogeo_aes")
local callbacks, writes, input = {}, {}, {}
local paused, resumed, goes, native_calls = 0, 0, 0, 0
local debugger = { execution_state = "run" }
local cpu = { shortname = "m68000", debug = {}, spaces = {} }
function cpu.debug:go() goes = goes + 1; debugger.execution_state = "run" end
local machine = { paused = false, state_io_boundary = false,
  screens = { at = function() return { frame_number = 7, raster_state_supported = true } end }, video = {} }
local outcome = "completed"
function machine:state_file_io(load, path)
  native_calls = native_calls + 1
  assert(path == "test.sta")
  return outcome
end
manager = { machine = machine }
local socket = {
  open = function() end,
  write = function(_, value) writes[#writes + 1] = value; return #value end,
  read = function() return table.remove(input, 1) or "" end,
}
emu = { file = function() return socket end,
  pause = function() machine.paused = true; paused = paused + 1 end,
  unpause = function() machine.paused = false; resumed = resumed + 1 end,
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
local entered = 0
set(callbacks.periodic, "service_frozen_socket", function() entered = entered + 1 end)

local screen = {frame_number=7}
machine.screens.at = function() return screen end
local ticks = 100
emu.osd_ticks = function() return ticks end
emu.osd_ticks_per_second = function() return 1000 end
local function request(name, argument)
  writes = {}
  local encoded = argument and ("," .. argument:gsub(".", function(c) return string.format("%02x", c:byte()) end)) or ""
  handle("qEmucap," .. name .. encoded)
  local reply = writes[#writes]
  assert(reply and reply:match("^%$"), name .. " did not reply")
  return reply:match("^%$(.*)#%x%x$")
end
set(service, "running", false)
machine.paused, machine.state_io_boundary = true, true
assert(request("features"):find("owned_frames_v1", 1, true))
assert(request("haltstate") == "frozen")
assert(request("framebegin", "cancel:120:1000") == "FRAME|cancel|running|0|120|none")
machine.paused, machine.state_io_boundary = false, false
screen.frame_number = 10
assert(request("framecancel", "stale") == "E00")
assert(not machine.paused and upvalue(callbacks.frame, "frame_wait_target") == 120)
assert(request("framecancel", "cancel") == "FRAME|cancel|stopping|3|120|cancelled")
assert(machine.paused and debugger.execution_state == "run")
assert(request("framepoll", "cancel") == "FRAME|cancel|stopping|3|120|cancelled")
assert(request("framefinish", "cancel") == "E09")
assert(request("haltstate") == "running")
writes = {}; callbacks.periodic()
assert(#writes == 0 and entered == 0, "pause intent escaped the scheduler boundary")
machine.state_io_boundary = true
callbacks.periodic()
assert(#writes == 0 and entered == 1, "owned stop emitted an uncorrelated response")
assert(request("framepoll", "cancel") == "FRAME|cancel|interrupted|3|120|cancelled")
assert(request("framefinish", "cancel") == "FRAME|cancel|interrupted|3|120|cancelled")
assert(request("haltstate") == "frozen")
assert(request("framepoll", "cancel") == "E00")
-- Completion wins a cancellation race, but cannot publish before scheduler unwind.
assert(request("framebegin", "done:2:1000") == "FRAME|done|running|0|2|none")
machine.paused, machine.state_io_boundary = false, false
screen.frame_number = 12
writes = {}; callbacks.frame()
assert(machine.paused and #writes == 0)
assert(request("framecancel", "done") == "FRAME|done|stopping|2|2|none")
assert(request("framefinish", "done") == "E09")
machine.state_io_boundary = true
writes = {}; callbacks.periodic()
assert(#writes == 0)
assert(request("framefinish", "done") == "FRAME|done|completed|2|2|none")
-- Raw interrupt preserves its own reply and the correlated child terminal.
assert(request("framebegin", "interrupt:120:1000") == "FRAME|interrupt|running|0|120|none")
machine.paused, machine.state_io_boundary = false, false
writes = {}; handle("\x03")
assert(machine.paused and #writes == 0)
assert(request("framepoll", "interrupt") == "FRAME|interrupt|stopping|0|120|cancelled")
machine.state_io_boundary = true
writes = {}; callbacks.periodic()
assert(#writes == 1 and writes[1]:match("^%$S05#"))
assert(request("framefinish", "interrupt") == "FRAME|interrupt|interrupted|0|120|cancelled")
-- Host deadline uses the same scheduler-unwind barrier as explicit cancellation.
assert(request("framebegin", "deadline:120:1000") == "FRAME|deadline|running|0|120|none")
machine.paused, machine.state_io_boundary = false, false
ticks = 1200
writes = {}; callbacks.frame()
assert(machine.paused and #writes == 0)
assert(request("framepoll", "deadline") == "FRAME|deadline|stopping|0|120|host_deadline")
assert(request("framefinish", "deadline") == "E09")
machine.state_io_boundary = true
writes = {}; callbacks.periodic()
assert(#writes == 0)
assert(request("framefinish", "deadline") == "FRAME|deadline|interrupted|0|120|host_deadline")
-- Old native hosts cannot advertise or admit owned frames.
machine.state_io_boundary = nil
assert(not request("features"):find("owned_frames_v1", 1, true))
assert(request("framebegin", "old:2:1000") == "E00")
print("Neo Geo owned frame cancellation, completion race, raw interrupt and old-host rejection passed")
