local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Step = require("emucap_step")

local function eq(actual, expected, message)
  if actual ~= expected then
    error(("FAIL %s: %s ~= %s"):format(message, tostring(actual), tostring(expected)))
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
