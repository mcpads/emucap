-- Production freeze scheduler의 구조적 회귀 테스트.
-- Run: EMUCAP_ADAPTER_DIR=adapters/mesen2 lua adapters/mesen2/native_halt_service_test.lua

local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
local path = dir .. "/emucap-core.lua"
local file = assert(io.open(path, "rb"))
local source = file:read("*a")
file:close()

local body = assert(source:match(
  "local function service_frozen_once%(%)\n(.-)\nend\n\n%-%- 최초 codeBreak"),
  "service_frozen_once body not found")

local function count(pattern)
  local n = 0
  for _ in body:gmatch(pattern) do n = n + 1 end
  return n
end

assert(not body:match("while%s"), "native halt service must not contain a while loop")
assert(not body:match("repeat%s"), "native halt service must not contain a repeat loop")
assert(not body:match("emu%.step%(%s*1"), "instruction-stepping freeze loop must not return")
assert(count("poll_line%(") == 1, "one callback must poll at most one request")
assert(count("flush_tx%(") == 1, "one callback must flush TX at most once")
assert(count("connect%(") == 1, "one callback must attempt reconnect at most once")
assert(source:match("emu%.eventType%.codeBreakIdle"), "patched native idle event must be registered")
assert(source:match("emu%.eventType%.codeBreakIdleSavestate"),
  "safe native halt savestate event must be registered")
assert(source:match('reply_err%(id, "unsafe_halt"'),
  "unsafe halt kinds must reject savestate operations explicitly")
assert(not source:match("FREEZE_BUDGET_MS"), "watchdog rearm budget must be removed")
assert(not source:match("os%.clock"), "freeze deadlines must use wall clock")

print("ALL NATIVE HALT SERVICE TESTS PASSED")
