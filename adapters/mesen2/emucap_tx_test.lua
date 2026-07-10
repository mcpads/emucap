-- Production emucap_tx.lua의 cursor/상한 계약을 fake socket으로 검증한다.
-- Run: EMUCAP_ADAPTER_DIR=. lua emucap_tx_test.lua

local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Tx = require("emucap_tx")

local function eq(a, b, msg)
  if a ~= b then error(("FAIL %s: %s ~= %s"):format(msg, tostring(a), tostring(b))) end
end

local function scripted_socket(steps)
  local sock = { calls = 0, bytes = {} }
  function sock:send(data, first)
    self.calls = self.calls + 1
    local step = assert(steps[self.calls], "unexpected send call")
    eq(first, step.first, "send cursor " .. self.calls)
    if step.last and step.last >= first then
      self.bytes[#self.bytes + 1] = data:sub(first, step.last)
    end
    if step.ok then return step.last end
    return nil, step.err, step.last
  end
  return sock
end

-- 여러 partial send와 timeout을 지나도 정확히 한 NDJSON line만 전송한다.
do
  local tx = Tx.new(64)
  assert(Tx.enqueue(tx, "abcdef"))
  local sock = scripted_socket({
    { first = 1, last = 2, err = "timeout" },
    { first = 3, last = 5, err = "timeout" },
    { first = 6, last = 7, ok = true },
  })
  eq(Tx.flush(tx, sock), "pending", "partial 1")
  eq(Tx.remaining(tx), 5, "remaining after partial 1")
  eq(Tx.flush(tx, sock), "pending", "partial 2")
  eq(Tx.flush(tx, sock), "complete", "partial completion")
  eq(table.concat(sock.bytes), "abcdef\n", "no loss or duplicate")
  eq(Tx.pending(tx), false, "complete clears state")
end

-- 전송 진전 없는 timeout은 cursor를 그대로 보존한다.
do
  local tx = Tx.new(64)
  assert(Tx.enqueue(tx, "hello"))
  local sock = scripted_socket({ { first = 1, last = 0, err = "timeout" } })
  eq(Tx.flush(tx, sock), "pending", "zero-progress timeout")
  eq(Tx.remaining(tx), 6, "timeout preserves full line")
end

-- hard error는 미완성 line을 폐기해 새 연결에 partial NDJSON을 이어 붙이지 않는다.
do
  local tx = Tx.new(64)
  assert(Tx.enqueue(tx, "broken"))
  local sock = scripted_socket({ { first = 1, last = 3, err = "closed" } })
  local status, err = Tx.flush(tx, sock)
  eq(status, "error", "hard error status")
  eq(err, "closed", "hard error kind")
  eq(Tx.pending(tx), false, "hard error clears state")
  assert(Tx.enqueue(tx, "fresh"), "fresh connection can enqueue")
end

-- keepalive가 부분 전송된 동안 최종 응답을 enqueue해도 순서와 framing을 보존한다.
do
  local tx = Tx.new(64)
  assert(Tx.enqueue(tx, "working"))
  local sock = scripted_socket({
    { first = 1, last = 3, err = "timeout" },
    { first = 1, last = 15, ok = true },
  })
  eq(Tx.flush(tx, sock), "pending", "keepalive partial")
  assert(Tx.enqueue(tx, "completed"), "final response queued behind keepalive")
  eq(Tx.remaining(tx), 15, "sent prefix compacted before append")
  eq(Tx.flush(tx, sock), "complete", "queued lines complete")
  eq(table.concat(sock.bytes), "working\ncompleted\n", "queued NDJSON order")
end

-- cap은 이미 보낸 prefix가 아니라 아직 보내지 못한 누적 bytes에 적용한다.
do
  local tx = Tx.new(8)
  assert(Tx.enqueue(tx, "abc")) -- 4 bytes
  local sock = scripted_socket({ { first = 1, last = 2, err = "timeout" } })
  eq(Tx.flush(tx, sock), "pending", "cap partial")
  assert(Tx.enqueue(tx, "wxyz")) -- remaining 2 + 5 = 7 bytes
  local ok, err = Tx.enqueue(tx, "q") -- 7 + 2 > cap
  eq(ok, nil, "cumulative cap rejected")
  eq(err, "too_large", "cumulative cap error")

  Tx.reset(tx)
  ok, err = Tx.enqueue(tx, "12345678") -- 8 bytes + newline > cap
  eq(ok, nil, "oversized enqueue rejected")
  eq(err, "too_large", "oversized error")
end

print("ALL EMUCAP TX TESTS PASSED")
