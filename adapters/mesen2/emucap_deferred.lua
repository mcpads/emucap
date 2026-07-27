local M = {}

local VALID_KIND = {
  run = true,
  press = true,
  probe = true,
}

local function integer(value, name, minimum)
  assert(type(value) == "number" and value == math.floor(value),
    "emucap_deferred: " .. name .. " must be an integer")
  assert(value >= minimum,
    "emucap_deferred: " .. name .. " must be >= " .. tostring(minimum))
end

local function validate(state)
  if state == nil then return end
  assert(type(state) == "table", "emucap_deferred: state must be a table or nil")
  integer(state.session, "session", 1)
  integer(state.id, "request id", 0)
  assert(VALID_KIND[state.kind], "emucap_deferred: unknown operation kind")
  integer(state.remaining, "remaining", 1)
  integer(state.age, "age", 0)
end

local function copy_with_progress(state, remaining, age)
  local next_state = {}
  for key, value in pairs(state) do next_state[key] = value end
  next_state.remaining = remaining
  next_state.age = age
  return next_state
end

local function effect(state, kind, status, frame, reason, breakpoint_id)
  return {
    kind = kind,
    session = state.session,
    id = state.id,
    operation_kind = state.kind,
    status = status,
    frame = frame,
    reason = reason,
    breakpoint_id = breakpoint_id,
    probe = state.probe,
    release_input = state.kind == "press" and kind ~= "working",
  }
end

function M.start(session, id, kind, remaining, probe)
  local state = {
    session = session,
    id = id,
    kind = kind,
    remaining = remaining,
    age = 0,
    probe = probe,
  }
  validate(state)
  return state
end

function M.tick(state, frame, keepalive_frames)
  if state == nil then return nil, nil end
  validate(state)
  integer(frame, "frame", 0)
  integer(keepalive_frames, "keepalive interval", 1)

  local remaining = state.remaining - 1
  local age = state.age + 1
  if remaining == 0 then
    return nil, effect(state, "terminal", "completed", frame)
  end

  local next_state = copy_with_progress(state, remaining, age)
  if age % keepalive_frames == 0 then
    return next_state, effect(next_state, "working", "working", frame)
  end
  return next_state, nil
end

function M.interrupt(state, status, reason, breakpoint_id, frame)
  if state == nil then return nil, nil end
  validate(state)
  assert(type(status) == "string" and status ~= "",
    "emucap_deferred: interrupt status must be non-empty")
  integer(frame, "frame", 0)
  if breakpoint_id ~= nil then integer(breakpoint_id, "breakpoint id", 0) end
  return nil, effect(state, "terminal", status, frame, reason, breakpoint_id)
end

function M.cancel(state)
  if state == nil then return nil, nil end
  validate(state)
  return nil, effect(state, "cancelled")
end

-- The transition is pure. This routine owns the observable effect order so production and tests
-- cannot disagree about releasing a transient press before a terminal response is enqueued.
function M.apply(effect_value, handlers)
  if effect_value == nil then return end
  assert(type(handlers) == "table", "emucap_deferred: handlers must be a table")
  if effect_value.release_input then
    assert(type(handlers.release_input) == "function",
      "emucap_deferred: release_input handler missing")
    handlers.release_input(effect_value)
  end
  local handler = handlers[effect_value.kind]
  if handler then handler(effect_value) end
end

return M
