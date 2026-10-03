-- Native owned advance driver. The dispatcher authenticates/adopts ownership
-- before construction. Call only on Mesen's emulation owner, never a reader thread.
local M = {}
local function integer(n) return math.type(n) == 'integer' and n >= 0 end
local function copy(t)
  local r = {}; for k, v in pairs(t) do r[k] = v end; return r
end

function M.new(host, unit, requested)
  assert(unit == 'frames' or unit == 'instructions')
  assert(integer(requested) and requested > 0 and requested <= 5000)
  local generation, previous, chunk, result, failure
  local done, reason, stop_deadline, advance_deadline = 0, nil, nil, nil
  local self = {}

  local function retire(message)
    if not failure then
      failure = message
      -- A failed native call may already have resumed execution. Request a stop
      -- without inventing evidence that it succeeded; retirement is permanent.
      local ok, current = pcall(host.getControlState)
      if ok and type(current) == 'table' and generation ~= nil
        and current.generation == generation and current.halted == false then
        pcall(host.breakExecution)
        pcall(host.endPacingWait)
      end
    end
    return nil, failure
  end
  local function observe()
    local ok, s = pcall(host.getControlState)
    if not ok or type(s) ~= 'table' or not integer(s.generation)
      or not integer(s.monotonicMs) or not integer(s.instructionBoundaries)
      or not integer(s.haltedCpuSteps) or not integer(s.ppuCycles)
      or type(s.halted) ~= 'boolean' then return retire('invalid_native_observation') end
    if generation and s.generation ~= generation then return retire('native_generation_changed') end
    if previous and (s.monotonicMs < previous.monotonicMs
      or s.instructionBoundaries < previous.instructionBoundaries
      or s.haltedCpuSteps < previous.haltedCpuSteps
      or s.ppuCycles < previous.ppuCycles) then return retire('native_counter_regressed') end
    generation, previous = s.generation, copy(s)
    return s
  end
  local function progress(s)
    if not chunk then return 0 end
    local delta = unit == 'frames' and (s.ppuCycles - chunk.origin.ppuCycles)
      or ((s.instructionBoundaries - chunk.origin.instructionBoundaries)
        + (s.haltedCpuSteps - chunk.origin.haltedCpuSteps))
    -- A cancellation halt can consume an additional CPU step. Count only the
    -- originally admitted work, using its saved target, not replaced remaining.
    return math.min(chunk.count, delta // chunk.per_unit)
  end
  local function stop(s, why)
    if not reason then reason, stop_deadline = why, s.monotonicMs + 5000 end
    if not s.halted then
      local ok = pcall(host.breakExecution)
      local woke = pcall(host.endPacingWait)
      if not ok or not woke then return retire('native_stop_request_failed') end
    end
    return true
  end
  function self:cancel(why)
    if failure then return nil, failure end
    if result then return true end
    if reason then return true end -- never extend the original stop deadline
    local s, err = observe(); if not s then return nil, err end
    return stop(s, why or 'cancelled')
  end
  function self:poll()
    if failure then return nil, failure end
    if result then return copy(result) end
    local s, err = observe(); if not s then return nil, err end
    if stop_deadline and s.monotonicMs > stop_deadline then return retire('stop_deadline_exceeded') end
    if not advance_deadline then
      if not s.halted then return retire('advance_requires_native_halt') end
      advance_deadline = s.monotonicMs + 245000
    end
    if not reason and s.monotonicMs >= advance_deadline then
      local ok; ok, err = stop(s, 'host_timeout'); if not ok then return nil, err end
    end
    if not s.halted then return nil end
    if chunk then
      local actual = progress(s)
      done = done + actual
      if actual < chunk.count and not reason then reason = 'native_interruption' end
      chunk = nil
    end
    if done == requested or reason then
      result = {status = done == requested and 'completed' or 'interrupted',
        unit = unit, requested = requested, count = done, state = 'frozen',
        generation = generation, stopped_at_ms = s.monotonicMs,
        stop_deadline_ms = stop_deadline}
      if done ~= requested then result.reason = reason end
      return copy(result)
    end
    local count = math.min(requested - done, unit == 'frames' and 30 or 5000)
    local ok = pcall(host.step, count,
      unit == 'frames' and host.stepType.ppuFrame or host.stepType.step)
    if not ok then return retire('native_step_failed') end
    local admitted; admitted, err = observe(); if not admitted then return nil, err end
    local target = unit == 'frames' and admitted.ppuRemaining or admitted.instructionRemaining
    if admitted.halted or not integer(target) or target < count or target % count ~= 0
      or (unit == 'instructions' and target ~= count)
      or admitted.ppuCycles ~= s.ppuCycles
      or admitted.instructionBoundaries ~= s.instructionBoundaries
      or admitted.haltedCpuSteps ~= s.haltedCpuSteps then
      return retire('invalid_native_step_admission')
    end
    chunk = {count = count, per_unit = target // count, origin = admitted}
    return nil
  end
  function self:is_retired() return failure ~= nil end
  function self:stop_deadline() return stop_deadline end
  return self
end
return M
