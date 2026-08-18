local M = {}

local VALID_UNIT = {
  frames = true,
  instructions = true,
}

local function integer(value, name, minimum)
  assert(type(value) == "number" and value == math.floor(value),
    "emucap_step: " .. name .. " must be an integer")
  assert(value >= minimum,
    "emucap_step: " .. name .. " must be >= " .. tostring(minimum))
end

local function parse_count(params, field, max_count)
  local value = params[field]
  if value == nil then value = 1 end
  if type(value) ~= "number" or value ~= math.floor(value) then
    return nil, "step count must be an integer"
  end
  if value < 1 then return nil, "step count must be at least 1" end
  if value > max_count then
    return nil, string.format(
      "step count %s exceeds synchronous limit %d; split the request and verify each terminal response",
      tostring(value), max_count)
  end
  return value
end

function M.parse_wire_step(method, params, max_count)
  assert(type(params) == "table", "emucap_step: params must be a table")
  integer(max_count, "maximum count", 1)

  if method == "step_instructions" then
    if params.unit ~= nil and params.unit ~= "instructions" then
      return nil, nil, "step_instructions accepts only the instructions unit"
    end
    if params.frames ~= nil then
      return nil, nil, "step_instructions accepts count, not frames"
    end
    local count, err = parse_count(params, "count", max_count)
    if not count then return nil, nil, err end
    return "instructions", count
  end

  if method ~= "step" then return nil, nil, "unsupported step method" end
  local unit = params.unit or "frames"
  if not VALID_UNIT[unit] then
    return nil, nil, "step unit must be frames or instructions"
  end
  if params.count ~= nil and params.frames ~= nil then
    return nil, nil, "step accepts either count or frames, not both"
  end
  local field = params.count ~= nil and "count" or "frames"
  local count, err = parse_count(params, field, max_count)
  if not count then return nil, nil, err end
  return unit, count
end

local function validate(state)
  if state == nil then return end
  assert(type(state) == "table", "emucap_step: state must be a table or nil")
  integer(state.session, "session", 1)
  integer(state.id, "request id", 0)
  assert(VALID_UNIT[state.unit], "emucap_step: unsupported unit")
  integer(state.requested, "requested count", 1)
  integer(state.remaining, "remaining count", 0)
  assert(state.remaining <= state.requested,
    "emucap_step: remaining count exceeds requested count")
end

local function terminal(state, status, frame, reason, breakpoint_id)
  local result = {
    status = status,
    unit = state.unit,
    requested = state.requested,
    frame = frame,
    state = "frozen",
  }
  if status == "completed" then result.count = state.requested end
  if reason ~= nil then result.reason = reason end
  if breakpoint_id ~= nil then result.breakpoint_id = breakpoint_id end
  return {
    session = state.session,
    id = state.id,
    result = result,
  }
end

function M.start(session, id, unit, count)
  local state = {
    session = session,
    id = id,
    unit = unit,
    requested = count,
    remaining = count,
  }
  validate(state)
  return state
end

function M.active(state)
  validate(state)
  return state ~= nil
end

function M.take_chunk(state, frame_limit, instruction_limit)
  validate(state)
  assert(state ~= nil, "emucap_step: cannot take a chunk without an active step")
  integer(frame_limit, "frame chunk limit", 1)
  integer(instruction_limit, "instruction chunk limit", 1)
  assert(state.remaining > 0, "emucap_step: completed step has no next chunk")
  local limit = state.unit == "instructions" and instruction_limit or frame_limit
  local chunk = math.min(state.remaining, limit)
  local next_state = {}
  for key, value in pairs(state) do next_state[key] = value end
  next_state.remaining = state.remaining - chunk
  return next_state, chunk
end

function M.complete(state, frame)
  validate(state)
  if state == nil or state.remaining > 0 then return state, nil end
  integer(frame, "frame", 0)
  return nil, terminal(state, "completed", frame)
end

function M.interrupt(state, reason, breakpoint_id, frame)
  validate(state)
  if state == nil then return nil, nil end
  assert(type(reason) == "string" and reason ~= "",
    "emucap_step: interrupt reason must be non-empty")
  if breakpoint_id ~= nil then integer(breakpoint_id, "breakpoint id", 0) end
  integer(frame, "frame", 0)
  return nil, terminal(state, "interrupted", frame, reason, breakpoint_id)
end

function M.cancel(state)
  validate(state)
  return nil
end

return M
