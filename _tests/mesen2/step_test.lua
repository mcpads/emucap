local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Step = require("emucap_step")

local function eq(actual, expected, message)
  if actual ~= expected then
    error(("FAIL %s: %s ~= %s"):format(message, tostring(actual), tostring(expected)))
  end
end

-- The two compatible wire spellings normalize to one exact operation, while malformed requests
-- fail before the core can arm native stepping.
do
  local unit, count, err = Step.parse_wire_step("step", { frames = 3 }, 5000)
  eq(unit, "frames", "frame wire unit")
  eq(count, 3, "frame wire count")
  eq(err, nil, "frame wire error")

  unit, count, err = Step.parse_wire_step(
    "step", { count = 4, unit = "instructions" }, 5000)
  eq(unit, "instructions", "unit-aware instruction unit")
  eq(count, 4, "unit-aware instruction count")
  eq(err, nil, "unit-aware instruction error")

  unit, count, err = Step.parse_wire_step("step_instructions", { count = 5 }, 5000)
  eq(unit, "instructions", "instruction alias unit")
  eq(count, 5, "instruction alias count")
  eq(err, nil, "instruction alias error")
end

do
  local invalid = {
    { "step", { unit = "cycles", count = 1 }, "invalid unit" },
    { "step", { count = 0 }, "zero count" },
    { "step", { count = -1 }, "negative count" },
    { "step", { count = 1.5 }, "fractional count" },
    { "step", { count = 1, frames = 1 }, "ambiguous count" },
    { "step", { count = 5001 }, "over-limit count" },
    { "step_instructions", { frames = 1 }, "instruction frames alias" },
    { "step_instructions", { count = 1, unit = "frames" }, "instruction unit mismatch" },
  }
  for _, case in ipairs(invalid) do
    local unit, count, err = Step.parse_wire_step(case[1], case[2], 5000)
    eq(unit, nil, case[3] .. " unit")
    eq(count, nil, case[3] .. " count")
    if type(err) ~= "string" or err == "" then error("FAIL " .. case[3] .. ": missing error") end
  end
end

-- A completed step can claim its exact requested count only after every chunk was consumed.
do
  local state = Step.start(3, 42, "frames", 31)
  local chunk
  local effect
  state, chunk = Step.take_chunk(state, 30, 20000)
  eq(chunk, 30, "first frame chunk")
  local unchanged, effect = Step.complete(state, 500)
  eq(unchanged.remaining, 1, "incomplete step remains active")
  eq(effect, nil, "incomplete step has no terminal")
  state, chunk = Step.take_chunk(unchanged, 30, 20000)
  eq(chunk, 1, "final frame chunk")
  state, effect = Step.complete(state, 501)
  eq(state, nil, "completed step clears active state")
  eq(effect.id, 42, "completed response id")
  eq(effect.result.status, "completed", "completed status")
  eq(effect.result.count, 31, "completed exact count")
  eq(effect.result.state, "frozen", "completed frozen state")
end

-- A pausing debugger stop discards the remaining request and carries only measured stop evidence.
do
  local state = Step.start(4, 43, "instructions", 5000)
  local chunk
  local effect
  state, chunk = Step.take_chunk(state, 30, 20000)
  eq(chunk, 5000, "instruction chunk")
  state, effect = Step.interrupt(state, "breakpoint", 7, 510)
  eq(state, nil, "interrupted step clears active state")
  eq(effect.id, 43, "interrupted response id")
  eq(effect.result.status, "interrupted", "interrupted status")
  eq(effect.result.reason, "breakpoint", "interruption reason")
  eq(effect.result.breakpoint_id, 7, "interruption breakpoint id")
  eq(effect.result.requested, 5000, "requested count remains evidence")
  eq(effect.result.count, nil, "unmeasured progress is not reported as exact")
  eq(effect.result.frame, 510, "interruption frame")
  eq(effect.result.state, "frozen", "interrupted frozen state")
end

-- Interrupting or cancelling an idle state is total and cannot create a terminal response.
do
  local state, effect = Step.interrupt(nil, "breakpoint", 1, 600)
  eq(state, nil, "idle interrupt state")
  eq(effect, nil, "idle interrupt effect")
  eq(Step.cancel(nil), nil, "idle cancellation")
end

print("ALL EMUCAP STEP TESTS PASSED")
