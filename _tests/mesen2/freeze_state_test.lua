-- A recording terminal and a later explicit step are different halt causes. Frame-boundary
-- eligibility survives a frame step, while instruction and interrupt halts remain ineligible.

local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local FreezeState = require("emucap_freeze_state")

local function equal(actual, expected, label)
  if actual ~= expected then
    error(string.format("%s: expected %s, got %s", label, tostring(expected), tostring(actual)), 2)
  end
end

do
  local state = FreezeState.halt("record_window", true)
  equal(state.reason, "record_window", "recording halt reason")
  equal(FreezeState.can_start_recording(state), true, "recording boundary is reusable")

  state = FreezeState.after_step("frames")
  equal(state.reason, "step", "frame step replaces the prior halt reason")
  equal(FreezeState.can_start_recording(state), true, "frame step ends at a reusable boundary")
end

do
  local state = FreezeState.after_step("instructions")
  equal(state.reason, "step", "instruction step halt reason")
  equal(FreezeState.can_start_recording(state), false,
    "instruction step does not claim a frame boundary")

  state = FreezeState.halt("breakpoint", false)
  equal(FreezeState.can_start_recording(state), false,
    "breakpoint halt does not inherit earlier recording eligibility")
end

print("ALL EMUCAP FREEZE STATE TESTS PASSED")
