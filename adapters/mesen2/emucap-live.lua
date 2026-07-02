-- emucap Mesen2 라이브 클라이언트 (능동 제어)
-- 필요 옵션: "Allow network access" + "Allow access to I/O and OS functions".
-- 먼저 emucap-mcp 서버가 떠 있어야 한다(기본 포트 47800).
--
-- freeze는 breakExecution + codeBreak로 건다(spin 무한대기는 1초 워치독으로 불가).
-- 이 파일은 상태기계·freeze-step·읽기·쓰기·입력을 다룬다. 지연명령·세이브스테이트·
-- 브레이크포인트는 별도로 얹는다.

local socket = require("socket.core")

local HOST = "127.0.0.1"
-- 포트: 교차-ROM 2-인스턴스를 위해 EMUCAP_PORT로 덮어쓸 수 있다(없으면 47800).
local PORT = tonumber(os.getenv("EMUCAP_PORT") or "") or 47800
local PROTOCOL_VERSION = 1
-- 데드맨: 명령 없이 이만큼 지나면 자동 resume(에이전트 死 시 무한 frozen 방지). 휴먼-인-루프로 길게
-- 들여다볼 땐 짧을 수 있어 env로 조정(EMUCAP_DEADMAN_MS). hotkey freeze는 데드맨 면제(무기한 hold).
local MAX_FREEZE_MS = tonumber(os.getenv("EMUCAP_DEADMAN_MS") or "") or 30000
local FREEZE_BUDGET_MS = 800  -- 워치독(1초) 마진: 이 안에서 codeBreak 재무장
-- freeze 중 연결끊김(서버 재시작//mcp 재연결) 시 freeze를 유지한 채 재접속을 시도해 장면을 보존한다
-- (즉시 resume하면 공들인 장면을 흘려버린다). 서버가 영영 안 오면 무한 frozen 방지로 이만큼 후 resume.
-- 재연결 지연·다중 재연결을 감안해 30s는 너무 짧다 → 넉넉히 10분 기본, env로 조정.
-- 0이면 giveup 없음(재접속될 때까지 무기한 freeze 유지).
local RECONNECT_GIVEUP_MS = tonumber(os.getenv("EMUCAP_RECONNECT_GIVEUP_MS") or "") or 600000
local freeze_disc_ms = nil    -- freeze 중 연결끊김 시작 시각(재접속 giveup 타이머)
local last_reconnect_ms = 0   -- 마지막 재접속 시도 시각(throttle — 매 명령 connect 폭주 방지)
-- 로컬 freeze 핫키: 사용자가 GUI Pause 대신 이 호스트 키를 누르면 그 자리에서 emucap freeze가
-- 걸린다. GUI Pause는 에뮬 스레드를 통째로 멈춰 모든 Lua 콜백(startFrame·codeBreak)이 정지 →
-- emucap이 응답불가(연결 끊김처럼 보임)가 되고 GUI resume 전엔 자동 복구도 안 된다. 이 핫키는
-- emucap의 codeBreak freeze라 얼린 채 read_memory/screenshot/get_state/step이 모두 동작한다 —
-- transient 스프라이트 팝업의 정확한 프레임을 잡아 OAM/VRAM을 검사하는 워크플로용(같은 키로 토글
-- resume). EMUCAP_FREEZE_KEY로 키 이름 변경(기본 Home), "off"/"none"/""이면 비활성. 유효 키 이름은
-- F1~F12·단일 영문자·Home/End/Insert·Space/Enter/Esc 등.
local FREEZE_KEY = os.getenv("EMUCAP_FREEZE_KEY")
if FREEZE_KEY == nil then FREEZE_KEY = "Home" end
do local lk = FREEZE_KEY:lower(); if lk == "" or lk == "off" or lk == "none" then FREEZE_KEY = nil end end
local freeze_key_ok = true     -- isKeyPressed가 무효 키에 에러 → 1회 보고 후 비활성
local prev_freeze_key = false  -- 라이징 에지 검출(running→freeze, frozen→resume 토글)

local STATE = "running"       -- "running" | "frozen"
local freeze_start_ms = nil   -- 마지막 명령 이후 경과 측정(데드맨). frozen 진입/명령 수신 시 갱신.
local freeze_reason = "paused"
local pending_step_id = nil   -- step(n) 완료 응답 대기
local step_remaining = 0       -- step(n)의 남은 단위(청크로 나눠 진행)
local step_unit = "frames"    -- "frames"(ppuFrame) | "instructions"(stepType.step)
local STEP_CHUNK = 30          -- 프레임 step 청크(≤1s, keepalive 보장)
local INSTR_CHUNK = 20000      -- 명령 step 청크(≤1s 안에서 keepalive)
local conn = nil
local rx_buf = ""
local frame = 0

-- 브레이크포인트/이벤트 상태
local KEEPALIVE_FRAMES = 30
local deferred = nil           -- run_frames/press_buttons 진행 상태 { id, kind, remaining, age }
local pending_io = nil         -- save_state/load_state 진행 상태 { id, kind, path, ref }
local next_bp_id = 1
local breakpoints = {}         -- id -> { ref, kind, start, end, pause_on_hit, auto_savestate }
local reset_bp = nil           -- break_on_reset: 리셋 핸들러 exec BP { ref, handler }
local EVENT_CAP = 256
local events = {}              -- poll_events로 드레인
local dropped = 0              -- 큐 상한 초과로 버린 이벤트 수
local CPU = nil                -- emu.cpuType.snes (로드 시 설정)

-- 실행추적(콜스택 + 트레이스): set_trace로 켜면 매 명령 exec 콜백이 (a) 콜스택을 shadow-track
-- (JSR/JSL push, RTS/RTL/RTI pop — 스택 손상에도 robust), (b) 최근 명령 링버퍼를 채운다. 매 명령이라 느리니
-- 크래시 추적 hunting 전용. Mesen Lua엔 네이티브 콜스택 API가 없어 직접 추적한다.
local trace_on = false
local trace_ref = nil
local TRACE_CAP = 256
local trace_ring = {}          -- 링버퍼 슬롯 -> { pc, op }
local trace_idx = 0            -- 누적 명령 수(슬롯 = (trace_idx-1)%CAP +1)
local callstack = {}           -- shadow 콜스택: 호출지(JSR/JSL의 pc) 리스트(바깥→안)

-- ── base64 (순수 Lua) ────────────────────────────────────────
local B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
local function base64(data)
  return ((data:gsub(".", function(x)
    local r, b = "", x:byte()
    for i = 8, 1, -1 do r = r .. (b % 2 ^ i - b % 2 ^ (i - 1) > 0 and "1" or "0") end
    return r
  end) .. "0000"):gsub("%d%d%d?%d?%d?%d?", function(x)
    if #x < 6 then return "" end
    local c = 0
    for i = 1, 6 do c = c + (x:sub(i, i) == "1" and 2 ^ (6 - i) or 0) end
    return B64:sub(c + 1, c + 1)
  end) .. ({ "", "==", "=" })[#data % 3 + 1])
end

-- ── JSON ─────────────────────────────────────────────────────
local ESC_MAP = {
  ['"'] = '\\"', ['\\'] = '\\\\',
  ['\b'] = '\\b', ['\f'] = '\\f', ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t',
}
-- JSON 문자열 이스케이프: 따옴표·역슬래시와 모든 제어문자(0x00-0x1F)를 처리한다.
-- 제어문자(\r·\t 등)를 빠뜨리면 invalid JSON이 된다.
local function esc(s)
  return (s:gsub('[%c"\\]', function(c)
    return ESC_MAP[c] or string.format('\\u%04x', string.byte(c))
  end))
end

-- 빈 배열도 []로 직렬화하려면 마커가 필요하다(빈 Lua 테이블은 배열/객체 구분 불가).
-- 항상 배열인 필드(events·breakpoints 등)는 as_array로 감싼다.
local ARRAY_MT = {}
local function as_array(t) return setmetatable(t or {}, ARRAY_MT) end

local function jvalue(v)
  local t = type(v)
  if t == "number" then
    return (v == math.floor(v)) and string.format("%d", v) or tostring(v)
  elseif t == "boolean" then
    return tostring(v)
  elseif t == "table" then
    -- dense 정수키(1..n) 또는 as_array 마커 → JSON 배열. 그 외는 객체.
    local n = #v
    local is_arr
    if getmetatable(v) == ARRAY_MT then
      is_arr = true
    else
      is_arr = n > 0
      if is_arr then
        local count = 0
        for _ in pairs(v) do count = count + 1 end
        is_arr = (count == n)
      end
    end
    if is_arr then
      local parts = {}
      for i = 1, n do parts[i] = jvalue(v[i]) end
      return "[" .. table.concat(parts, ",") .. "]"
    end
    local parts = {}
    for k, val in pairs(v) do
      parts[#parts + 1] = '"' .. esc(tostring(k)) .. '":' .. jvalue(val)
    end
    return "{" .. table.concat(parts, ",") .. "}"
  else
    return '"' .. esc(tostring(v)) .. '"'
  end
end

-- 범용 JSON 디코더(요청 줄 파싱). 객체·배열·문자열·숫자·true/false/null. 키마다 정규식을
-- 하드코딩하지 않으므로 새 파라미터가 중앙 파서를 건드리지 않는다.
local function json_decode(s)
  local i = 1
  local parse_value
  local function skip_ws()
    while i <= #s and s:sub(i, i):match("%s") do i = i + 1 end
  end
  local function parse_string()
    i = i + 1
    local out = {}
    while i <= #s do
      local c = s:sub(i, i)
      if c == '"' then
        i = i + 1
        return table.concat(out)
      elseif c == '\\' then
        local n = s:sub(i + 1, i + 1)
        if n == 'u' then
          out[#out + 1] = utf8.char(tonumber(s:sub(i + 2, i + 5), 16) or 0)
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
    i = i + 1
    local obj = {}
    skip_ws()
    if s:sub(i, i) == '}' then i = i + 1; return obj end
    while true do
      skip_ws()
      local key = parse_string()
      skip_ws()
      i = i + 1 -- ':'
      obj[key] = parse_value()
      skip_ws()
      local c = s:sub(i, i)
      if c == ',' then i = i + 1
      elseif c == '}' then i = i + 1; return obj
      else error("expected , or }") end
    end
  end
  local function parse_array()
    i = i + 1
    local arr = {}
    skip_ws()
    if s:sub(i, i) == ']' then i = i + 1; return arr end
    while true do
      arr[#arr + 1] = parse_value()
      skip_ws()
      local c = s:sub(i, i)
      if c == ',' then i = i + 1
      elseif c == ']' then i = i + 1; return arr
      else error("expected , or ]") end
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

-- 요청 파싱: 봉투를 통째로 디코드해 params를 일반 테이블로 돌려준다. 중첩 덕에 envelope id와
-- params.id가 자연히 구분된다.
local function parse_request(line)
  local ok, env = pcall(json_decode, line)
  if not ok or type(env) ~= "table" then return nil, nil, {} end
  local p = type(env.params) == "table" and env.params or {}
  return env.id, env.method, p
end

-- ── 소켓 ─────────────────────────────────────────────────────
local function connect()
  local c = socket.tcp()
  c:settimeout(0)
  c:connect(HOST, PORT)
  conn = c
  rx_buf = ""
end

local function send_line(s)
  if not conn then return end
  -- 응답 전송은 블로킹으로 한다 — 논블로킹(settimeout 0) 부분 전송은 나머지 바이트(개행
  -- 포함)를 버려 큰 reply(screenshot·get_state·dump_memory)의 NDJSON 프레이밍을 깨뜨린다.
  -- 블로킹 send는 전량 전송을 보장하거나 에러를 낸다. 끝나면 수신용 논블로킹으로 복귀.
  conn:settimeout(nil)
  local ok, err = conn:send(s .. "\n")
  conn:settimeout(0)
  if not ok and err ~= "timeout" then conn = nil end
end

local function poll_line()
  if not conn then return nil end
  local line, err, partial = conn:receive("*l", rx_buf)
  if line then
    rx_buf = ""
    return line
  elseif err == "timeout" then
    rx_buf = partial or ""
    return nil
  else
    conn = nil
    return nil
  end
end

-- 호스트 freeze 핫키 상태. 무효 키 이름은 isKeyPressed가 에러를 던지므로 pcall로 감싸고,
-- 한 번 실패하면 기능을 끈다(emucap.lua와 동일 패턴). 키 미설정이면 항상 false.
local function freeze_key_down()
  if not (FREEZE_KEY and freeze_key_ok) then return false end
  local ok, pressed = pcall(emu.isKeyPressed, FREEZE_KEY)
  if not ok then
    freeze_key_ok = false
    emu.log("[emucap] freeze 핫키 '" .. tostring(FREEZE_KEY) .. "' 무효 — 비활성(EMUCAP_FREEZE_KEY로 변경)")
    return false
  end
  return pressed and true or false
end

local function reply_ok(id, result)
  send_line(string.format('{"id":%d,"ok":true,"result":%s}', id, jvalue(result)))
end

local function reply_err(id, kind, msg)
  send_line(string.format(
    '{"id":%d,"ok":false,"error":{"kind":"%s","message":"%s"}}',
    id, kind, esc(tostring(msg))))
end

-- ── 입력 ─────────────────────────────────────────────────────
local input_hold = nil   -- { port=, tbl={a=true,...} }

-- Mesen emu.setInput은 소문자 키만 인식한다(대문자/오타는 에러 없이 무시됨).
local VALID_BUTTONS = {
  a = true, b = true, x = true, y = true, l = true, r = true,
  start = true, select = true, up = true, down = true, left = true, right = true,
}
local BUTTON_ALIASES = {
  enter = "start", ["return"] = "start",
  l1 = "l", r1 = "r", lb = "l", rb = "r",
}
local function buttons_to_table(buttons)
  local t = {}
  local unknown = {}
  for _, b in ipairs(buttons or {}) do
    local raw = tostring(b)
    local lb = raw:lower()              -- "A" → "a" 정규화
    lb = BUTTON_ALIASES[lb] or lb
    if VALID_BUTTONS[lb] then
      t[lb] = true
    else
      unknown[#unknown + 1] = raw
    end
  end
  if #unknown > 0 then
    return nil, "unknown SNES button(s): " .. table.concat(unknown, ", ")
  end
  return t
end

-- emu.setInput(input, port, subPort) — 입력 테이블이 첫 인자. inputPolled 콜백에서
-- 적용해야 ROM이 읽기 전에 반영된다(문서 명시). startFrame 적용은 물리 폴링에 덮인다.
local function apply_input()
  if not input_hold then return true end
  local ok, err = pcall(emu.setInput, input_hold.tbl, input_hold.port)
  if not ok then
    pcall(emu.log, "emucap: setInput failed: " .. tostring(err))
    return false, tostring(err)
  end
  return true
end

-- ── 핸들러: (ok=true, result) 또는 (false, kind, msg) ─────────
local handlers = {}

function handlers.hello()
  local result = {
    protocol_version = PROTOCOL_VERSION,
    system = "snes",
    adapter = "mesen2-live",
    build = os.getenv("EMUCAP_BUILD_HASH") or "unknown",  -- launch가 넘긴 emucap git hash(status.emulator_build)
    methods = { "read_memory", "screenshot", "get_state", "get_rom_info", "status",
                "write_memory", "set_input", "pause", "step", "resume",
                "run_frames", "press_buttons", "save_state", "load_state",
                "set_breakpoint", "watch_register", "clear_breakpoint", "list_breakpoints",
                "clear_all_breakpoints", "poll_events", "set_trace", "get_trace", "call_stack",
                "break_on_reset", "dump_memory", "find_pattern", "probe", "reset", "disassemble" },
  }
  -- memory_types: read_memory가 받는 memory_type = emu.memType의 키 전체. 정적 추측이 아니라 Mesen
  -- API의 실제 메모리 타입을 런타임 열거해 완전·정확하게 advertise한다(능력 발견). MCP가
  -- status.memory_types로 표면화. emu.memType이 없으면 생략(graceful — MCP가 빈 목록 처리).
  if emu and emu.memType then
    local mtypes = {}
    for k, _ in pairs(emu.memType) do mtypes[#mtypes + 1] = k end
    table.sort(mtypes)
    result.memory_types = mtypes
  end
  local name = os.getenv("EMUCAP_NAME")
  if name then result.name = name end
  local token = os.getenv("EMUCAP_SESSION_TOKEN")
  if token then result.session_token = token end
  local content = os.getenv("EMUCAP_CONTENT")
  if content then result.content = content end
  return true, result
end

function handlers.read_memory(p)
  local mt = emu.memType[p.memory_type] or p.memory_type
  local out = {}
  for i = 0, (p.length - 1) do
    out[#out + 1] = string.format("%02x", emu.read(p.address + i, mt, false))
  end
  return true, { hex = table.concat(out) }
end

function handlers.write_memory(p)
  local mt = emu.memType[p.memory_type] or p.memory_type
  local hex = p.hex
  -- 홀수 길이 hex는 마지막 single nibble을 한 바이트로 써넣는 조용한 오류를 낸다 — 거부한다.
  if type(hex) ~= "string" or #hex % 2 ~= 0 then
    return false, "bad_params", "hex는 짝수 길이 hex 문자열이어야"
  end
  local n = 0
  for i = 1, #hex, 2 do
    local byte = tonumber(hex:sub(i, i + 1), 16)
    if not byte then return false, "bad_params", "hex 디코드 실패" end
    emu.write(p.address + n, byte, mt)
    n = n + 1
  end
  return true, { written = n }
end

function handlers.set_input(p)
  local tbl, err = buttons_to_table(p.buttons)
  if not tbl then return false, "bad_params", err end
  input_hold = { port = p.port or 0, tbl = tbl }
  local ok, apply_err = apply_input()
  if not ok then return false, "emulator_error", "emu.setInput failed: " .. tostring(apply_err) end
  return true, { applied = true }
end

function handlers.screenshot()
  return true, { png_base64 = base64(emu.takeScreenshot()) }
end

-- get_state는 전 상태(레지스터·DMA·PPU·SPC 등 수백 필드)를 돌려준다. groups를 주면 키의
-- 그룹 prefix(첫 "." 앞)로 걸러 토큰 비용을 줄인다. 예: groups=["cpu","ppu"].
function handlers.get_state(p)
  local st = emu.getState()
  if not (p.groups and #p.groups > 0) then
    return true, { state = st }
  end
  local want = {}
  for _, g in ipairs(p.groups) do want[g] = true end
  local out = {}
  for k, v in pairs(st) do
    local grp = k:match("^([^.%[]+)")   -- 첫 "." 또는 "[" 앞
    if grp and want[grp] then out[k] = v end
  end
  return true, { state = out }
end

function handlers.get_rom_info()
  local info = emu.getRomInfo()
  return true, { name = info.name, path = info.path, sha1 = info.fileSha1Hash }
end

function handlers.status()
  local r = { connected = true, frame = frame, state = STATE }
  if STATE == "frozen" then r.reason = freeze_reason end   -- "hotkey"면 사용자가 로컬 핫키로 얼림
  -- 핫키 진단(Home "가끔 안 됨" 분간): freeze_key=키명, armed=무장여부, down=지금 눌림 감지중.
  -- Home을 눌렀는데 down=false면 창 포커스/키명 문제(로직 아님), down=true인데 freeze 안 되면 로직 버그.
  r.freeze_key = FREEZE_KEY or "off"
  r.freeze_key_armed = (FREEZE_KEY ~= nil) and freeze_key_ok
  r.freeze_key_down = freeze_key_down()
  return true, r
end

-- 게임을 리셋한다(리셋 버튼 없으면 전원 재투입과 동일). 로드된 ROM 바이트는 그대로이므로
-- "처음부터 다시"엔 쓰되, 리빌드한 ROM 검증은 Mesen의 "Reload ROM" 단축키를 쓴다(Lua 미노출).
function handlers.reset()
  emu.reset()
  return true, { reset = true }
end

-- 타깃 메모리를 hex로 읽는다(probe 완료 시 사용).
local function read_target(pr)
  local mt = emu.memType[pr.mem] or pr.mem
  local out = {}
  for i = 0, (pr.len - 1) do
    out[#out + 1] = string.format("%02x", emu.read(pr.addr + i, mt, false))
  end
  return table.concat(out)
end

-- ── 지연 명령 (run_frames / press_buttons / probe): 프레임마다 진행, 끝나면 응답 ──
local function tick_deferred()
  deferred.remaining = deferred.remaining - 1
  deferred.age = deferred.age + 1
  if deferred.remaining <= 0 then
    if deferred.kind == "press" then input_hold = nil end   -- 버튼 해제
    if deferred.kind == "probe" then
      reply_ok(deferred.id, { hex = read_target(deferred.probe), frame = frame })
    else
      reply_ok(deferred.id, { status = "completed", frame = frame })
    end
    deferred = nil
  elseif deferred.age % KEEPALIVE_FRAMES == 0 then
    send_line(string.format('{"id":%d,"ok":true,"result":{"status":"working"}}', deferred.id))
  end
end

-- 백스톱 B2: freeze(브레이크포인트 등)가 진행 중 지연 명령(press/run_frames/probe)을 가로채면
-- frozen 동안 tick_deferred가 안 돌아 응답이 막힌다. freeze 진입 시 여기서 마무리해 클라이언트
-- 타임아웃을 막는다. press면 버튼을 뗀다.
local function flush_deferred(status, reason, bp_id)
  if not deferred then return end
  if deferred.kind == "press" then input_hold = nil end
  local r = { status = status, frame = frame }
  if reason then r.reason = reason end
  if bp_id then r.breakpoint_id = bp_id end
  reply_ok(deferred.id, r)
  deferred = nil
end

-- ── 세이브스테이트 (createSavestate/loadSavestate는 exec 콜백 컨텍스트 필요) ──
local IO_LO, IO_HI = 0, 0xFFFFFF
local function on_io_exec()
  if not pending_io then return end
  local op = pending_io
  pending_io = nil
  emu.removeMemoryCallback(op.ref, emu.callbackType.exec, IO_LO, IO_HI, CPU)
  -- probe: load → (F프레임 지연) → read. 원자적이라 load와 read 사이 외부 개입이 없어
  -- 결정론적이다(loadSavestate가 상태를 정확 복원하므로 주입 시점은 무관).
  if op.kind == "probe" then
    local ok, err = pcall(function()
      local f = assert(io.open(op.path, "rb")); local data = f:read("*a"); f:close()
      emu.loadSavestate(data)
    end)
    if not ok then reply_err(op.id, "io_error", err); return end
    if op.probe.frame <= 0 then
      reply_ok(op.id, { hex = read_target(op.probe), frame = frame })
    else
      deferred = { id = op.id, kind = "probe", remaining = op.probe.frame, age = 0, probe = op.probe }
    end
    return
  end
  local ok, err = pcall(function()
    if op.kind == "save" then
      local data = emu.createSavestate()
      local f = assert(io.open(op.path, "wb")); f:write(data); f:close()
    else
      local f = assert(io.open(op.path, "rb")); local data = f:read("*a"); f:close()
      emu.loadSavestate(data)
    end
  end)
  if ok then reply_ok(op.id, { status = "completed", path = op.path })
  else reply_err(op.id, "io_error", err) end
end

local function arm_io(kind, id, path)
  if not path then reply_err(id, "bad_params", "path 필요"); return end
  pending_io = { kind = kind, id = id, path = path }
  pending_io.ref = emu.addMemoryCallback(on_io_exec, emu.callbackType.exec, IO_LO, IO_HI, CPU)
end

-- probe: 베이스 복귀 → frame프레임 전진 → 타깃 읽기, 한 명령에서 원자적으로(결정론적).
-- 의미 키: state(베이스 세이브스테이트), frame(프레임), memory_type/address/length(타깃).
local function arm_probe(id, p)
  if not p.state then reply_err(id, "bad_params", "state 필요"); return end
  pending_io = {
    kind = "probe", id = id, path = p.state,
    probe = { frame = p.frame or 0, mem = p.memory_type, addr = p.address or 0, len = p.length or 1 },
  }
  pending_io.ref = emu.addMemoryCallback(on_io_exec, emu.callbackType.exec, IO_LO, IO_HI, CPU)
end

-- ── 브레이크포인트 ───────────────────────────────────────────
-- 24비트 실행 주소(뱅크 포함). pc 필터·이벤트 기록에 쓴다.
local function full_pc(st)
  return (st["cpu.k"] or 0) * 65536 + (st["cpu.pc"] or 0)
end

-- hex 허용 숫자 파서(snapshot 스펙 문자열 내 주소용): 0x/$/10진.
local function snum(s)
  s = tostring(s):gsub("^%s*(.-)%s*$", "%1")
  if s:match("^%$") then return tonumber(s:sub(2), 16) end
  if s:match("^0[xX]") then return tonumber(s:sub(3), 16) end
  return tonumber(s)
end

local function record_hit(bp, addr, value)
  local st = emu.getState()
  if bp.pc_min then                       -- pc 조건: 지정 pc 범위에서 일어난 접근만(노이즈 제거)
    local pc = full_pc(st)
    if pc < bp.pc_min or pc > bp.pc_max then return end
  end
  if #events >= EVENT_CAP then
    dropped = dropped + 1
  else
    local ev = {
      type = "breakpoint_hit", breakpoint_id = bp.id, kind = bp.kind,
      address = addr, value = value or 0, pc = full_pc(st), frame = frame,
    }
    -- write BP가 PPU 데이터 포트($2118/9 VRAM·$2122 CGRAM·$2104 OAM)에 걸렸으면 목적지 주소를 함께
    -- 싣는다. dma BP가 DMA 적재를 잡듯, 이건 CPU의 소량 직접 포트 쓰기(STA $2118 등 타일맵 1엔트리)가
    -- "VRAM 어느 워드주소로 갔나"를 PC·값과 함께 답하게 한다(런타임 라벨 타일맵 추적). addr은 뱅크미러로
    -- $80xxxx일 수 있어 하위 16비트로 판별. (VRAM/OAM write를 소스+값과 함께 기록.)
    if bp.kind == "write" then
      local low = addr % 0x10000
      if low == 0x2118 or low == 0x2119 then ev.vram_addr = st["ppu.vramAddress"]
      elseif low == 0x2122 then ev.cgram_addr = st["ppu.cgramAddress"]
      elseif low == 0x2104 then ev.oam_addr = st["ppu.oamRamAddress"] end
    end
    -- 히트 순간 atomic 스냅샷: freeze 후 read 사이 워치독-회피 step(1) 드리프트(+데드맨)로 ZP 등
    -- 명령단위 상태가 호출마다 변해 "히트 순간"을 못 잡는다. 그래서 히트 시점에 레지스터(항상)와
    -- set_breakpoint의 snapshot 스펙(mt:addr:len) 메모리를 여기서 잡아 이벤트에 실어 보존 → 이후 드리프트 무관.
    ev.regs = { pc = full_pc(st), a = st["cpu.a"], x = st["cpu.x"], y = st["cpu.y"],
                sp = st["cpu.sp"], d = st["cpu.d"], dbr = st["cpu.dbr"], k = st["cpu.k"], ps = st["cpu.ps"] }
    if bp.snapshot_specs then
      local snaps = {}
      for _, sp in ipairs(bp.snapshot_specs) do
        local out = {}
        for i = 0, sp.len - 1 do out[#out + 1] = string.format("%02x", emu.read(sp.addr + i, sp.mt, false)) end
        snaps[#snaps + 1] = { memory_type = sp.mt_name, address = sp.addr, hex = table.concat(out) }
      end
      ev.snapshot = as_array(snaps)
    end
    events[#events + 1] = ev
  end
  if bp.pause_on_hit and STATE ~= "frozen" then
    flush_deferred("interrupted", "breakpoint", bp.id)   -- 진행 중 지연 명령 마무리(B2)
    STATE = "frozen"; freeze_reason = "breakpoint"
    emu.breakExecution()
  end
end

-- 레지스터 범위 break: 매 명령 exec 콜백에서 호출. 레지스터가 [lo,hi] 벗어나면 그 명령에서 freeze.
local function record_reg_hit(bp, pc, v)
  if #events >= EVENT_CAP then
    dropped = dropped + 1
  else
    events[#events + 1] = {
      type = "register_break", breakpoint_id = bp.id, register = bp.register,
      value = v, min = bp.min, max = bp.max, pc = pc, frame = frame,
    }
  end
  if bp.pause_on_hit and STATE ~= "frozen" then
    flush_deferred("interrupted", "register_break", bp.id)
    STATE = "frozen"; freeze_reason = "register_break"
    emu.breakExecution()
  end
end

-- 레지스터 범위 워치: full-range exec 콜백에서 매 명령 레지스터를 보고 [min,max] 벗어나면 break.
-- SP 폭주 등 레지스터 derail을 그 명령에서 잡는다. 매 명령 getState라 느리니 hunting 전용(끝나면 clear).
function handlers.watch_register(p)
  local id = next_bp_id; next_bp_id = next_bp_id + 1
  local reg = "cpu." .. (p.register or "sp")
  local bp = {
    id = id, kind = "reg", register = p.register or "sp",
    min = p.min or 0, max = p.max or 0xffff, pause_on_hit = p.pause_on_hit,
    cbtype = emu.callbackType.exec, start = 0, end_ = 0xffffff,
  }
  bp.ref = emu.addMemoryCallback(function(addr, value)
    local v = emu.getState()[reg]
    if v and (v < bp.min or v > bp.max) then record_reg_hit(bp, addr, v) end
  end, bp.cbtype, bp.start, bp.end_, CPU)
  breakpoints[id] = bp
  return true, { id = id }
end

-- 비트 AND(순수 산술 — Lua 버전 무관). value_mask는 최대 32비트.
local function band(a, b)
  local r, bit = 0, 1
  while a > 0 and b > 0 do
    if a % 2 == 1 and b % 2 == 1 then r = r + bit end
    a = math.floor(a / 2); b = math.floor(b / 2); bit = bit * 2
  end
  return r
end

-- 값-조건 BP가 비교할 접근 값. value_len=1이면 콜백이 준 접근 바이트(read=읽힌 값, write=쓰일 값)를
-- 그대로 쓴다. value_len>1이면 저바이트=접근 바이트, 상위는 addr+i에서 little-endian으로 읽는다(SNES).
local function access_value(bp, addr, value)
  if (bp.value_len or 1) <= 1 then return value end
  local mt = emu.memType[bp.memory_type] or emu.memType.snesMemory
  local v = value
  for i = 1, bp.value_len - 1 do
    v = v + emu.read(addr + i, mt, false) * (256 ^ i)
  end
  return v
end

-- DMA 채널 스냅샷: MDMAEN($420B) 비트로 활성 채널의 src/dest/size/mode를 get_state에서 읽는다.
-- DMA/HDMA는 CPU를 우회해 read/write BP가 못 잡으므로(NMI/VBlank 그래픽 전송), 이걸로 "무엇이
-- 어디로 전송됐나"를 포착한다. MDMAEN write는 CPU 명령(STA $420B)이라 write 콜백으로 잡힌다.
local function dma_snapshot(mdmaen)
  local st = emu.getState()
  local chans = {}
  for ch = 0, 7 do
    if band(mdmaen, 2 ^ ch) ~= 0 then
      local pfx = "dmaController.channel[" .. ch .. "]."
      local dest = st[pfx .. "destAddress"]
      local c = {
        channel = ch,
        src = (st[pfx .. "srcBank"] or 0) * 65536 + (st[pfx .. "srcAddress"] or 0),
        dest = dest,     -- B-bus 레지스터 하위($21xx): 0x18/0x19=VRAM, 0x22=CGRAM, 0x04=OAM
        size = st[pfx .. "transferSize"],   -- 바이트(0이면 0x10000)
        mode = st[pfx .. "transferMode"],
      }
      -- write BP는 DMA가 "어디로" 썼는지 못 잡는다. B-bus 목적지별 PPU 목적지 주소를 함께 캡처
      -- (트리거 시점의 주소 = 전송 시작 주소). 이걸로 "CHR이 VRAM $7000에 들어갔나" 같은 질의가 가능.
      if dest == 0x18 or dest == 0x19 then c.vram_addr = st["ppu.vramAddress"]      -- VRAM 워드주소(CHR/타일맵)
      elseif dest == 0x22 then c.cgram_addr = st["ppu.cgramAddress"]                 -- CGRAM(팔레트)
      elseif dest == 0x04 then c.oam_addr = st["ppu.oamRamAddress"] end              -- OAM
      chans[#chans + 1] = c
    end
  end
  return chans
end

function handlers.set_breakpoint(p)
  local id = next_bp_id; next_bp_id = next_bp_id + 1
  -- NMI/IRQ: 메모리 접근이 아니라 이벤트(인터럽트 진입). exec BP가 못 잡는 NMI/VBlank 컨텍스트를
  -- 그 진입에서 freeze해 핸들러 상태를 검사·step하게 한다.
  if p.kind == "nmi" or p.kind == "irq" then
    local evtype = (p.kind == "nmi") and emu.eventType.nmi or emu.eventType.irq
    local bp = { id = id, kind = p.kind, is_event = true, evtype = evtype, pause_on_hit = p.pause_on_hit }
    bp.ref = emu.addEventCallback(function()
      local st = emu.getState()
      if #events < EVENT_CAP then
        events[#events + 1] = { type = "breakpoint_hit", breakpoint_id = id, kind = p.kind,
          address = 0, value = 0, pc = full_pc(st), frame = frame }
      else dropped = dropped + 1 end
      if bp.pause_on_hit and STATE ~= "frozen" then
        flush_deferred("interrupted", p.kind, id); STATE = "frozen"; freeze_reason = p.kind; emu.breakExecution()
      end
    end, evtype)
    breakpoints[id] = bp
    return true, { id = id }
  end
  -- DMA: MDMAEN($420B, 또는 start로 지정) write 시 채널 스냅샷을 dma 이벤트로. 매 프레임 발생이라
  -- 기본은 freeze 안 함(pause_on_hit=true면 freeze). poll_events로 "이번 프레임 DMA"를 드레인.
  if p.kind == "dma" then
    -- $420B(MDMAEN)는 banks $00-$3F·$80-$BF에 미러된다. 게임이 어느 뱅크에서 STA $420B 하든 잡으려면
    -- 미러를 등록해야 한다(Mesen 메모리 콜백은 뱅크별 절대주소 — bank $00 등록은 bank $80 접근을 못 잡음).
    -- 기본은 슬로우($00:420B)·패스트($80:420B) 뱅크 둘. p.start로 특정 뱅크 미러만 지정 가능.
    -- start=$420B(canonical) 또는 미지정/0이면 슬로우($00)·패스트($80) 뱅크 미러 둘 다 자동 등록.
    -- 특정 뱅크 미러만 원하면 그 절대주소(예 $80:420B=0x80420B)를 start로.
    local regs = (p.start == nil or p.start == 0 or p.start == 0x420B) and { 0x420B, 0x80420B } or { p.start }
    local bp = { id = id, kind = "dma", is_dma = true, cbtype = emu.callbackType.write,
                 dma_refs = {}, pause_on_hit = p.pause_on_hit }
    -- 플러드 필터(선택, dma kind에서 value/pc 필드 재활용): value=dest(B-bus low: 0x18/0x19 VRAM,
    -- 0x04 OAM, 0x22 CGRAM)만, pc_min/pc_max=VRAM vram_addr 범위. 관심 채널 없으면 이벤트 자체를 스킵 →
    -- 매프레임 OAM 같은 잡음에 1회성 폰트 DMA가 묻히지 않게.
    local dest_filter, vmin, vmax = p.value, p.pc_min, p.pc_max
    local has_filter = (dest_filter ~= nil) or (vmin ~= nil) or (vmax ~= nil)
    local function on_dma(addr, value)
      local chans = dma_snapshot(value)
      if has_filter then
        local kept = {}
        for _, c in ipairs(chans) do
          local ok = true
          if dest_filter ~= nil and c.dest ~= dest_filter then ok = false end
          if ok and (vmin ~= nil or vmax ~= nil) then
            local va = c.vram_addr
            if va == nil then ok = false
            elseif vmin ~= nil and va < vmin then ok = false
            elseif vmax ~= nil and va > vmax then ok = false end
          end
          if ok then kept[#kept + 1] = c end
        end
        if #kept == 0 then return end          -- 관심 채널 없음 → 스킵(플러드 제거)
        chans = kept
      end
      if #events < EVENT_CAP then
        events[#events + 1] = { type = "dma", breakpoint_id = id, address = addr, mdmaen = value,
          channels = as_array(chans), pc = full_pc(emu.getState()), frame = frame }
      else dropped = dropped + 1 end
      if bp.pause_on_hit and STATE ~= "frozen" then
        flush_deferred("interrupted", "dma", id); STATE = "frozen"; freeze_reason = "dma"; emu.breakExecution()
      end
    end
    for _, reg in ipairs(regs) do
      bp.dma_refs[#bp.dma_refs + 1] = { ref = emu.addMemoryCallback(on_dma, bp.cbtype, reg, reg, CPU), reg = reg }
    end
    breakpoints[id] = bp
    return true, { id = id }
  end
  local cbtype = emu.callbackType[p.kind]
  if not cbtype then return false, "bad_params", "kind는 exec/read/write/nmi/irq/dma" end
  -- 값-조건: read/write BP에서 접근 값이 (value & value_mask)와 같을 때만 발화.
  -- exec BP엔 접근 값 개념이 없어 무시. value 미지정이면 종전대로 모든 접근에 발화.
  local has_value = (p.value ~= nil) and (p.kind ~= "exec")
  local bp = {
    id = id, kind = p.kind, memory_type = p.memory_type,
    pause_on_hit = p.pause_on_hit, auto_savestate = p.auto_savestate,
    start = p.start or 0, end_ = p["end"] or p.start or 0,
    cbtype = cbtype, pc_min = p.pc_min, pc_max = p.pc_max,   -- pc 조건(선택)
    has_value = has_value, value = p.value or 0,
    value_mask = p.value_mask or 0xFFFFFFFF, value_len = math.max(1, math.min(4, p.value_len or 1)),
  }
  -- snapshot: 히트 순간 atomic 캡처할 메모리 스펙 리스트("mt:addr:len", addr는 0x/$/10진). record_hit이
  -- 레지스터(항상)와 함께 이벤트에 싣는다 → 워치독 드리프트/데드맨 무관하게 히트 순간 보존.
  if p.snapshot then
    bp.snapshot_specs = {}
    for _, spec in ipairs(p.snapshot) do
      local mt_s, addr_s, len_s = tostring(spec):match("^%s*([^:]+):([^:]+):([^:]+)%s*$")
      if mt_s then
        bp.snapshot_specs[#bp.snapshot_specs + 1] =
          { mt = emu.memType[mt_s] or mt_s, mt_name = mt_s, addr = snum(addr_s) or 0, len = snum(len_s) or 1 }
      end
    end
  end
  local function on_access(addr, value)
    if bp.has_value then
      local v = access_value(bp, addr, value)
      if band(v, bp.value_mask) ~= band(bp.value, bp.value_mask) then return end  -- 값 불일치 → 무시
    end
    record_hit(bp, addr, value)
  end
  -- 뱅크 미러 자동등록: snesMemory의 system 레지스터/IO 영역($2000-$7FFF: PPU/CPU 레지스터)을 뱅크 없이
  -- ($2117 등) 준 단일 주소면, 게임이 어느 뱅크($00 슬로우/$80 패스트)에서 접근하든 잡게 $00·$80 미러를
  -- 둘 다 등록한다(Mesen 콜백은 뱅크별 절대주소 — bank $00만 걸면 bank $80 실행 게임의 $2117 등을 놓침
  -- ). 범위 BP·뱅크 명시 주소($XX0000+)·LowRAM($0000-$1FFF, snesWorkRam 권장)은 그대로.
  local mirrors = { { bp.start, bp.end_ } }
  if (p.kind == "read" or p.kind == "write") and p.memory_type == "snesMemory"
     and bp.start == bp.end_ and bp.start >= 0x2000 and bp.start < 0x8000 then
    mirrors = { { bp.start, bp.start }, { bp.start + 0x800000, bp.start + 0x800000 } }
  end
  bp.mirror_refs = {}
  for _, m in ipairs(mirrors) do
    bp.mirror_refs[#bp.mirror_refs + 1] =
      { ref = emu.addMemoryCallback(on_access, cbtype, m[1], m[2], CPU), lo = m[1], hi = m[2] }
  end
  breakpoints[id] = bp
  return true, { id = id }
end

function handlers.clear_breakpoint(p)
  local bp = breakpoints[p.id]
  if not bp then return false, "not_found", "그런 breakpoint 없음" end
  if bp.is_event then emu.removeEventCallback(bp.ref, bp.evtype)
  elseif bp.is_dma then
    for _, d in ipairs(bp.dma_refs) do emu.removeMemoryCallback(d.ref, bp.cbtype, d.reg, d.reg, CPU) end
  else
    for _, m in ipairs(bp.mirror_refs) do emu.removeMemoryCallback(m.ref, bp.cbtype, m.lo, m.hi, CPU) end
  end
  breakpoints[p.id] = nil
  return true, { cleared = p.id }
end

function handlers.list_breakpoints()
  local out = {}
  for _, bp in pairs(breakpoints) do
    out[#out + 1] = {
      id = bp.id, kind = bp.kind, memory_type = bp.memory_type,
      start = bp.start, ["end"] = bp.end_,
      register = bp.register, min = bp.min, max = bp.max,   -- reg 워치(watch_register)
      pc_min = bp.pc_min, pc_max = bp.pc_max,            -- pc 조건
      pause_on_hit = bp.pause_on_hit, auto_savestate = bp.auto_savestate,
    }
  end
  return true, { breakpoints = as_array(out) }
end

function handlers.clear_all_breakpoints()
  local n = 0
  for id, bp in pairs(breakpoints) do
    if bp.is_event then emu.removeEventCallback(bp.ref, bp.evtype)
    elseif bp.is_dma then
      for _, d in ipairs(bp.dma_refs) do emu.removeMemoryCallback(d.ref, bp.cbtype, d.reg, d.reg, CPU) end
    else
      for _, m in ipairs(bp.mirror_refs) do emu.removeMemoryCallback(m.ref, bp.cbtype, m.lo, m.hi, CPU) end
    end
    breakpoints[id] = nil
    n = n + 1
  end
  return true, { cleared = n }
end

function handlers.poll_events()
  local out = { events = as_array(events), dropped = dropped }
  events = {}; dropped = 0
  return true, out
end

-- break_on_reset: 게임이 리셋 핸들러를 실행하면(워치독 리셋·하드 크래시→리셋) freeze. 리셋벡터
-- $00:FFFC에서 핸들러 주소를 읽어 그 지점에 exec BP. (SNES엔 invalid opcode가 없고 SP wrap은 watch_register.)
function handlers.break_on_reset(p)
  local on = p.enabled and true or false
  if on and not reset_bp then
    local handler = emu.read16(0xFFFC, emu.memType.snesMemory, false)  -- 리셋벡터(뱅크0)
    reset_bp = { handler = handler }
    reset_bp.ref = emu.addMemoryCallback(function(addr, value)
      if #events < EVENT_CAP then
        events[#events + 1] = { type = "crash", reason = "reset_vector", pc = addr, frame = frame }
      else dropped = dropped + 1 end
      if STATE ~= "frozen" then
        flush_deferred("interrupted", "crash", 0)
        STATE = "frozen"; freeze_reason = "crash"; emu.breakExecution()
      end
    end, emu.callbackType.exec, handler, handler, CPU)
    return true, { watching = true, handler = handler }
  elseif not on and reset_bp then
    emu.removeMemoryCallback(reset_bp.ref, emu.callbackType.exec, reset_bp.handler, reset_bp.handler, CPU)
    reset_bp = nil
    return true, { watching = false }
  end
  return true, { watching = reset_bp ~= nil, handler = reset_bp and reset_bp.handler }
end

-- ── 실행추적 (콜스택 + 트레이스) ─────────────────────────────
-- 매 명령 exec 콜백. value가 opcode 바이트(exec 콜백). 링버퍼 기록 + JSR/JSL/RTS/RTL/RTI로 콜스택 갱신.
-- 현재 SP보다 위로 올라간(=리턴된) 프레임을 정리. JMP로 리턴하는 코드(RTS 없음)도 잡는다.
-- 프레임.sp는 호출 직전 SP. 리턴하면 SP가 그 값 이상으로 회복되므로, sp >= frame.sp면 pop.
local function reconcile_callstack(sp)
  while #callstack > 0 and sp >= callstack[#callstack].sp do
    table.remove(callstack)
  end
end

local function trace_cb(addr, value)
  local op = value
  if type(op) ~= "number" then op = emu.read(addr, emu.memType.snesMemory, false) end
  trace_ring[(trace_idx % TRACE_CAP) + 1] = { pc = addr, op = op }
  trace_idx = trace_idx + 1
  if op == 0x20 or op == 0x22 then                    -- JSR / JSL: 호출지 push(SP 정합 후)
    local sp = emu.getState()["cpu.sp"]
    reconcile_callstack(sp)                            -- 이미 리턴한 프레임(JMP-리턴 포함) 정리
    callstack[#callstack + 1] = { pc = addr, sp = sp }
  elseif op == 0x60 or op == 0x6b or op == 0x40 then  -- RTS / RTL / RTI: pop
    if #callstack > 0 then table.remove(callstack) end
  end
end

-- 실행추적 on/off. 켜면 매 명령 콜백 등록(느림 — hunting 전용). 켤 때 링·콜스택 초기화.
function handlers.set_trace(p)
  local on = p.enabled and true or false
  if on and not trace_on then
    trace_ring = {}; trace_idx = 0; callstack = {}
    trace_ref = emu.addMemoryCallback(trace_cb, emu.callbackType.exec, 0, 0xffffff, CPU)
    trace_on = true
  elseif not on and trace_on then
    emu.removeMemoryCallback(trace_ref, emu.callbackType.exec, 0, 0xffffff, CPU)
    trace_ref = nil; trace_on = false
  end
  return true, { tracing = trace_on }
end

-- 최근 count개 명령을 시간순(오래된→최신). pc는 24비트 실행주소, op는 opcode 바이트.
function handlers.get_trace(p)
  local count = math.min(p.count or TRACE_CAP, TRACE_CAP)
  local total = math.min(trace_idx, TRACE_CAP)
  local n = math.min(count, total)
  local out = {}
  for i = 0, n - 1 do
    out[#out + 1] = trace_ring[((trace_idx - n + i) % TRACE_CAP) + 1]
  end
  return true, { trace = as_array(out), tracing = trace_on, total = trace_idx }
end

-- 콜스택: 호출지(JSR/JSL의 pc) 리스트, 바깥→안(안쪽이 마지막). "어떻게 여기 왔나" 즉답.
-- 조회 시 현재 SP로 한 번 더 정합(마지막 호출 이후 JMP로 리턴한 프레임 정리).
function handlers.call_stack()
  reconcile_callstack(emu.getState()["cpu.sp"])
  local out = {}
  for _, f in ipairs(callstack) do out[#out + 1] = f.pc end
  return true, { call_stack = as_array(out), depth = #callstack, tracing = trace_on }
end

-- ── 메모리 덤프 (emucap diff 입력) ───────────────────────────
-- 표준 리전을 .bin과 regions.json으로 디렉토리에 쓴다. 콘솔 변경 시 목록을 바꾼다.
local DUMP_REGIONS = {
  { name = "wram", mt = "snesWorkRam",   base = 8257536, size = 0x20000 },  -- 128KB @ $7E0000
  { name = "vram", mt = "snesVideoRam",  base = 0,       size = 0x10000 },  -- 64KB
  { name = "cram", mt = "snesCgRam",     base = 0,       size = 0x200 },    -- 512B(팔레트)
  { name = "oam",  mt = "snesSpriteRam", base = 0,       size = 0x220 },    -- 544B(스프라이트)
}

function handlers.dump_memory(p)
  if not p.path then return false, "bad_params", "path 필요" end
  os.execute('mkdir -p "' .. p.path .. '"')
  local metas = {}
  for _, r in ipairs(DUMP_REGIONS) do
    local mt = emu.memType[r.mt]
    local buf = {}
    for i = 0, r.size - 1 do
      buf[i + 1] = string.char(emu.read(i, mt, false))
    end
    local f = assert(io.open(p.path .. "/" .. r.name .. ".bin", "wb"))
    f:write(table.concat(buf))
    f:close()
    metas[#metas + 1] = string.format(
      '{"name":"%s","memory_type":"%s","base_address":%d,"size":%d}',
      r.name, r.mt, r.base, r.size)
  end
  local mf = assert(io.open(p.path .. "/regions.json", "wb"))
  mf:write("[" .. table.concat(metas, ",") .. "]")
  mf:close()
  return true, { path = p.path, regions = #DUMP_REGIONS }
end

-- ── 바이트패턴 검색 (find_pattern) ───────────────────────────
-- 알려진 선형 메모리 타입의 영역 크기(start만 주고 length 생략 시 끝까지 스캔용). 콘솔 추가 시 보강.
local REGION_SIZE = {
  snesWorkRam = 0x20000, snesVideoRam = 0x10000, snesCgRam = 0x200,
  snesSpriteRam = 0x220, snesSaveRam = 0x8000,
}
local SCAN_CAP = 0x20000   -- 1초 워치독 안전: 한 호출 최대 128KB 스캔(emu.read ~2M/s → ≈65ms)

-- 영역을 어댑터 내부에서 한 번 읽어 string.find(plain)로 매칭 오프셋들을 돌려준다 → 128KB를 와이어로
-- 안 보내고 오프셋만 회신(토큰·지연 최소). 런타임 문자열/버퍼/테이블
-- 위치 특정용(예: ROM에 정적으로 없는 런타임-빌드 라벨을 WRAM에서 찾기). 결정론적 결과는 frozen 권장.
-- 의미 키: memory_type, hex(찾을 바이트열, 짝수 길이), start(오프셋, 기본 0), length(검색 길이; 미지정 시
-- 영역 끝까지), max_matches(상한, 기본 256), align(이 배수 오프셋만, 기본 1 — 테이블 엔트리 검색).
function handlers.find_pattern(p)
  local mt = emu.memType[p.memory_type] or p.memory_type
  local hex = p.hex
  if type(hex) ~= "string" or #hex < 2 or #hex % 2 ~= 0 then
    return false, "bad_params", "hex는 짝수 길이(≥1바이트) hex 문자열"
  end
  local pat = {}
  for i = 1, #hex, 2 do
    local b = tonumber(hex:sub(i, i + 1), 16)
    if not b then return false, "bad_params", "hex 디코드 실패" end
    pat[#pat + 1] = string.char(b)
  end
  pat = table.concat(pat)
  local start = p.start or 0
  if start < 0 then start = 0 end
  local len = p.length
  if not len then
    local rs = REGION_SIZE[p.memory_type]
    if rs then len = rs - start
    else return false, "bad_params", "length 필요(알 수 없는 memory_type은 검색 길이를 명시)" end
  end
  if len < 0 then len = 0 end
  local truncated_scan = false
  if len > SCAN_CAP then len = SCAN_CAP; truncated_scan = true end   -- 워치독 안전 상한
  -- 영역을 바이너리 문자열로 1회 적재
  local buf = {}
  for i = 0, len - 1 do buf[i + 1] = string.char(emu.read(start + i, mt, false)) end
  buf = table.concat(buf)
  local align = (p.align and p.align >= 1) and p.align or 1
  local max_matches = p.max_matches or 256
  local matches, truncated, pos = {}, false, 1
  while true do
    local s = string.find(buf, pat, pos, true)   -- plain=true: 리터럴 바이트열(정규식 아님)
    if not s then break end
    local off = start + (s - 1)
    if (off - start) % align == 0 then
      if #matches >= max_matches then truncated = true; break end
      matches[#matches + 1] = off
    end
    pos = s + 1                                    -- 겹치는 매칭도 찾도록 1바이트씩 전진
  end
  return true, {
    matches = as_array(matches), count = #matches,
    truncated = truncated or truncated_scan, scanned = len, start = start,
  }
end

-- ── 디스어셈블러 (65816) ─────────────────────────────────────
-- Mesen2 Lua엔 디스어셈블 API가 없어 65816 디코더를 직접 구현한다(스탠드얼론 디스어셈블러에서 포팅).
-- M/X 플래그는 현재 CPU 상태(cpu.ps bit5=M, bit4=X)에서 시작해 REP/SEP로 전진 추적한다 —
-- 즉시값 폭(immM/immX)이 플래그 의존이라, 명령 경계가 정확하려면 추적이 필수.

-- 어드레싱 모드 → 기본 오퍼랜드 바이트 수(immM/immX는 M/X로 1↔2 가변).
local MODE_SIZE = {
  imp = 0, acc = 0, imm8 = 1, immM = 1, immX = 1,
  dp = 1, dpx = 1, dpy = 1, dpind = 1, dpindx = 1, dpindy = 1, dpindl = 1, dpindly = 1,
  sr = 1, sry = 1, rel = 1,
  abs = 2, abx = 2, aby = 2, absind = 2, absindx = 2, rell = 2, bm = 2,
  long = 3, lngx = 3,
}

-- 65816 전체 opcode → {니모닉, 모드}.
local OPCODES = {
  [0x00]={"BRK","imm8"},   [0x01]={"ORA","dpindx"}, [0x02]={"COP","imm8"},  [0x03]={"ORA","sr"},
  [0x04]={"TSB","dp"},     [0x05]={"ORA","dp"},     [0x06]={"ASL","dp"},    [0x07]={"ORA","dpindl"},
  [0x08]={"PHP","imp"},    [0x09]={"ORA","immM"},   [0x0A]={"ASL","acc"},   [0x0B]={"PHD","imp"},
  [0x0C]={"TSB","abs"},    [0x0D]={"ORA","abs"},    [0x0E]={"ASL","abs"},   [0x0F]={"ORA","long"},
  [0x10]={"BPL","rel"},    [0x11]={"ORA","dpindy"}, [0x12]={"ORA","dpind"}, [0x13]={"ORA","sry"},
  [0x14]={"TRB","dp"},     [0x15]={"ORA","dpx"},    [0x16]={"ASL","dpx"},   [0x17]={"ORA","dpindly"},
  [0x18]={"CLC","imp"},    [0x19]={"ORA","aby"},    [0x1A]={"INC","acc"},   [0x1B]={"TCS","imp"},
  [0x1C]={"TRB","abs"},    [0x1D]={"ORA","abx"},    [0x1E]={"ASL","abx"},   [0x1F]={"ORA","lngx"},
  [0x20]={"JSR","abs"},    [0x21]={"AND","dpindx"}, [0x22]={"JSL","long"},  [0x23]={"AND","sr"},
  [0x24]={"BIT","dp"},     [0x25]={"AND","dp"},     [0x26]={"ROL","dp"},    [0x27]={"AND","dpindl"},
  [0x28]={"PLP","imp"},    [0x29]={"AND","immM"},   [0x2A]={"ROL","acc"},   [0x2B]={"PLD","imp"},
  [0x2C]={"BIT","abs"},    [0x2D]={"AND","abs"},    [0x2E]={"ROL","abs"},   [0x2F]={"AND","long"},
  [0x30]={"BMI","rel"},    [0x31]={"AND","dpindy"}, [0x32]={"AND","dpind"}, [0x33]={"AND","sry"},
  [0x34]={"BIT","dpx"},    [0x35]={"AND","dpx"},    [0x36]={"ROL","dpx"},   [0x37]={"AND","dpindly"},
  [0x38]={"SEC","imp"},    [0x39]={"AND","aby"},    [0x3A]={"DEC","acc"},   [0x3B]={"TSC","imp"},
  [0x3C]={"BIT","abx"},    [0x3D]={"AND","abx"},    [0x3E]={"ROL","abx"},   [0x3F]={"AND","lngx"},
  [0x40]={"RTI","imp"},    [0x41]={"EOR","dpindx"}, [0x42]={"WDM","imm8"},  [0x43]={"EOR","sr"},
  [0x44]={"MVP","bm"},     [0x45]={"EOR","dp"},     [0x46]={"LSR","dp"},    [0x47]={"EOR","dpindl"},
  [0x48]={"PHA","imp"},    [0x49]={"EOR","immM"},   [0x4A]={"LSR","acc"},   [0x4B]={"PHK","imp"},
  [0x4C]={"JMP","abs"},    [0x4D]={"EOR","abs"},    [0x4E]={"LSR","abs"},   [0x4F]={"EOR","long"},
  [0x50]={"BVC","rel"},    [0x51]={"EOR","dpindy"}, [0x52]={"EOR","dpind"}, [0x53]={"EOR","sry"},
  [0x54]={"MVN","bm"},     [0x55]={"EOR","dpx"},    [0x56]={"LSR","dpx"},   [0x57]={"EOR","dpindly"},
  [0x58]={"CLI","imp"},    [0x59]={"EOR","aby"},    [0x5A]={"PHY","imp"},   [0x5B]={"TCD","imp"},
  [0x5C]={"JML","long"},   [0x5D]={"EOR","abx"},    [0x5E]={"LSR","abx"},   [0x5F]={"EOR","lngx"},
  [0x60]={"RTS","imp"},    [0x61]={"ADC","dpindx"}, [0x62]={"PER","rell"},  [0x63]={"ADC","sr"},
  [0x64]={"STZ","dp"},     [0x65]={"ADC","dp"},     [0x66]={"ROR","dp"},    [0x67]={"ADC","dpindl"},
  [0x68]={"PLA","imp"},    [0x69]={"ADC","immM"},   [0x6A]={"ROR","acc"},   [0x6B]={"RTL","imp"},
  [0x6C]={"JMP","absind"}, [0x6D]={"ADC","abs"},    [0x6E]={"ROR","abs"},   [0x6F]={"ADC","long"},
  [0x70]={"BVS","rel"},    [0x71]={"ADC","dpindy"}, [0x72]={"ADC","dpind"}, [0x73]={"ADC","sry"},
  [0x74]={"STZ","dpx"},    [0x75]={"ADC","dpx"},    [0x76]={"ROR","dpx"},   [0x77]={"ADC","dpindly"},
  [0x78]={"SEI","imp"},    [0x79]={"ADC","aby"},    [0x7A]={"PLY","imp"},   [0x7B]={"TDC","imp"},
  [0x7C]={"JMP","absindx"},[0x7D]={"ADC","abx"},    [0x7E]={"ROR","abx"},   [0x7F]={"ADC","lngx"},
  [0x80]={"BRA","rel"},    [0x81]={"STA","dpindx"}, [0x82]={"BRL","rell"},  [0x83]={"STA","sr"},
  [0x84]={"STY","dp"},     [0x85]={"STA","dp"},     [0x86]={"STX","dp"},    [0x87]={"STA","dpindl"},
  [0x88]={"DEY","imp"},    [0x89]={"BIT","immM"},   [0x8A]={"TXA","imp"},   [0x8B]={"PHB","imp"},
  [0x8C]={"STY","abs"},    [0x8D]={"STA","abs"},    [0x8E]={"STX","abs"},   [0x8F]={"STA","long"},
  [0x90]={"BCC","rel"},    [0x91]={"STA","dpindy"}, [0x92]={"STA","dpind"}, [0x93]={"STA","sry"},
  [0x94]={"STY","dpx"},    [0x95]={"STA","dpx"},    [0x96]={"STX","dpy"},   [0x97]={"STA","dpindly"},
  [0x98]={"TYA","imp"},    [0x99]={"STA","aby"},    [0x9A]={"TXS","imp"},   [0x9B]={"TXY","imp"},
  [0x9C]={"STZ","abs"},    [0x9D]={"STA","abx"},    [0x9E]={"STZ","abx"},   [0x9F]={"STA","lngx"},
  [0xA0]={"LDY","immX"},   [0xA1]={"LDA","dpindx"}, [0xA2]={"LDX","immX"},  [0xA3]={"LDA","sr"},
  [0xA4]={"LDY","dp"},     [0xA5]={"LDA","dp"},     [0xA6]={"LDX","dp"},    [0xA7]={"LDA","dpindl"},
  [0xA8]={"TAY","imp"},    [0xA9]={"LDA","immM"},   [0xAA]={"TAX","imp"},   [0xAB]={"PLB","imp"},
  [0xAC]={"LDY","abs"},    [0xAD]={"LDA","abs"},    [0xAE]={"LDX","abs"},   [0xAF]={"LDA","long"},
  [0xB0]={"BCS","rel"},    [0xB1]={"LDA","dpindy"}, [0xB2]={"LDA","dpind"}, [0xB3]={"LDA","sry"},
  [0xB4]={"LDY","dpx"},    [0xB5]={"LDA","dpx"},    [0xB6]={"LDX","dpy"},   [0xB7]={"LDA","dpindly"},
  [0xB8]={"CLV","imp"},    [0xB9]={"LDA","aby"},    [0xBA]={"TSX","imp"},   [0xBB]={"TYX","imp"},
  [0xBC]={"LDY","abx"},    [0xBD]={"LDA","abx"},    [0xBE]={"LDX","aby"},   [0xBF]={"LDA","lngx"},
  [0xC0]={"CPY","immX"},   [0xC1]={"CMP","dpindx"}, [0xC2]={"REP","imm8"},  [0xC3]={"CMP","sr"},
  [0xC4]={"CPY","dp"},     [0xC5]={"CMP","dp"},     [0xC6]={"DEC","dp"},    [0xC7]={"CMP","dpindl"},
  [0xC8]={"INY","imp"},    [0xC9]={"CMP","immM"},   [0xCA]={"DEX","imp"},   [0xCB]={"WAI","imp"},
  [0xCC]={"CPY","abs"},    [0xCD]={"CMP","abs"},    [0xCE]={"DEC","abs"},   [0xCF]={"CMP","long"},
  [0xD0]={"BNE","rel"},    [0xD1]={"CMP","dpindy"}, [0xD2]={"CMP","dpind"}, [0xD3]={"CMP","sry"},
  [0xD4]={"PEI","dp"},     [0xD5]={"CMP","dpx"},    [0xD6]={"DEC","dpx"},   [0xD7]={"CMP","dpindly"},
  [0xD8]={"CLD","imp"},    [0xD9]={"CMP","aby"},    [0xDA]={"PHX","imp"},   [0xDB]={"STP","imp"},
  [0xDC]={"JML","absind"}, [0xDD]={"CMP","abx"},    [0xDE]={"DEC","abx"},   [0xDF]={"CMP","lngx"},
  [0xE0]={"CPX","immX"},   [0xE1]={"SBC","dpindx"}, [0xE2]={"SEP","imm8"},  [0xE3]={"SBC","sr"},
  [0xE4]={"CPX","dp"},     [0xE5]={"SBC","dp"},     [0xE6]={"INC","dp"},    [0xE7]={"SBC","dpindl"},
  [0xE8]={"INX","imp"},    [0xE9]={"SBC","immM"},   [0xEA]={"NOP","imp"},   [0xEB]={"XBA","imp"},
  [0xEC]={"CPX","abs"},    [0xED]={"SBC","abs"},    [0xEE]={"INC","abs"},   [0xEF]={"SBC","long"},
  [0xF0]={"BEQ","rel"},    [0xF1]={"SBC","dpindy"}, [0xF2]={"SBC","dpind"}, [0xF3]={"SBC","sry"},
  [0xF4]={"PEA","abs"},    [0xF5]={"SBC","dpx"},    [0xF6]={"INC","dpx"},   [0xF7]={"SBC","dpindly"},
  [0xF8]={"SED","imp"},    [0xF9]={"SBC","aby"},    [0xFA]={"PLX","imp"},   [0xFB]={"XCE","imp"},
  [0xFC]={"JSR","absindx"},[0xFD]={"SBC","abx"},    [0xFE]={"INC","abx"},   [0xFF]={"SBC","lngx"},
}

local function s8(v) return (v >= 0x80) and (v - 0x100) or v end
local function s16(v) return (v >= 0x8000) and (v - 0x10000) or v end

-- 오퍼랜드 문자열. addr16=명령 시작의 뱅크 내 16비트 주소, b1/b2/b3=오퍼랜드 바이트.
local function fmt_operand(mode, addr16, size, b1, b2, b3)
  if mode == "imp" then return "" end
  if mode == "acc" then return "A" end
  if mode == "bm" then return string.format("$%02X,$%02X", b1, b2) end          -- dest, src
  if mode == "rel" then return string.format("$%04X", (addr16 + 2 + s8(b1)) % 0x10000) end
  if mode == "rell" then return string.format("$%04X", (addr16 + 3 + s16(b1 + b2 * 256)) % 0x10000) end
  local val
  if size == 1 then val = b1
  elseif size == 2 then val = b1 + b2 * 256
  else val = b1 + b2 * 256 + b3 * 65536 end
  if mode == "immM" or mode == "immX" then
    return (size == 2) and string.format("#$%04X", val) or string.format("#$%02X", val)
  elseif mode == "imm8" then return string.format("#$%02X", val)
  elseif mode == "dp" then return string.format("$%02X", val)
  elseif mode == "dpx" then return string.format("$%02X,X", val)
  elseif mode == "dpy" then return string.format("$%02X,Y", val)
  elseif mode == "dpind" then return string.format("($%02X)", val)
  elseif mode == "dpindx" then return string.format("($%02X,X)", val)
  elseif mode == "dpindy" then return string.format("($%02X),Y", val)
  elseif mode == "dpindl" then return string.format("[$%02X]", val)
  elseif mode == "dpindly" then return string.format("[$%02X],Y", val)
  elseif mode == "sr" then return string.format("$%02X,S", val)
  elseif mode == "sry" then return string.format("($%02X,S),Y", val)
  elseif mode == "abs" then return string.format("$%04X", val)
  elseif mode == "abx" then return string.format("$%04X,X", val)
  elseif mode == "aby" then return string.format("$%04X,Y", val)
  elseif mode == "absind" then return string.format("($%04X)", val)
  elseif mode == "absindx" then return string.format("($%04X,X)", val)
  elseif mode == "long" then return string.format("$%06X", val)
  elseif mode == "lngx" then return string.format("$%06X,X", val)
  end
  return ""
end

-- disassemble(address, count): 24비트 실행주소(뱅크 포함)에서 count개 명령. snesMemory 버스에서 읽는다.
-- M/X 시작값은 현재 cpu.ps(없으면 8bit 가정). 반환 [{addr,text,bytes}] — Mednafen과 같은 형태.
function handlers.disassemble(p)
  local addr = p.address or 0
  local count = math.max(1, math.min(p.count or 8, 256))
  local mt = emu.memType[p.memory_type] or emu.memType.snesMemory
  local st = emu.getState()
  local ps = st["cpu.ps"] or st["cpu.p"] or st["cpu.status"] or 0x30
  local m8 = (math.floor(ps / 0x20) % 2) == 1   -- bit5=M: set→8bit A
  local x8 = (math.floor(ps / 0x10) % 2) == 1   -- bit4=X: set→8bit X/Y
  local out = {}
  local a = addr
  for _ = 1, count do
    local addr16 = a % 0x10000
    local opcode = emu.read(a, mt, false)
    local entry = OPCODES[opcode]
    if not entry then
      out[#out + 1] = { addr = string.format("0x%06X", a),
                        text = string.format(".DB $%02X", opcode),
                        bytes = string.format("%02X", opcode) }
      a = a + 1
    else
      local mnem, mode = entry[1], entry[2]
      local size = MODE_SIZE[mode]
      if mode == "immM" and not m8 then size = 2 end
      if mode == "immX" and not x8 then size = 2 end
      local b1 = (size >= 1) and emu.read(a + 1, mt, false) or 0
      local b2 = (size >= 2) and emu.read(a + 2, mt, false) or 0
      local b3 = (size >= 3) and emu.read(a + 3, mt, false) or 0
      local operand = fmt_operand(mode, addr16, size, b1, b2, b3)
      local text = (operand == "") and mnem or (mnem .. " " .. operand)
      local raw = string.format("%02X", opcode)
      if size >= 1 then raw = raw .. string.format(" %02X", b1) end
      if size >= 2 then raw = raw .. string.format(" %02X", b2) end
      if size >= 3 then raw = raw .. string.format(" %02X", b3) end
      out[#out + 1] = { addr = string.format("0x%06X", a), text = text, bytes = raw }
      if opcode == 0xC2 then          -- REP: 비트 클리어 → 16bit
        if math.floor(b1 / 0x20) % 2 == 1 then m8 = false end
        if math.floor(b1 / 0x10) % 2 == 1 then x8 = false end
      elseif opcode == 0xE2 then      -- SEP: 비트 셋 → 8bit
        if math.floor(b1 / 0x20) % 2 == 1 then m8 = true end
        if math.floor(b1 / 0x10) % 2 == 1 then x8 = true end
      end
      a = a + 1 + size
    end
  end
  return true, as_array(out)
end

-- ── 디스패치 ─────────────────────────────────────────────────
-- RUNNING에서 한 줄 처리. pause면 freeze 진입.
local function dispatch(line)
  local id, method, p = parse_request(line)
  id = id or 0
  if method == "pause" then
    STATE = "frozen"; freeze_reason = "paused"
    reply_ok(id, { state = "frozen" })
    emu.breakExecution()   -- 실제 freeze는 codeBreak에서
    return
  end
  if method == "step" or method == "resume" then
    reply_err(id, "not_paused", "step/resume는 frozen에서만 가능")
    return
  end
  -- 지연 명령(즉시 응답 안 함; 프레임 경과/exec 후 응답)
  if method == "run_frames" then
    deferred = { id = id, kind = "run", remaining = p.n or 1, age = 0 }
    return
  end
  if method == "press_buttons" then
    local tbl, err = buttons_to_table(p.buttons)
    if not tbl then reply_err(id, "bad_params", err); return end
    input_hold = { port = p.port or 0, tbl = tbl }
    deferred = { id = id, kind = "press", remaining = p.frames or 1, age = 0 }
    return
  end
  if method == "save_state" then arm_io("save", id, p.path); return end
  if method == "load_state" then arm_io("load", id, p.path); return end
  if method == "probe" then arm_probe(id, p); return end
  local h = handlers[method]
  if not h then reply_err(id, "unknown_method", tostring(method)); return end
  -- handler 규약: 성공은 (true, result), 실패는 (false, kind, msg) 3-tuple.
  local ok, a, b, c = pcall(h, p)
  if not ok then reply_err(id, "emulator_error", a); return end
  if a == true then reply_ok(id, b) else reply_err(id, b, c) end
end

-- FROZEN에서 한 줄 처리. step/resume면 동작 지시 반환.
local function handle_in_freeze(line)
  local id, method, p = parse_request(line)
  id = id or 0
  if method == "resume" then
    reply_ok(id, { state = "running" })
    return "resume"
  elseif method == "step" then
    step_unit = (p.unit == "instructions") and "instructions" or "frames"
    step_remaining = p.frames or 1
    pending_step_id = id   -- 완료 응답은 청크들이 끝난 뒤
    return "step"
  elseif method == "pause" then
    reply_ok(id, { state = "frozen" })   -- 멱등
  elseif method == "run_frames" then
    -- frozen이면 원자적으로 resume하며 진행한다 — deferred를 세팅하고 "resume"을 반환해 freeze 루프를 빠져나가
    -- 게임을 재개한다. Rust는 이제 별도 ensure_running(resume)을 안 보낸다(별도 resume은 run_frames 도착 전
    -- free-run으로 one-shot watch/BP를 조기 소진시키는 레이스라 제거됨 — Mednafen과 동일 원자 resume 규약).
    deferred = { id = id, kind = "run", remaining = p.n or 1, age = 0 }
    return "resume"
  elseif method == "press_buttons" then
    local tbl, err = buttons_to_table(p.buttons)
    if not tbl then reply_err(id, "bad_params", err); return nil end
    input_hold = { port = p.port or 0, tbl = tbl }
    deferred = { id = id, kind = "press", remaining = p.frames or 1, age = 0 }
    return "resume"
  elseif method == "save_state" or method == "load_state" or method == "probe" then
    reply_err(id, "frozen", "frozen 상태에서는 불가 — step/resume 사용")
  else
    local h = handlers[method]
    if not h then reply_err(id, "unknown_method", tostring(method))
    else
      local ok, a, b, c = pcall(h, p)
      if not ok then reply_err(id, "emulator_error", a)
      elseif a == true then reply_ok(id, b) else reply_err(id, b, c) end
    end
  end
  return nil
end

-- step(n)을 청크(≤STEP_CHUNK)로 진행. codeBreak가 청크마다 재발화하므로
-- startFrame이 step 중 뜨는지에 의존하지 않는다.
local function do_step_chunk()
  if step_unit == "instructions" then
    local chunk = math.min(step_remaining, INSTR_CHUNK)
    step_remaining = step_remaining - chunk
    emu.step(chunk, emu.stepType.step)   -- CPU 명령 단위
  else
    local chunk = math.min(step_remaining, STEP_CHUNK)
    step_remaining = step_remaining - chunk
    emu.step(chunk, emu.stepType.ppuFrame)
  end
end

-- codeBreak: 멈춘 채 명령 서비스. 1초 워치독 내로 반드시 리턴.
emu.addEventCallback(function()
  if STATE ~= "frozen" then return end

  -- 진행 중 step의 다음 청크 또는 완료(직전 청크의 emu.step이 끝나 재진입)
  if pending_step_id then
    if step_remaining <= 0 then
      reply_ok(pending_step_id, { status = "completed", frame = frame })
      pending_step_id = nil
      -- 아래 정상 freeze 스핀으로 진행
    else
      -- 다음 청크: keepalive로 서버 타임아웃을 막고 이어서 진행
      send_line(string.format('{"id":%d,"ok":true,"result":{"status":"working"}}', pending_step_id))
      do_step_chunk()
      return
    end
  end

  -- 데드맨은 "마지막 명령 이후" 경과로 잰다. start_ms를 콜백 진입마다 새로 잡으면
  -- 워치독 회피(FREEZE_BUDGET_MS)가 매번 codeBreak를 재무장하며 카운터를 리셋해 데드맨이
  -- 영영 발동하지 않는다. freeze 진입 시 한 번 잡고, 명령을 받을 때만 갱신한다.
  if not freeze_start_ms then freeze_start_ms = os.clock() * 1000 end
  local cb_start_ms = os.clock() * 1000  -- 이번 콜백 진입(워치독 회피용 — 매 진입 리셋)
  local last_key_ms = 0                   -- freeze 핫키 폴 throttle(스핀이 빨라 매 반복 폴은 과함)
  while true do
    -- 로컬 resume 핫키(frozen→running 토글). 스핀이 매우 빠르니 ~16ms마다만 폴(라이징 에지 1회).
    if FREEZE_KEY and freeze_key_ok then
      local now = os.clock() * 1000
      if now - last_key_ms >= 16 then
        last_key_ms = now
        local fk = freeze_key_down()
        if fk and not prev_freeze_key then
          prev_freeze_key = fk
          if #events < EVENT_CAP then events[#events + 1] = { type = "user_resume", reason = "hotkey", frame = frame }
          else dropped = dropped + 1 end
          STATE = "running"; freeze_start_ms = nil; emu.resume(); return
        end
        prev_freeze_key = fk
      end
    end
    local line = poll_line()
    if line then
      freeze_start_ms = os.clock() * 1000                     -- 활동 — 비활동 타이머 리셋
      freeze_disc_ms = nil                                    -- 응답 수신 = 재접속됨 → giveup 타이머 리셋
      local act = handle_in_freeze(line)
      if act == "resume" then STATE = "running"; freeze_start_ms = nil; emu.resume(); return end
      if act == "step" then do_step_chunk(); return end       -- 첫 청크 시작
    elseif not conn then
      -- B3: 연결끊김(/mcp 재연결=서버 재시작 등)이면 freeze를 유지한 채 재접속을 시도한다(장면 보존 —
      -- 즉시 resume하면 공들인 장면을 흘려버린다). 포트영속 서버는 같은 포트로 돌아오므로 connect가
      -- 성공하고 다음 poll_line이 hello를 받아 정상화된다.
      local now = os.clock() * 1000
      freeze_disc_ms = freeze_disc_ms or now
      -- giveup: 서버가 영영 안 오면 무한 frozen 방지로 resume(최후수단). 0이면 무기한 유지.
      if RECONNECT_GIVEUP_MS > 0 and now - freeze_disc_ms >= RECONNECT_GIVEUP_MS then
        freeze_disc_ms = nil; STATE = "running"; freeze_start_ms = nil; emu.resume(); return
      end
      -- 재접속은 0.5s마다만 시도(throttle) — 매 codeBreak connect 폭주 방지.
      if now - last_reconnect_ms >= 500 then last_reconnect_ms = now; connect() end
      -- 워치독: step(1)은 1명령 드리프트라, 매번이 아니라 FREEZE_BUDGET_MS 마진에서만 재무장(드리프트 1/800ms로
      -- 최소화 — 장면 보존이 목적). 그 전엔 return하지 않고 스핀을 계속(다음 반복서 poll_line이 hello를 받음).
      if now - cb_start_ms >= FREEZE_BUDGET_MS then emu.step(1, emu.stepType.step); return end
    elseif freeze_reason ~= "hotkey" and (os.clock() * 1000 - freeze_start_ms) >= MAX_FREEZE_MS then
      -- B4 데드맨(명령 없이 경과 → 자동 resume). 단 hotkey freeze는 제외: 사용자가 Home으로
      -- 직접 건 무기한 hold라, human-in-loop 검사 중 명령 간격이 30s를 넘으면 데드맨이 끼어들어
      -- 데모가 진행돼버린다. hotkey는 같은 키 토글
      -- resume·에이전트 resume·연결끊김(B3)으로만 끝낸다. (pause/breakpoint freeze는 종전대로 데드맨.)
      STATE = "running"; freeze_start_ms = nil; emu.resume(); return
    elseif (os.clock() * 1000 - cb_start_ms) >= FREEZE_BUDGET_MS then
      -- 워치독 회피: codeBreak 재무장. step(1)은 1명령 전진(드리프트). breakExecution 재무장도
      -- 0드리프트가 아니라(서비스하려면 CPU가 조금 돌아야 함) step(1) 유지. 히트 순간의 정확한
      -- 상태가 필요하면 set_breakpoint의 snapshot으로 atomic 캡처하라(드리프트·데드맨 면역).
      emu.step(1, emu.stepType.step); return
    end
  end
end, emu.eventType.codeBreak)

-- 입력 적용: ROM이 읽기 직전인 inputPolled에서 주입한 입력을 덮어쓴다.
emu.addEventCallback(function()
  apply_input()
end, emu.eventType.inputPolled)

-- RUNNING 프레임 루프
emu.addEventCallback(function()
  frame = frame + 1
  if not conn then connect(); return end
  -- frozen이면(또는 step 청크 진행 중) 명령은 codeBreak가 서비스한다. 여기선 아무 것도 안 함.
  if STATE == "frozen" then return end
  -- 로컬 freeze 핫키(running 한정, 라이징 에지 1회): 사용자가 GUI Pause 대신 이 키로 그 프레임을
  -- 얼린다 → codeBreak freeze라 emucap이 응답을 유지(read/screenshot/get_state/step 가능). 지연
  -- 명령(run_frames 등) 중엔 그 응답이 묶여 있으니 건드리지 않는다(끝난 뒤 다시 누르면 됨).
  do
    local fk = freeze_key_down()
    -- 라이징 에지에 freeze. 지연 명령(run_frames/press_buttons) 중에도 막지 않고 그걸 마무리(flush)한 뒤
    -- 얼린다.
    if fk and not prev_freeze_key then
      prev_freeze_key = fk
      if deferred then flush_deferred("interrupted", "hotkey") end
      if #events < EVENT_CAP then
        events[#events + 1] = { type = "user_freeze", reason = "hotkey", frame = frame, pc = full_pc(emu.getState()) }
      else dropped = dropped + 1 end
      STATE = "frozen"; freeze_reason = "hotkey"
      pcall(emu.drawString, 8, 8, "emucap FROZEN (hotkey)", 0xFFFFFF, 0x000000, 0, 180)
      emu.breakExecution()
      return
    end
    prev_freeze_key = fk
  end
  -- 지연 명령(run_frames/press_buttons) 진행 중이면 그것만 진행(에이전트는 대기 중).
  if deferred then tick_deferred(); return end
  local line = poll_line()
  if line then dispatch(line) end
end, emu.eventType.startFrame)

CPU = emu.cpuType.snes   -- 브레이크포인트/세이브스테이트 exec 콜백용(콘솔 변경 시 함께)

if emu.getScriptDataFolder() == "" then
  emu.displayMessage("emucap", "I/O 접근 꺼짐 — Script Settings에서 켜야 함")
end
emu.log("emucap-live(능동) 로드됨: " .. HOST .. ":" .. PORT)

-- 콜드부팅 1회성 DMA 포착(EMUCAP_PREARM): soft reset로는 재현 안 되는 전원ON 1회 DMA(예: OBJ 폰트
-- 로드)는 BP를 부팅 '전'에 무장해야 잡힌다. 이 환경변수가 있으면 스크립트 로드(=ROM 부팅 직전)에 dma
-- 캡처를 사전무장한다. Mesen을 새로 띄우면(=콜드부팅, RAM 클리어 → init 플래그 초기화 → 1회 DMA 재발생)
-- 그 부팅 DMA가 BP 활성 상태로 발화해 events에 버퍼되고, 에이전트가 연결 후 poll_events로 회수한다.
-- 형식: EMUCAP_PREARM="dma" | "dma:<dest>" | "dma:<dest>:<vmin>-<vmax>"  (dest=B-bus low, 24=VRAM)
do
  local prearm = os.getenv("EMUCAP_PREARM")
  if prearm and prearm:match("^dma") then
    local dest = tonumber(prearm:match("^dma:(%d+)"))
    local vmin, vmax = prearm:match(":%d+:(%d+)%-(%d+)")
    local pok, ok, info = pcall(handlers.set_breakpoint, {
      kind = "dma", start = 0x420B, value = dest,
      pc_min = vmin and tonumber(vmin) or nil, pc_max = vmax and tonumber(vmax) or nil,
      pause_on_hit = false,
    })
    if pok and ok == true then
      emu.log("[emucap] pre-arm dma 캡처 활성(bp " .. tostring(info.id) .. "): " .. prearm)
    else
      emu.log("[emucap] pre-arm dma 실패: " .. prearm .. " (" .. tostring(info) .. ")")
    end
  end
end
