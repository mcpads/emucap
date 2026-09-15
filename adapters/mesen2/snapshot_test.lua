local directory = arg[0]:match("^(.*[/\\])") or "./"
package.path = directory .. "?.lua;" .. package.path
local Snapshot = require("emucap_snapshot")
local saves = 0
local host = {
  getState = function() return { ["cpu.pc"]=0x8123, ["cpu.k"]=0x80, ["ppu.frameCount"]=42 } end,
  getMasterClock = function() return 9007199254740993 end,
  getRomInfo = function() return { fileSha1Hash=string.rep("ab",20) } end,
  createSavestate = function() saves=saves+1; return "\0\n\255state" end,
}
local ok, kind = Snapshot.capture(host, false, "snes")
assert(not ok and kind == "unsafe_halt" and saves == 0)
ok, kind = Snapshot.capture(host, true, "nes")
assert(not ok and saves == 0)
local result
ok, result = Snapshot.capture(host, true, "snes")
assert(ok and saves == 1 and result.hex == "000aff7374617465")
assert(result.halt.cycle.value == "9007199254740993")
assert(result.halt.frame.value == "42" and result.halt.pc == 0x8123 and result.halt.program_bank == 0x80)
host.createSavestate = function() host.getMasterClock=function() return 0 end; return "changed" end
assert(not pcall(Snapshot.capture, host, true, "snes"))
print("snapshot capture: unsafe rejection, exact clock, payload and drift checks passed")
