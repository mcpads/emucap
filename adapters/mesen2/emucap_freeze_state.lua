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

function M.can_start_recording(state)
  return type(state) == "table" and state.frame_boundary_proven == true
end

return M
