-- Exercise production interrupt/frozen-service callbacks with a controlled native pause.
local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
assert(os.getenv("EMUCAP_MAME_PROFILE") == "pc98")
local callbacks, input, writes = {}, {}, {}
local pauses, resumes, goes = 0, 0, 0
local debugger = { execution_state = "run" }
local cpu = { shortname = "i386", debug = {} }
function cpu.debug:go() goes = goes + 1; debugger.execution_state = "run" end
local machine = { paused = false, screens = { at = function() return { frame_number = 7 } end }, video = {} }
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
emu = { file = function() return socket end,
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
set(callbacks.frame, "frame_wait_target", 120)
set(callbacks.frame, "frame_wait_requested", 120)
set(callbacks.frame, "frame_wait_stop", true)
set(callbacks.frame, "frame_wait_screen_start", 7)
-- Native pause is established before S05; only a subsequent explicit c releases it.
input = { "$c#63" }
handle("\x03")
assert(pauses == 1 and resumes == 1 and goes == 1)
assert(upvalue(callbacks.frame, "frame_wait_target") == nil, "interrupted frame target survived")
local count = #writes
callbacks.frame()
assert(#writes == count, "cancelled wait emitted a later completion")
-- Socket loss while servicing a freeze cannot implicitly resume the guest.
set(service, "read_socket", function() error("owned socket closed") end)
handle("\x03")
assert(machine.paused and resumes == 1 and goes == 1)
assert(not upvalue(service, "in_frozen_socket_service"), "service latch survived EOF")
-- Failed native pause is an error, never an S05 success or usable control.
machine.paused = false
emu.pause = function() end
writes = {}
local ok, err = pcall(handle, "\x03")
assert(not ok and tostring(err):find("native pause was not established", 1, true))
assert(#writes == 0)
handle("c")
assert(goes == 1 and writes[2]:find("STATE:load_failed_unverified", 1, true))
-- Failure to send an already verified stop preserves the halt and clears the service latch.
machine.paused = true
socket.write = function() error("stop reply transport failed") end
local sent, send_error = pcall(handle, "\x03")
assert(not sent and tostring(send_error):find("stop reply transport failed", 1, true))
assert(machine.paused and not upvalue(service, "in_frozen_socket_service") and resumes == 1)
print("PC-98 interrupt clears frame wait, publishes after native pause, and preserves halt on socket loss")
