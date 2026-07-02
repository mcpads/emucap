-- emucap-live.lua의 flush_deferred 회귀 테스트(스탠드얼론). `lua flush_deferred_test.lua`.
-- 브레이크포인트가 press/run_frames 도중 발화해 freeze되면 frozen 동안 tick_deferred가 안 돌아
-- deferred가 응답을 못 보내니, freeze 진입 시 flush한다.
-- 아래 flush_deferred는 emucap-live.lua의 사본 — 한쪽을 바꾸면 함께 갱신한다.

local deferred, input_hold, frame
local last_reply

local function reply_ok(id, result) last_reply = { id = id, result = result } end

local function flush_deferred(status, reason, bp_id)
  if not deferred then return end
  if deferred.kind == "press" then input_hold = nil end
  local r = { status = status, frame = frame }
  if reason then r.reason = reason end
  if bp_id then r.breakpoint_id = bp_id end
  reply_ok(deferred.id, r)
  deferred = nil
end

local function eq(a, b, msg)
  if a ~= b then error(("FAIL %s: %s ~= %s"):format(msg, tostring(a), tostring(b))) end
end

-- press 진행 중 freeze → interrupted 응답 + 버튼 해제 + deferred 정리 + reason/breakpoint_id
frame = 500
input_hold = { port = 0, tbl = {} }
deferred = { kind = "press", id = 42, remaining = 100, age = 3 }
last_reply = nil
flush_deferred("interrupted", "breakpoint", 7)
eq(last_reply.id, 42, "press 응답 id")
eq(last_reply.result.status, "interrupted", "press status")
eq(last_reply.result.frame, 500, "press frame")
eq(last_reply.result.reason, "breakpoint", "reason")
eq(last_reply.result.breakpoint_id, 7, "breakpoint_id")
eq(input_hold, nil, "버튼 해제됨")
eq(deferred, nil, "deferred 정리됨")

-- run_frames 진행 중 freeze → interrupted (input_hold 무관)
frame = 700
input_hold = nil
deferred = { kind = "run", id = 7, remaining = 50, age = 1 }
last_reply = nil
flush_deferred("interrupted")
eq(last_reply.id, 7, "run 응답 id")
eq(last_reply.result.status, "interrupted", "run status")
eq(deferred, nil, "run deferred 정리됨")

-- deferred 없으면 아무 일도 안 함(응답 안 보냄)
deferred = nil
last_reply = nil
flush_deferred("interrupted")
eq(last_reply, nil, "deferred 없으면 무동작")

print("ALL FLUSH_DEFERRED TESTS PASSED")
