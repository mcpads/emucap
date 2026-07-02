-- emucap-live.lua write_memory의 hex→bytes 파싱(emu.write 제외 순수 로직) 단위 테스트.
-- `lua write_hex_test.lua`. 한쪽을 바꾸면 emucap-live.lua write_memory도 함께 갱신한다.
local function parse_write_hex(hex)
  if type(hex) ~= "string" or #hex % 2 ~= 0 then return nil, "bad_params" end
  local bytes = {}
  for i = 1, #hex, 2 do
    local byte = tonumber(hex:sub(i, i + 1), 16)
    if not byte then return nil, "bad_params" end
    bytes[#bytes + 1] = byte
  end
  return bytes
end

local function eq(a, b, msg)
  if a ~= b then error(("FAIL %s: %s ~= %s"):format(msg, tostring(a), tostring(b))) end
end

-- 정상 짝수 hex
local b = parse_write_hex("deadbeef")
eq(#b, 4, "len"); eq(b[1], 0xde, "b1"); eq(b[4], 0xef, "b4")
-- 홀수 길이 hex는 거부
eq(parse_write_hex("abc"), nil, "홀수 거부")
-- 비-hex 거부
eq(parse_write_hex("zz"), nil, "비-hex 거부")
-- 빈 문자열은 0바이트(짝수)
eq(#parse_write_hex(""), 0, "빈 hex")
print("ALL WRITE-HEX TESTS PASSED")
