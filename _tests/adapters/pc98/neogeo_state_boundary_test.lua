-- Exercise the registered production callbacks with a controlled scheduler/socket.
-- A stop must publish after scheduler unwind; only the pause owner may resume.
local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
assert(os.getenv("EMUCAP_MAME_PROFILE") == "neogeo_aes")
local callbacks, writes, input = {}, {}, {}
local paused, resumed, goes, native_calls = 0, 0, 0, 0
local debugger = { execution_state = "run" }
local cpu = { shortname = "m68000", debug = {} }
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
set(callbacks.frame, "frame_wait_target", 1)
set(callbacks.frame, "frame_wait_requested", 1)
set(callbacks.frame, "frame_wait_stop", true)
set(callbacks.frame, "frame_wait_screen_start", 6)
callbacks.frame()
assert(machine.paused and paused == 1 and #writes == 0)
assert(debugger.execution_state == "run", "frame stop must not enter a debugger instruction hook")
callbacks.periodic()
assert(#writes == 0 and entered == 0, "timer/scheduler stack published an early stop")
machine.state_io_boundary = true
callbacks.periodic()
assert(writes[2]:match("^%$OK#") and entered == 1)
set(callbacks.periodic, "service_frozen_socket", service)
input = { "$c#63" }
service()
assert(resumed == 1 and not machine.paused and goes == 1, "owned frame pause was not released")
-- Existing external pause survives a control request and subsequent resume signal.
machine.paused = true
set(service, "running", false)
input = { "$c#63" }
service()
assert(machine.paused and resumed == 1)
-- Ordinary pause also defers publication; an existing debugger stop stays in place.
machine.paused = false
writes = {}
handle("\x03")
assert(machine.paused and #writes == 0)
set(callbacks.periodic, "service_frozen_socket", function() end)
callbacks.periodic()
assert(writes[1]:match("^%$S05#"))
set(callbacks.periodic, "service_frozen_socket", service)
debugger.execution_state = "stop"
writes = {}
handle("\x03")
assert(writes[1]:match("^%$S05#") and goes == 2)
-- The native reply is forwarded without scheduling a frame or changing the halt.
local path = ("test.sta"):gsub(".", function(c) return string.format("%02x", c:byte()) end)
for _, result in ipairs({"completed", "unsafe_halt", "load_failed_rolled_back"}) do
  outcome = result
  writes = {}
  handle("qEmucap,loadsync," .. path)
  assert(writes[2]:find("STATE:" .. result, 1, true) and goes == 2)
end
assert(native_calls == 3)
outcome = "load_failed_unverified"
handle("qEmucap,loadsync," .. path)
local before = goes
handle("c")
assert(goes == before, "uncertain restore accepted resume")
-- Socket failure after an uncertain load must preserve the owned pause.
set(service, "running", false)
set(service, "read_socket", function() error("transport closed") end)
service()
assert(machine.paused and resumed == 1)
print("Neo Geo deferred stop, pause ownership and native state failure handling passed")
