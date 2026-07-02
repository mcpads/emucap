-- emucap-live.lua의 esc()(JSON 문자열 이스케이프) 단위 테스트. `lua esc_test.lua`.
-- 아래 esc는 emucap-live.lua의 사본 — 한쪽을 바꾸면 함께 갱신한다.
local ESC_MAP = {
  ['"'] = '\\"', ['\\'] = '\\\\',
  ['\b'] = '\\b', ['\f'] = '\\f', ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t',
}
local function esc(s)
  return (s:gsub('[%c"\\]', function(c)
    return ESC_MAP[c] or string.format('\\u%04x', string.byte(c))
  end))
end

local function eq(a, b, msg)
  if a ~= b then error(("FAIL %s: %q ~= %q"):format(msg, a, b)) end
end

eq(esc('a"b'), 'a\\"b', 'quote')
eq(esc('a\\b'), 'a\\\\b', 'backslash')
eq(esc('a\nb'), 'a\\nb', 'newline')
eq(esc('a\tb'), 'a\\tb', 'tab')           -- 미이스케이프면 invalid JSON
eq(esc('a\rb'), 'a\\rb', 'cr')            -- 미이스케이프면 invalid JSON
eq(esc('a\bb'), 'a\\bb', 'backspace')
eq(esc('a\fb'), 'a\\fb', 'formfeed')
eq(esc('a\0b'), 'a\\u0000b', 'null')      -- 기타 제어문자는 \u00XX
eq(esc('a' .. string.char(1) .. 'b'), 'a\\u0001b', 'soh')
eq(esc('a' .. string.char(31) .. 'b'), 'a\\u001fb', 'unit-sep')
eq(esc('plain text'), 'plain text', 'plain')
print('ALL ESC TESTS PASSED')
