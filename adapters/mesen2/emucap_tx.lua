local M = {}

local DEFAULT_CAP = 8 * 1024 * 1024

function M.new(cap)
  cap = tonumber(cap) or DEFAULT_CAP
  assert(cap >= 1, "emucap_tx: cap must be positive")
  return { cap = math.floor(cap), data = nil, next = 1 }
end

function M.reset(tx)
  tx.data = nil
  tx.next = 1
end

function M.pending(tx)
  return tx.data ~= nil
end

function M.remaining(tx)
  if not tx.data then return 0 end
  return #tx.data - tx.next + 1
end

function M.enqueue(tx, line)
  local data = tostring(line) .. "\n"
  if #data > tx.cap then return nil, "too_large" end
  if not tx.data then
    tx.data = data
    tx.next = 1
    return true
  end

  local remaining = M.remaining(tx)
  if remaining + #data > tx.cap then return nil, "too_large" end

  -- 이미 보낸 prefix를 버린 뒤 다음 완성 line을 붙인다. host는 요청을 직렬화하지만
  -- keepalive가 부분 전송된 동안 최종 응답이 생길 수 있으므로 단일 line 슬롯이면 안 된다.
  if tx.next > 1 then
    tx.data = tx.data:sub(tx.next)
    tx.next = 1
  end
  tx.data = tx.data .. data
  return true
end

function M.flush(tx, sock)
  if not tx.data then return "idle" end

  -- LuaSocket send(data, i)는 성공 시 마지막 전송 byte index를, 실패 시
  -- nil, error, 마지막 전송 byte index를 반환한다. slice를 새로 만들지 않고
  -- 원본 line과 절대 cursor를 유지해야 partial send 뒤 중복·손실이 없다.
  local last, err, partial = sock:send(tx.data, tx.next)
  local sent_through = last or partial
  if type(sent_through) == "number" and sent_through >= tx.next then
    tx.next = math.min(sent_through + 1, #tx.data + 1)
  end

  if not last and err and err ~= "timeout" then
    M.reset(tx)
    return "error", err
  end
  if tx.next > #tx.data then
    M.reset(tx)
    return "complete"
  end
  return "pending", err
end

return M
