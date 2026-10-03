package.path = (os.getenv('EMUCAP_ADAPTER_DIR') or '.') .. '/?.lua;' .. package.path
local Advance = require('emucap_control_advance')
local function fixture(unit, count)
  local s = {generation = 1, monotonicMs = 100, halted = true,
    instructionBoundaries = 0, haltedCpuSteps = 0, ppuCycles = 0}
  local calls = {steps = 0, breaks = 0, wakes = 0}
  local host = {stepType = {ppuFrame = 1, step = 2}}
  function host.getControlState() local r = {}; for k,v in pairs(s) do r[k]=v end; return r end
  function host.step(n, kind)
    assert(s.halted); calls.steps = calls.steps + 1; s.halted = false
    s.ppuRemaining = kind == 1 and n * 100 or -1
    s.instructionRemaining = kind == 2 and n or -1
  end
  function host.breakExecution()
    calls.breaks = calls.breaks + 1
    -- This replaces counters without proving stop, just like the native request.
    s.ppuRemaining, s.instructionRemaining = -1, 1
  end
  function host.endPacingWait() calls.wakes = calls.wakes + 1 end
  return Advance.new(host, unit or 'frames', count or 60), s, calls, host
end

-- Scheduled work is not completed work. A fractional frame is not rounded up,
-- and cancel acknowledgement cannot substitute for the eventual native halt.
do
  local d,s,c = fixture(); assert(d:poll() == nil)
  s.ppuCycles = 1250; assert(d:cancel()); assert(c.breaks == 1 and c.wakes == 1)
  assert(d:poll() == nil); s.halted = true
  local r = assert(d:poll()); assert(r.status == 'interrupted' and r.count == 12)
  assert(c.steps == 1 and r.stop_deadline_ms == 5100)
  r.count = 999; assert(d:poll().count == 12)
end
-- An undispatched cancellation does not advance the guest.
do
  local d,s,c = fixture(); assert(d:cancel()); local r = assert(d:poll())
  assert(r.count == 0 and r.status == 'interrupted' and c.steps == 0 and c.breaks == 0)
end
-- Exact completion wins a late cancellation only once a stop is observed.
do
  local d,s,c = fixture('frames', 2); d:poll(); s.ppuCycles = 200
  d:cancel(); assert(d:poll() == nil); s.halted = true
  assert(d:poll().status == 'completed' and c.steps == 1)
end
-- Intermediate chunks continue, while an independent breakpoint halts early.
do
  local d,s,c = fixture(); d:poll(); s.ppuCycles = 3000; s.halted = true
  assert(d:poll() == nil and c.steps == 2)
  s.ppuCycles = 3700; s.halted = true
  local r = assert(d:poll()); assert(r.count == 37 and r.reason == 'native_interruption')
end
-- Native instruction units include HALT iterations; they are not opcode counts.
do
  local d,s = fixture('instructions', 10); d:poll()
  s.instructionBoundaries, s.haltedCpuSteps, s.halted = 2, 8, true
  assert(d:poll().status == 'completed')
end
-- Repeated cancellation and late stopped callbacks cannot renew a failed budget.
do
  local d,s,c = fixture(); d:poll(); d:cancel()
  s.monotonicMs = 5000; d:cancel(); assert(c.breaks == 1)
  s.monotonicMs, s.halted = 5101, true
  local r,e = d:poll(); assert(r == nil and e == 'stop_deadline_exceeded')
  assert(d:is_retired()); s.monotonicMs = 5102; assert(d:poll() == nil)
end
-- A host timeout requests stop first and publishes only after actual halt.
do
  local d,s,c = fixture(); d:poll(); s.monotonicMs = 245100
  assert(d:poll() == nil and c.breaks == 1)
  s.halted = true; assert(d:poll().reason == 'host_timeout')
end
-- Changed debugger generation or regressing native counters invalidate proof.
for _, field in ipairs({'generation', 'monotonicMs', 'ppuCycles', 'instructionBoundaries', 'haltedCpuSteps'}) do
  local d,s,c = fixture(); d:poll()
  if field == 'generation' then s[field] = 2
  elseif field == 'monotonicMs' then s[field] = 99
  else s[field] = -1 end
  assert(d:poll() == nil and d:is_retired())
  if field == 'generation' then assert(c.breaks == 0 and c.wakes == 0) end
end
do
  local d,s = fixture(); d:poll(); s.ppuCycles = 10; d:poll(); s.ppuCycles = 9
  local r,e = d:poll(); assert(r == nil and e == 'native_counter_regressed')
end
-- A native dispatch failure may already have resumed; retire and request stop.
do
  local d,s,c,h = fixture()
  h.step = function() s.halted = false; error('failed after resume') end
  local r,e = d:poll()
  assert(r == nil and e == 'native_step_failed' and d:is_retired())
  assert(c.breaks == 1 and c.wakes == 1)
end
print('mesen owned native advance tests passed')
