local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "adapters/mesen2"
package.path = dir .. "/?.lua;" .. package.path

local NativeCallstack = require("emucap_native_callstack")

local function ok(condition, message)
  if not condition then error("FAIL " .. message) end
end

local result = NativeCallstack.capture(0x08000100, true, {
  {
    source = 0x08000000, target = 0x08001000, returnAddress = 0x08000004,
    returnStackPointer = 0, flags = 0,
    absoluteSource = 0, sourceMemoryType = 92,
    absoluteTarget = 0x1000, targetMemoryType = 92,
    absoluteReturn = 4, returnMemoryType = 92,
  },
  {
    source = 0x08001020, target = 0x18, returnAddress = 0x08001024,
    returnStackPointer = 0, flags = 2,
    absoluteSource = 0x1020, sourceMemoryType = 92,
    absoluteTarget = 0x18, targetMemoryType = 12,
    absoluteReturn = 0x1024, returnMemoryType = 92,
  },
})

ok(result.depth == 3, "current PC and two native frames are returned")
ok(result.frames[1].pc == 0x08000100 and result.frames[1].thumb, "frozen PC is first")
ok(result.frames[2].kind == "irq", "native IRQ flag is preserved")
ok(result.frames[2].pc == 0x08001020, "innermost native frame comes next")
ok(result.frames[3].kind == "call", "ordinary call remains distinct from IRQ")
ok(result.order == "innermost_to_outermost", "response order is explicit")
ok(result.native_depth == 2 and not result.truncated, "native depth and completeness are explicit")

local many = {}
for index = 1, 511 do
  many[index] = { source = index, flags = 0 }
end
local bounded = NativeCallstack.capture(7, false, many)
ok(bounded.depth == 64, "public response is bounded")
ok(bounded.frames[2].pc == 511 and bounded.frames[64].pc == 449, "innermost frames are retained")
ok(bounded.truncated, "native or response capacity is reported")

print("ALL MESEN NATIVE CALL STACK TESTS PASSED")
