local M = {}

function M.halt(reason, frame_boundary_proven)
  assert(type(reason) == "string" and reason ~= "", "freeze reason must be a non-empty string")
  return {
    reason = reason,
    frame_boundary_proven = frame_boundary_proven == true,
  }
end

function M.after_step(unit)
  assert(unit == "frames" or unit == "instructions", "unsupported step unit")
  return M.halt("step", unit == "frames")
end

function M.can_start_recording(state, origin)
  if type(state) ~= "table" then return false end
  -- reset_release validates and binds its sinks before queuing reset, then establishes a new
  -- boundary in the native reset callback before the first post-reset guest tick. It therefore
  -- does not inherit or read from the current halt position. Other origins advance from the
  -- current position and still require a proven frame boundary.
  return origin == "reset_release" or origin == "state_load"
    or state.frame_boundary_proven == true
end

return M
