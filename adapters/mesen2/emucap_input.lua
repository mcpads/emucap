local M = {}

-- Mesen setInput changes only keys present in its table. A replacement input
-- must explicitly clear every other supported button, including on release.
-- Readback proves the live device bits; a silent write to an absent device is
-- not a successful input operation.
function M.apply(host, valid, pressed, port)
  local state = {}
  for name in pairs(valid) do state[name] = false end
  for name, value in pairs(pressed) do
    if not valid[name] or type(value) ~= "boolean" then
      return false, "invalid native input button state"
    end
    state[name] = value
  end
  local ok, err = pcall(function()
    host.setInput(state, port)
    local observed = host.getInput(port)
    if type(observed) ~= "table" then error("native input readback unavailable") end
    for name, value in pairs(state) do
      if observed[name] ~= value then
        error("native input readback differs for " .. name)
      end
    end
  end)
  if not ok then return false, tostring(err) end
  return true
end

return M
