-- emucap-live.lua의 json_decode/parse_request 단위 테스트(스탠드얼론). `lua json_decode_test.lua`.
-- 아래 디코더는 emucap-live.lua의 사본 — 한쪽을 바꾸면 함께 갱신한다.
-- 범용 JSON 디코더(요청 줄 파싱용). 객체·배열·문자열·숫자·true/false/null.
local function json_decode(s)
  local i = 1
  local parse_value
  local function skip_ws()
    while i <= #s and s:sub(i, i):match("%s") do i = i + 1 end
  end
  local function parse_string()
    i = i + 1 -- 여는 따옴표
    local out = {}
    while i <= #s do
      local c = s:sub(i, i)
      if c == '"' then
        i = i + 1
        return table.concat(out)
      elseif c == '\\' then
        local n = s:sub(i + 1, i + 1)
        if n == 'u' then
          local cp = tonumber(s:sub(i + 2, i + 5), 16) or 0
          out[#out + 1] = utf8.char(cp)
          i = i + 6
        else
          local map = { ['"'] = '"', ['\\'] = '\\', ['/'] = '/', b = '\b', f = '\f', n = '\n', r = '\r', t = '\t' }
          out[#out + 1] = map[n] or n
          i = i + 2
        end
      else
        out[#out + 1] = c
        i = i + 1
      end
    end
    error("unterminated string")
  end
  local function parse_number()
    local j = i
    while i <= #s and s:sub(i, i):match("[%d%.eE%+%-]") do i = i + 1 end
    return tonumber(s:sub(j, i - 1))
  end
  local function parse_object()
    i = i + 1 -- {
    local obj = {}
    skip_ws()
    if s:sub(i, i) == '}' then i = i + 1; return obj end
    while true do
      skip_ws()
      local key = parse_string()
      skip_ws()
      i = i + 1 -- :
      obj[key] = parse_value()
      skip_ws()
      local c = s:sub(i, i)
      if c == ',' then i = i + 1
      elseif c == '}' then i = i + 1; return obj
      else error("expected , or } at " .. i) end
    end
  end
  local function parse_array()
    i = i + 1 -- [
    local arr = {}
    skip_ws()
    if s:sub(i, i) == ']' then i = i + 1; return arr end
    while true do
      arr[#arr + 1] = parse_value()
      skip_ws()
      local c = s:sub(i, i)
      if c == ',' then i = i + 1
      elseif c == ']' then i = i + 1; return arr
      else error("expected , or ] at " .. i) end
    end
  end
  parse_value = function()
    skip_ws()
    local c = s:sub(i, i)
    if c == '{' then return parse_object()
    elseif c == '[' then return parse_array()
    elseif c == '"' then return parse_string()
    elseif c == 't' then i = i + 4; return true
    elseif c == 'f' then i = i + 5; return false
    elseif c == 'n' then i = i + 4; return nil
    else return parse_number() end
  end
  return parse_value()
end

local function parse_request(line)
  local ok, env = pcall(json_decode, line)
  if not ok or type(env) ~= "table" then return nil, nil, {} end
  local p = env.params
  if type(p) ~= "table" then p = {} end
  return env.id, env.method, p
end

-- ── 테스트: 각 메서드의 실제 요청 줄 ──
local function eq(a, b, msg)
  if a ~= b then error(("FAIL %s: %s ~= %s"):format(msg, tostring(a), tostring(b))) end
end

-- read_memory
local id, m, p = parse_request('{"v":1,"id":5,"method":"read_memory","params":{"memory_type":"snesWorkRam","address":16,"length":64}}')
eq(id, 5, "id"); eq(m, "read_memory", "method")
eq(p.memory_type, "snesWorkRam", "mt"); eq(p.address, 16, "addr"); eq(p.length, 64, "len")

-- write_memory (hex)
id, m, p = parse_request('{"v":1,"id":6,"method":"write_memory","params":{"memory_type":"snesWorkRam","address":10,"hex":"aa"}}')
eq(p.hex, "aa", "hex"); eq(p.address, 10, "waddr")

-- set_breakpoint (start/end — end는 예약어 → p["end"])
id, m, p = parse_request('{"v":1,"id":7,"method":"set_breakpoint","params":{"kind":"write","memory_type":"snesMemory","start":0,"end":15,"pause_on_hit":true,"auto_savestate":false}}')
eq(p.kind, "write", "kind"); eq(p.start, 0, "start"); eq(p["end"], 15, "end")
eq(p.pause_on_hit, true, "poh"); eq(p.auto_savestate, false, "as")

-- clear_breakpoint (params.id vs envelope id — 중첩으로 구분)
id, m, p = parse_request('{"v":1,"id":99,"method":"clear_breakpoint","params":{"id":2}}')
eq(id, 99, "env id"); eq(p.id, 2, "bp id")

-- press_buttons (배열)
id, m, p = parse_request('{"v":1,"id":8,"method":"press_buttons","params":{"port":0,"buttons":["a","start"],"frames":30}}')
eq(p.frames, 30, "frames"); eq(#p.buttons, 2, "btn count"); eq(p.buttons[1], "a", "btn1"); eq(p.buttons[2], "start", "btn2")

-- run_frames
id, m, p = parse_request('{"v":1,"id":9,"method":"run_frames","params":{"n":120}}')
eq(p.n, 120, "n")

-- save_state (path)
id, m, p = parse_request('{"v":1,"id":10,"method":"save_state","params":{"path":"/tmp/base.mss"}}')
eq(p.path, "/tmp/base.mss", "path")

-- probe (의미 키 state/frame)
id, m, p = parse_request('{"v":1,"id":11,"method":"probe","params":{"state":"/tmp/b.mss","frame":600,"memory_type":"snesWorkRam","address":0,"length":16}}')
eq(p.state, "/tmp/b.mss", "state"); eq(p.frame, 600, "frame"); eq(p.length, 16, "plen")

print("ALL JSON DECODE TESTS PASSED")
