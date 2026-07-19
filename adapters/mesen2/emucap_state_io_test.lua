-- Production savestate file helper tests.
-- Run: EMUCAP_ADAPTER_DIR=adapters/mesen2 lua adapters/mesen2/emucap_state_io_test.lua

local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local StateIo = require("emucap_state_io")

local base = os.tmpname()
os.remove(base)
local path = base .. ".mss"

local function write(pathname, data)
  local file = assert(io.open(pathname, "wb"))
  assert(file:write(data))
  assert(file:close())
end

local function read(pathname)
  local file = assert(io.open(pathname, "rb"))
  local data = assert(file:read("*a"))
  assert(file:close())
  return data
end

write(path, "old")
local saved_bytes = StateIo.save({
  createSavestate = function() return "new-state" end,
}, path, 1)
assert(saved_bytes == 9, "save must report serialized byte length")
assert(read(path) == "new-state", "save must replace the destination")

write(path, "keep-me")
local ok = pcall(StateIo.save, {
  createSavestate = function() error("serializer failed") end,
}, path, 2)
assert(not ok, "serializer failure must be reported")
assert(read(path) == "keep-me", "serializer failure must preserve the previous file")

local loaded_data
local loaded_bytes = StateIo.load({
  loadSavestate = function(data)
    loaded_data = data
    return true
  end,
}, path)
assert(loaded_bytes == 7 and loaded_data == "keep-me", "load must pass exact file bytes to Mesen")

ok = pcall(StateIo.load, {
  loadSavestate = function() return false end,
}, path)
assert(not ok, "a rejected savestate must be reported")

os.remove(path)
print("ALL EMUCAP STATE I/O TESTS PASSED")
