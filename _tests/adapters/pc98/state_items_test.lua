-- Requirements: empty items do not truncate a save or get read, and presave refreshes
-- derived buffers before serialization. A read failure must never report success.
local root, dir = arg[1] or ".", assert(arg[2], "temporary output directory required")
local states = dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/state_items.lua")
local prepared = false
local values = { "stale", "tail" }
manager = { machine = { prepare_state_save = function() prepared = true; values[1] = "fresh" end } }
local writes = {}
local items = {
  [0] = { size = 1, count = 5, read_block = function() return values[1] end,
    write = function(_, offset, value) writes[offset] = value end },
  [1] = { size = 1, count = 0, read_block = function() error("Invalid save item") end },
  [2] = { size = 1, count = 4, read_block = function() return values[2] end,
    write = function(_, offset, value) writes[5 + offset] = value end },
  [3] = { size = 0, count = 0 },
}
emu = { item = function(index) return items[index] end }
local count, skipped = states.save(dir)
assert(count == 2 and skipped == 0, "empty item truncated the save or caused a read failure")
assert(prepared, "save did not prepare current device state")
local function read(path)
  local f = assert(io.open(path, "rb")); local data = f:read("*a"); f:close(); return data
end
assert(read(dir .. "/item_000000.bin") == "fresh", "stale derived buffer saved")
assert(read(dir .. "/item_000002.bin") == "tail", "trailing device state missing")
local restored = states.load(dir)
assert(restored == 2 and writes[0] == string.byte("f") and writes[8] == string.byte("l"))
items[2].read_block = function() error("device read failed") end
local failed, reason = states.save(dir)
assert(failed == nil and reason:find("device read failed", 1, true))
items[2].read_block = function() return "x" end
failed, reason = states.save(dir)
assert(failed == nil and reason:find("length", 1, true), "short item read reported success")
-- Older producers may explicitly encode zero-length entries. They restore no device bytes.
local manifest = assert(io.open(dir .. "/manifest.txt", "wb"))
assert(manifest:write("1|1|0|0|empty.bin\n")); manifest:close()
local empty = assert(io.open(dir .. "/empty.bin", "wb")); empty:close()
local empty_restored, empty_skipped = states.load(dir)
assert(empty_restored == 0 and empty_skipped == 0, "empty entries claimed device restoration")
manager.machine.prepare_state_save = function() error("presave failed") end
local ok = pcall(states.save, dir)
assert(not ok, "presave failure must not produce a successful save")
-- Failed output writes still close both open files, so bridge staging cleanup can run.
manager.machine.prepare_state_save = function() end
local original_open, closed = io.open, 0
io.open = function()
  return { write = function() return nil, "disk full" end,
           close = function() closed = closed + 1; return true end }
end
failed, reason = states.save("virtual")
io.open = original_open
assert(failed == nil and reason:find("disk full", 1, true) and closed == 2)
print("PC-98 save item lifecycle tests passed")
