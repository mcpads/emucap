-- One bounded socket read and at most one complete frame per service callback.
-- TX backpressure does not prevent the session owner from reading control intent.
local M = {}
function M.new(cap)
  assert(math.type(cap) == 'integer' and cap > 0)
  return { cap = cap, bytes = '' }
end
function M.reset(rx) rx.bytes = '' end
function M.poll(rx, socket, control_cap)
  local cap = math.min(rx.cap, control_cap or rx.cap)
  local newline = rx.bytes:find('\n', 1, true)
  if not newline then
    if #rx.bytes > cap then return nil, 'too_large' end
    local data, err, partial = socket:receive(math.min(4096, cap + 1 - #rx.bytes))
    local incoming = data or partial or ''
    rx.bytes = rx.bytes .. incoming
    newline = rx.bytes:find('\n', 1, true)
    if not newline and #rx.bytes > cap then return nil, 'too_large' end
    -- EOF is ownership loss even if a partial final request was delivered.
    if not data and err and err ~= 'timeout' then return nil, err end
  end
  if not newline then return nil end
  if newline - 1 > cap then return nil, 'too_large' end
  local line = rx.bytes:sub(1, newline - 1)
  rx.bytes = rx.bytes:sub(newline + 1)
  return line
end
return M
