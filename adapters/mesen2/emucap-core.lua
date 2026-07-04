-- 이 파일은 제네릭 코어다. 시스템별 엔트리 스크립트(emucap-snes.lua/emucap-sms.lua)가 전역 SYS를
-- 설정한 뒤 dofile(이 파일)한다. SYS는 buttons/aliases/system/system_label/cpu_type/default_memtype/
-- reset_vector/bank_mirror/dump_regions/region_sizes를 담는다.
assert(SYS and SYS.buttons and SYS.cpu_type and SYS.default_memtype,
  "emucap-core: 전역 SYS config가 없다 — 엔트리 스크립트에서 SYS를 설정하고 dofile 하라")
assert(SYS.snapshot_regs,
  "emucap-core: SYS.snapshot_regs가 없다 — 엔트리가 snapshot_regs를 설정해야 한다")
-- disassemble/op_is_call/op_is_return은 optional이다. Lua ISA 디코더와 SP기반 콜스택 모델이 맞는
-- 시스템(SNES=65816·GG=Z80·GB=SM83)만 제공한다. ARM(GBA)처럼 디코더가 크고 콜스택이 LR기반이라 코어의
-- SP모델과 안 맞는 ISA는 이 셋을 비우면 disassemble·call_stack이 미구현(TODO — 고칠 것)로 광고·거부된다.
local HAS_DISASM = SYS.disassemble ~= nil
local HAS_CALLSTACK = (SYS.op_is_call ~= nil) and (SYS.op_is_return ~= nil)

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
-- 들여다볼 땐 짧을 수 있어 env로 조정(EMUCAP_DEADMAN_MS). **0 이하면 데드맨 비활성**(무기한 hold —
-- hotkey freeze처럼; BP 히트 후 오래 검사할 때). hotkey freeze는 값과 무관하게 항상 데드맨 면제.
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
-- frozen 중 get_state를 서빙하는 freeze 시점 스냅샷. freeze 진입 첫 codeBreak에서(트레드밀 step(1) 재무장 전)
-- 한 번 캡처한다 — lazy(첫 get_state)로 잡으면 그 사이 재무장 step(1) 드리프트가 섞인다. live emu.getState()는
-- 비싸 매 서빙마다 부르면 codeBreak 예산(FREEZE_BUDGET)을 넘겨 step(1) 드리프트를 유발하므로, frozen에선 이
-- 스냅샷에서 서빙한다. 명시적 step/resume에서만 무효화 → baseline 트레드밀 step(1)엔 유지돼 안정 레퍼런스가 된다.
local freeze_snapshot = nil
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
local READ_CAP = 0x20000       -- read_memory 상한(워치독 안전: 대량 읽기가 emu 스레드를 초단위 블록하지 않게 — find_pattern SCAN_CAP과 동형)
local WATCH_REG_BUDGET = 1000000  -- watch_register 자동해제 기본 예산(명령 수): full-range exec+매명령 getState라 무기한이면 emu 스레드를 굶긴다. p.max_instructions로 조정.
-- VRAM 재구성 BP 자동해제 예산: watch_register(매 명령 getState라 1M 필수)보다 크다. 매 명령 비용은 opcode
-- 비교뿐이고 getState는 VRAM 쓰기마다만(빈도는 게임이 정함, ~수천/초 — 매 명령 flood 아님)이라 예산은 peak를
-- 안 바꾸고 never-hit BP의 무장 지속시간만 정한다. 1M(≈1초)은 쓰기를 유발하기 전에 만료돼 무용하고, 과하게
-- 크면 잊힌 never-hit BP가 오래 emu를 느리게 둔다. 100M ≈ ~2분 게임시간이 헌팅엔 넉넉하고 backstop도 적당하다.
-- pause_on_hit면 첫 히트에서 freeze라 예산은 "대상이 영영 안 쓰이는" 경우에만 물린다. p.max_instructions로 조정.
local VRAM_RECON_BUDGET = 100000000
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
local VALID_BUTTONS = SYS.buttons
local BUTTON_ALIASES = SYS.aliases
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
    return nil, "unknown " .. SYS.system_label .. " button(s): " .. table.concat(unknown, ", ")
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
  -- disassemble/call_stack은 ISA 구현(SYS.disassemble·op_is_call/op_is_return)이 있을 때만 advertise한다.
  -- GBA처럼 미제공이면 methods에서 빠져 status.methods에 안 뜨고, 호출 시 handler가 unsupported로 거부한다.
  local method_list = { "read_memory", "screenshot", "get_state", "get_rom_info", "status",
                "write_memory", "set_input", "pause", "step", "resume",
                "run_frames", "press_buttons", "save_state", "load_state",
                "set_breakpoint", "watch_register", "clear_breakpoint", "list_breakpoints",
                "clear_all_breakpoints", "poll_events", "set_trace", "get_trace",
                "break_on_reset", "dump_memory", "find_pattern", "probe", "reset" }
  if HAS_DISASM then method_list[#method_list + 1] = "disassemble" end
  if HAS_CALLSTACK then method_list[#method_list + 1] = "call_stack" end
  local result = {
    protocol_version = PROTOCOL_VERSION,
    system = SYS.system,
    adapter = "mesen2-live",
    build = os.getenv("EMUCAP_BUILD_HASH") or "unknown",  -- launch가 넘긴 emucap git hash(status.emulator_build)
    methods = method_list,
  }
  -- memory_types: read_memory가 받는 memory_type = emu.memType의 키 전체. 정적 추측이 아니라 Mesen
  -- API의 실제 메모리 타입을 런타임 열거해 advertise한다(능력 발견). MCP가
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
  local length = p.length or 0
  if length < 0 then length = 0 end
  -- 워치독 안전 상한: 멀티MB 읽기는 emu 스레드를 초단위 블록해 소켓을 굶긴다(find_pattern SCAN_CAP과 동형).
  -- 넘치면 조용히 자르지 않고 에러로 거부한다 — truncated+ok로 부분 데이터를 성공처럼 돌려주면 검증
  -- consumer(observe/regression)가 hex만 읽어 prefix만 해시/비교해 거짓 pass/fail을 낼 수 있다.
  -- 큰 영역은 dump_memory를 쓰거나 READ_CAP 이하로 나눠 읽는다.
  if length > READ_CAP then
    return false, "bad_params",
      string.format("read_memory length %d가 상한 %d 초과 — dump_memory를 쓰거나 나눠 읽어라", length, READ_CAP)
  end
  local out = {}
  for i = 0, length - 1 do
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

-- frozen이면 freeze 시점 스냅샷에서, running이면 live로 상태를 준다. 스냅샷은 freeze 진입 첫 codeBreak에서
-- 잡히므로(위 freeze_snapshot 주석) 드리프트 없는 freeze 지점 상태다. 아래 `if not freeze_snapshot`은 방어적
-- fallback(어떤 이유로 codeBreak 전에 get_state가 오면) — 정상 경로에선 이미 채워져 있다.
local function frozen_state()
  if STATE == "frozen" then
    if not freeze_snapshot then freeze_snapshot = emu.getState() end
    return freeze_snapshot
  end
  return emu.getState()
end

-- get_state는 전 상태(레지스터·DMA·PPU·SPC 등 수백 필드)를 돌려준다. groups를 주면 키의
-- 그룹 prefix(첫 "." 앞)로 걸러 토큰 비용을 줄인다. 예: groups=["cpu","ppu"]. frozen이면 freeze 시점
-- 스냅샷을 서빙해 get_state發 드리프트를 없앤다(baseline 트레드밀 드리프트는 남음 — 진짜 0-드리프트는 fork 필요).
function handlers.get_state(p)
  local st = frozen_state()
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

-- 백스톱: freeze(브레이크포인트 등)가 진행 중 지연 명령(press/run_frames/probe)을 가로채면
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

-- full-range exec 콜백(save/load/probe·watch_register·set_trace)의 상한. 대부분 24비트(0xFFFFFF)면 CPU 실행
-- 주소를 다 덮지만, GBA(ARM7)는 카트ROM 0x08000000·EWRAM 0x02000000에서 실행하므로 24비트 콜백은 절대
-- 발화하지 않는다(save_state가 영영 안 끝남). SYS.exec_range_max로 32비트 주소공간을 덮게 한다(SNES/GG/GB는
-- 미설정 → 0xFFFFFF 그대로, 무영향).
local EXEC_MAX = SYS.exec_range_max or 0xFFFFFF

-- ── 세이브스테이트 (createSavestate/loadSavestate는 exec 콜백 컨텍스트 필요) ──
local IO_LO, IO_HI = 0, EXEC_MAX
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
-- 실행 주소. pc 필터·이벤트 기록에 쓴다. SNES=뱅크(k)*0x10000+pc, GG/GB=pc(뱅크 없음).
-- ARM(GBA)은 cpu.pc가 없고 PC가 r15이므로 폴백한다(없으면 이벤트 pc가 0으로 눕는다).
local function full_pc(st)
  local pc = st["cpu.pc"]
  if pc == nil then pc = st["cpu.r15"] end
  return (st["cpu.k"] or 0) * 65536 + (pc or 0)
end

-- hex 허용 숫자 파서(snapshot 스펙 문자열 내 주소용): 0x/$/10진.
local function snum(s)
  s = tostring(s):gsub("^%s*(.-)%s*$", "%1")
  if s:match("^%$") then return tonumber(s:sub(2), 16) end
  if s:match("^0[xX]") then return tonumber(s:sub(3), 16) end
  return tonumber(s)
end

local function record_hit(bp, addr, value)
  -- 핫 BP 플러드 가드: pause_on_hit=false BP가 이벤트 버퍼(EVENT_CAP)를 채운 뒤엔 매 히트마다 비싼
  -- emu.getState()를 부르지 않고 즉시 드롭한다. 프레임당 수천 번 실행되는 핫 주소에 exec/write BP를
  -- 걸면 getState 플러드가 emu 스레드를 stall시켜 소켓을 굶기고 연결이 끊기던 문제를 막는다. freeze BP는
  -- 첫 히트에서 STATE=frozen이 되어 스스로 멈추므로(더는 실행 안 함) 이 가드에서 제외한다.
  if #events >= EVENT_CAP and not bp.pause_on_hit then
    dropped = dropped + 1
    return
  end
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
    -- write BP가 시스템 데이터 포트에 걸렸으면 목적지 주소를 이벤트에 라벨링(런타임 타일맵 추적: CPU의
    -- 소량 직접 포트 쓰기가 "어느 워드주소로 갔나"를 PC·값과 함께 답하게). 포트 의미는 ISA별이라
    -- SYS.port_semantics로 위임 — SNES만 $2118/$2122/$2104를 라벨하고, 없는 시스템(GG 등)은 평범한 메모리
    -- 접근이라 아무 것도 안 붙인다(SNES 하드코딩 누수 제거).
    if bp.kind == "write" and SYS.port_semantics then
      SYS.port_semantics(ev, addr, st)
    end
    -- 히트 순간 atomic 스냅샷: freeze 후 read 사이 워치독-회피 step(1) 드리프트(+데드맨)로 ZP 등
    -- 명령단위 상태가 호출마다 변해 "히트 순간"을 못 잡는다. 그래서 히트 시점에 레지스터(항상)와
    -- set_breakpoint의 snapshot 스펙(mt:addr:len) 메모리를 여기서 잡아 이벤트에 실어 보존 → 이후 드리프트 무관.
    -- 레지스터 세트는 ISA별이라 SYS.snapshot_regs로 위임(SNES=65816, GG=Z80). pc는 두 ISA 공통(full_pc가
    -- 뱅크 없는 Z80에선 cpu.pc로 축약).
    ev.regs = SYS.snapshot_regs(st)
    ev.regs.pc = full_pc(st)
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
    flush_deferred("interrupted", "breakpoint", bp.id)   -- 진행 중 지연 명령 마무리
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
  local budget = math.max(1, p.max_instructions or WATCH_REG_BUDGET)
  local bp = {
    id = id, kind = "reg", register = p.register or "sp",
    min = p.min or 0, max = p.max or 0xffff, pause_on_hit = p.pause_on_hit,
    cbtype = emu.callbackType.exec, start = 0, end_ = EXEC_MAX,
    seen = 0, budget = budget,
  }
  bp.ref = emu.addMemoryCallback(function(addr, value)
    -- 플러드 가드: 버퍼가 차고 비-pausing이면 매-명령 getState 전에 즉시 드롭.
    if #events >= EVENT_CAP and not bp.pause_on_hit then dropped = dropped + 1; return end
    -- 자동 해제: full-range exec가 매 명령 getState라, 레지스터가 범위 안이면 이벤트도 안 쌓여 위 가드가
    -- 안 걸린 채 무기한 emu 스레드를 굶긴다. 명령 예산을 넘으면 스스로 콜백을 떼고(hunting 전용)
    -- watch_disarmed 이벤트를 남긴다 → 조용한 stall이 아니게. 더 오래 감시하려면 max_instructions를 키우거나
    -- 다시 무장. (자기 콜백 내 removeMemoryCallback은 Mesen에서 안전 — on_io_exec와 동일 패턴.)
    bp.seen = bp.seen + 1
    if bp.seen > bp.budget then
      emu.removeMemoryCallback(bp.ref, bp.cbtype, bp.start, bp.end_, CPU)
      breakpoints[id] = nil
      if #events < EVENT_CAP then
        events[#events + 1] = { type = "watch_disarmed", breakpoint_id = id, register = bp.register,
          reason = "instruction_budget", instructions = bp.budget, frame = frame }
      else dropped = dropped + 1 end
      return
    end
    local v = emu.getState()[reg]
    if v and (v < bp.min or v > bp.max) then record_reg_hit(bp, addr, v) end
  end, bp.cbtype, bp.start, bp.end_, CPU)
  breakpoints[id] = bp
  return true, { id = id, max_instructions = budget }
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
  -- 상위바이트 읽기용 memory_type. 버스주소로 변환한 BP는 addr이 버스주소이므로 RAM-상대
  -- memory_type(예 smsWorkRam)으로 addr+i를 읽으면 범위 밖을 읽는다 — 버스 memtype으로 읽어야 한다.
  local mt = (bp.bus_translated and emu.memType[SYS.default_memtype])
    or emu.memType[bp.memory_type] or emu.memType[SYS.default_memtype]
  local v = value
  for i = 1, bp.value_len - 1 do
    v = v + emu.read(addr + i, mt, false) * (256 ^ i)
  end
  return v
end

-- DMA 채널 스냅샷: MDMAEN($420B) 비트로 활성 채널의 src/dest/size/mode를 get_state에서 읽는다.
-- DMA/HDMA는 CPU를 우회해 read/write BP가 못 잡으므로(NMI/VBlank 그래픽 전송), 이걸로 "무엇이
-- 어디로 전송됐나"를 포착한다. MDMAEN write는 CPU 명령(STA $420B)이라 write 콜백으로 잡힌다.
local function dma_snapshot(st, mdmaen)
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

-- VRAM write BP 재구성: memtype이 CPU 버스에 없어(SMS/GG VDP는 데이터포트 OUT) Mesen memory 콜백이 못 잡는
-- 시스템에서, full-range exec 콜백으로 데이터포트 write를 감지해 VRAM 주소 BP를 지원한다. 감지·주소는
-- SYS.vram_write_target(pc,opcode)가 준다(ISA별). per-instruction이라 watch_register처럼 budget으로 자동해제한다
-- (hunting 전용 — 끝나면 clear). pause_on_hit면 히트에서 freeze. value 필터(value_len=1)는 데이터바이트에 적용.
local function setup_vram_recon_bp(bp, budget)
  bp.is_vram_recon = true
  bp.cbtype = emu.callbackType.exec
  bp.recon_lo, bp.recon_hi = 0, EXEC_MAX
  bp.seen, bp.budget = 0, budget
  bp.ref = emu.addMemoryCallback(function(pc, opcode)
    -- freeze 중(Mesen 1초 워치독 회피용 codeBreak 재무장의 step 드리프트)엔 아무 것도 안 한다: per-instruction
    -- 콜백이 드리프트 명령에 재히트하거나 예산을 태우지 않게. BP는 무장을 유지하고 resume 때 다시 작동한다.
    if STATE == "frozen" then return end
    if #events >= EVENT_CAP and not bp.pause_on_hit then dropped = dropped + 1; return end
    bp.seen = bp.seen + 1
    if bp.seen > bp.budget then            -- 예산 초과: 자동해제(무기한 per-instruction으로 emu 스레드 굶김 방지)
      emu.removeMemoryCallback(bp.ref, bp.cbtype, bp.recon_lo, bp.recon_hi, CPU)
      breakpoints[bp.id] = nil
      if #events < EVENT_CAP then
        events[#events + 1] = { type = "watch_disarmed", breakpoint_id = bp.id, kind = "write",
          reason = "instruction_budget", instructions = bp.budget, frame = frame }
      else dropped = dropped + 1 end
      return
    end
    local va, data = SYS.vram_write_target(pc, opcode)
    if va and va >= bp.start and va <= bp.end_ then
      if bp.has_value then
        local v = access_value(bp, va, data)
        if band(v, bp.value_mask) ~= band(bp.value, bp.value_mask) then return end
      end
      record_hit(bp, va, data)
    end
  end, bp.cbtype, bp.recon_lo, bp.recon_hi, CPU)
end

function handlers.set_breakpoint(p)
  local id = next_bp_id; next_bp_id = next_bp_id + 1
  -- NMI/IRQ: 메모리 접근이 아니라 이벤트(인터럽트 진입). exec BP가 못 잡는 NMI/VBlank 컨텍스트를
  -- 그 진입에서 freeze해 핸들러 상태를 검사·step하게 한다.
  if p.kind == "nmi" or p.kind == "irq" then
    local evtype = (p.kind == "nmi") and emu.eventType.nmi or emu.eventType.irq
    local bp = { id = id, kind = p.kind, is_event = true, evtype = evtype, pause_on_hit = p.pause_on_hit }
    bp.ref = emu.addEventCallback(function()
      -- 플러드 가드: 스캔라인 IRQ는 프레임당 ~224회다. 버퍼가 차고 비-pausing이면 비싼 getState
      -- 전에 즉시 드롭. pausing BP는 첫 히트에서 freeze해 스스로 멈추므로 예외.
      if #events >= EVENT_CAP and not bp.pause_on_hit then dropped = dropped + 1; return end
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
    -- 능력 게이트: dma BP는 SNES MDMAEN($420B) 컨트롤러 전용이다. 그런 DMA가 없는 시스템(GG/Z80 등)은
    -- 미구현(TODO — 고칠 것)를 낸다(break_on_reset의 `if not SYS.reset_vector` 패턴 — garbage 대신 명시적 에러).
    if not SYS.dma_supported then return false, "unsupported", "dma breakpoints not supported for " .. SYS.system end
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
      -- 플러드 가드: DMA write는 프레임마다 발생한다. 버퍼가 차고 비-pausing이면 dma_snapshot의
      -- getState 전에 즉시 드롭.
      if #events >= EVENT_CAP and not bp.pause_on_hit then dropped = dropped + 1; return end
      local st = emu.getState()          -- 히트당 1회: dma_snapshot과 pc 라벨이 같은 스냅샷을 공유(중복 getState 제거).
      local chans = dma_snapshot(st, value)
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
          channels = as_array(chans), pc = full_pc(st), frame = frame }
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
  -- 잘못된 BP 주소 등록 방지: read/write BP는 CPU-버스 주소로 콜백을 단다 — addMemoryCallback은 memory_type을
  -- 콜백 등록에 쓰지 않는다. 그래서 RAM memory_type의 상대 offset을 주면(예 GG smsWorkRam:0x0B) 버스 0x000B(ROM)에
  -- 등록돼 영원히 미발동한다(read_memory는 같은 인자로 WRAM offset을 읽어 조용한 불일치). SYS.bp_bus_base에
  -- 등록된 memory_type만 버스 base를 더해 실제 버스주소로 변환(smsWorkRam:0x0B → 0xC00B)해 발화하게 한다.
  -- 이미 버스인 memory_type(smsMemory:0xC00B)·SNES(맵이 비어 identity)는 그대로.
  if (p.kind == "read" or p.kind == "write") and SYS.bp_bus_base then
    local base = SYS.bp_bus_base[bp.memory_type]
    -- 변환하면 콜백 addr이 버스주소가 된다. value_len>1의 상위바이트를 읽을 때 RAM-상대 memory_type이
    -- 아니라 버스 memtype으로 읽어야 하므로 표시해 둔다(access_value 참조).
    if base then bp.start = bp.start + base; bp.end_ = bp.end_ + base; bp.bus_translated = true end
  end
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
  -- non-CPU-버스 memtype 라우팅(조용한 실패 제거): VDP VRAM/CRAM 등은 CPU 버스에 없어 Mesen memory
  -- 콜백이 절대 안 잡는다(실측). SYS.non_bus_write_memtypes로 (a) 재구성 경로 또는 (b) 정직한 에러로 보낸다 —
  -- 조용히 ROM 주소에 걸려 영영 미발동하던 것을 막는다. 선언 안 한 시스템(SNES 등)은 종전 CPU-버스 경로 그대로.
  if (p.kind == "write" or p.kind == "read") and SYS.non_bus_write_memtypes then
    local disp = SYS.non_bus_write_memtypes[bp.memory_type]
    if disp == "vram_recon" then
      if p.kind ~= "write" then
        return false, "unsupported", bp.memory_type .. " read BP 재구성 미구현(write만) — status.methods 참조"
      end
      local budget = math.max(1, p.max_instructions or VRAM_RECON_BUDGET)
      setup_vram_recon_bp(bp, budget)
      breakpoints[id] = bp
      return true, { id = id, mechanism = "vdp_write_reconstruction", max_instructions = budget }
    elseif disp then
      return false, "unsupported", bp.memory_type
        .. "은 CPU 버스에 없어(VDP 포트 write) memory " .. p.kind .. " BP로 못 잡는다 — 재구성 미구현(TODO). status.methods 참조"
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
  if SYS.bank_mirror and (p.kind == "read" or p.kind == "write") and p.memory_type == SYS.default_memtype
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
  elseif bp.is_vram_recon then
    emu.removeMemoryCallback(bp.ref, bp.cbtype, bp.recon_lo, bp.recon_hi, CPU)
  elseif bp.kind == "reg" then
    emu.removeMemoryCallback(bp.ref, bp.cbtype, bp.start, bp.end_, CPU)   -- watch_register: 단일 full-range exec ref(mirror 없음)
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
    elseif bp.is_vram_recon then
      emu.removeMemoryCallback(bp.ref, bp.cbtype, bp.recon_lo, bp.recon_hi, CPU)
    elseif bp.kind == "reg" then
      emu.removeMemoryCallback(bp.ref, bp.cbtype, bp.start, bp.end_, CPU)
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
  if not SYS.reset_vector then return false, "unsupported", "break_on_reset not supported for " .. SYS.system end
  local on = p.enabled and true or false
  if on and not reset_bp then
    local handler = emu.read16(SYS.reset_vector, emu.memType[SYS.default_memtype], false)  -- 리셋벡터
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
  if type(op) ~= "number" then op = emu.read(addr, emu.memType[SYS.default_memtype], false) end
  trace_ring[(trace_idx % TRACE_CAP) + 1] = { pc = addr, op = op }
  trace_idx = trace_idx + 1
  -- 콜스택 shadow-track은 op_is_call/op_is_return이 있는 ISA만 갱신한다(GBA엔 없어 트레이스 링만 채운다).
  if HAS_CALLSTACK and SYS.op_is_call(op) then         -- 호출 명령: 호출지 push(SP 정합 후)
    local sp = emu.getState()["cpu.sp"]
    reconcile_callstack(sp)                            -- 이미 리턴한 프레임(JMP-리턴 포함) 정리
    callstack[#callstack + 1] = { pc = addr, sp = sp }
  elseif HAS_CALLSTACK and SYS.op_is_return(op) then   -- 리턴 명령: pop
    if #callstack > 0 then table.remove(callstack) end
  end
end

-- 실행추적 on/off. 켜면 매 명령 콜백 등록(느림 — hunting 전용). 켤 때 링·콜스택 초기화.
function handlers.set_trace(p)
  local on = p.enabled and true or false
  if on and not trace_on then
    trace_ring = {}; trace_idx = 0; callstack = {}
    trace_ref = emu.addMemoryCallback(trace_cb, emu.callbackType.exec, 0, EXEC_MAX, CPU)
    trace_on = true
  elseif not on and trace_on then
    emu.removeMemoryCallback(trace_ref, emu.callbackType.exec, 0, EXEC_MAX, CPU)
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
  if not HAS_CALLSTACK then
    return false, "unsupported", "call_stack not supported for " .. SYS.system .. " (no SP-based call-stack model for this ISA)"
  end
  reconcile_callstack(emu.getState()["cpu.sp"])
  local out = {}
  for _, f in ipairs(callstack) do out[#out + 1] = f.pc end
  return true, { call_stack = as_array(out), depth = #callstack, tracing = trace_on }
end

-- ── 메모리 덤프 (emucap diff 입력) ───────────────────────────
-- 표준 리전을 .bin과 regions.json으로 디렉토리에 쓴다. 콘솔 변경 시 목록을 바꾼다.
local DUMP_REGIONS = SYS.dump_regions

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
local REGION_SIZE = SYS.region_sizes
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

-- ── 디스어셈블러 (ISA별 — SYS 위임) ─────────────────────────
-- Mesen2 Lua엔 디스어셈블 API가 없어 디코더를 직접 구현한다. ISA 로직은 코어에 두지 않고
-- 엔트리(emucap-snes=65816, emucap-sms=Z80)가 SYS.disassemble로 제공한다. 코어는 memory_type을
-- 해석해 read_byte 클로저를 넘기고, ISA는 명령 경계·니모닉을 결정한다.

-- disassemble(address, count): 실행주소에서 count개 명령. 반환 [{addr,text,bytes}] — Mednafen과 같은 형태.
-- read_byte(addr)=emu.read(addr, mt, false)(mt는 p.memory_type 또는 SYS.default_memtype).
function handlers.disassemble(p)
  if not HAS_DISASM then
    return false, "unsupported", "disassemble not supported for " .. SYS.system .. " (no Lua ISA decoder for this system)"
  end
  local addr = p.address or 0
  local count = math.max(1, math.min(p.count or 8, 256))
  local mt = emu.memType[p.memory_type] or emu.memType[SYS.default_memtype]
  local read_byte = function(x) return emu.read(x, mt, false) end
  return true, as_array(SYS.disassemble(read_byte, addr, count))
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
  -- 명시적 step은 위치를 전진시키므로 freeze 스냅샷을 무효화 → 다음 get_state가 새 위치에서 재캡처.
  freeze_snapshot = nil
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
  if not freeze_start_ms then
    freeze_start_ms = os.clock() * 1000
    -- freeze 시점 스냅샷을 이 첫 codeBreak에서 잡는다 — 트레드밀 step(1) 재무장(아래 FREEZE_BUDGET 분기)과
    -- 이번 콜백의 예산 타이머(cb_start_ms) 시작 전이라, CPU가 아직 freeze 지점에 있다. lazy(첫 get_state) 캡처면
    -- freeze~첫 get_state 사이에 재무장 step(1)이 끼어 드리프트가 섞이므로 여기서 선캡처한다. getState는 한 번뿐이라
    -- 예산에도 안 걸린다. step/resume에서만 무효화(freeze_snapshot=nil)해 baseline 트레드밀엔 유지된다.
    if not freeze_snapshot then freeze_snapshot = emu.getState() end
  end
  local cb_start_ms = os.clock() * 1000  -- 이번 콜백 진입(워치독 회피용 — 매 진입 리셋)
  local last_key_ms = 0                   -- freeze 핫키 폴 throttle(스핀이 빨라 매 반복 폴은 과함)
  while true do
    -- 워치독 회피(명령 버스트): poll_line이 명령을 연속으로 반환하면 아래 `if line` 분기만 반복돼 예산
    -- 재무장 검사(elseif)에 도달하지 못한 채 누적 실행이 1초 워치독을 넘겨 스크립트가 죽는다(poll_line에서
    -- "Maximum execution time exceeded"). 최상단에서 명령 유무와 무관하게 예산 초과면 codeBreak를 재무장하고
    -- 반환한다 — 큐에 남은 명령은 다음 codeBreak 진입에서 이어 서비스한다(스텝 1 드리프트는 기존 재무장과 동일).
    if (os.clock() * 1000 - cb_start_ms) >= FREEZE_BUDGET_MS then
      emu.step(1, emu.stepType.step); return
    end
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
          STATE = "running"; freeze_start_ms = nil; freeze_snapshot = nil; emu.resume(); return
        end
        prev_freeze_key = fk
      end
    end
    local line = poll_line()
    if line then
      freeze_start_ms = os.clock() * 1000                     -- 활동 — 비활동 타이머 리셋
      freeze_disc_ms = nil                                    -- 응답 수신 = 재접속됨 → giveup 타이머 리셋
      local act = handle_in_freeze(line)
      if act == "resume" then STATE = "running"; freeze_start_ms = nil; freeze_snapshot = nil; emu.resume(); return end
      if act == "step" then do_step_chunk(); return end       -- 첫 청크 시작
    elseif not conn then
      -- 연결끊김(/mcp 재연결=서버 재시작 등)이면 freeze를 유지한 채 재접속을 시도한다(장면 보존 —
      -- 즉시 resume하면 공들인 장면을 흘려버린다). 포트영속 서버는 같은 포트로 돌아오므로 connect가
      -- 성공하고 다음 poll_line이 hello를 받아 정상화된다.
      local now = os.clock() * 1000
      freeze_disc_ms = freeze_disc_ms or now
      -- giveup: 서버가 영영 안 오면 무한 frozen 방지로 resume(최후수단). 0이면 무기한 유지.
      if RECONNECT_GIVEUP_MS > 0 and now - freeze_disc_ms >= RECONNECT_GIVEUP_MS then
        freeze_disc_ms = nil; STATE = "running"; freeze_start_ms = nil; freeze_snapshot = nil; emu.resume(); return
      end
      -- 재접속은 0.5s마다만 시도(throttle) — 매 codeBreak connect 폭주 방지.
      if now - last_reconnect_ms >= 500 then last_reconnect_ms = now; connect() end
      -- 워치독: step(1)은 1명령 드리프트라, 매번이 아니라 FREEZE_BUDGET_MS 마진에서만 재무장(드리프트 1/800ms로
      -- 최소화 — 장면 보존이 목적). 그 전엔 return하지 않고 스핀을 계속(다음 반복서 poll_line이 hello를 받음).
      if now - cb_start_ms >= FREEZE_BUDGET_MS then emu.step(1, emu.stepType.step); return end
    elseif freeze_reason ~= "hotkey" and MAX_FREEZE_MS > 0 and (os.clock() * 1000 - freeze_start_ms) >= MAX_FREEZE_MS then
      -- 데드맨(명령 없이 경과 → 자동 resume). 단 hotkey freeze는 제외: 사용자가 Home으로
      -- 직접 건 무기한 hold라, human-in-loop 검사 중 명령 간격이 30s를 넘으면 데드맨이 끼어들어
      -- 데모가 진행돼버린다. `EMUCAP_DEADMAN_MS<=0`(MAX_FREEZE_MS<=0)이면 데드맨 전면 비활성 —
      -- BP 히트 후 무기한 hold(에이전트 死 위험을 감수). hotkey는 같은 키 토글
      -- resume·에이전트 resume·연결끊김으로만 끝낸다. (pause/breakpoint freeze는 데드맨 활성 시.)
      STATE = "running"; freeze_start_ms = nil; freeze_snapshot = nil; emu.resume(); return
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

CPU = emu.cpuType[SYS.cpu_type]   -- 브레이크포인트/세이브스테이트 exec 콜백용

if emu.getScriptDataFolder() == "" then
  emu.displayMessage("emucap", "I/O 접근 꺼짐 — Script Settings에서 켜야 함")
end
emu.log("emucap-core(능동) 로드됨: " .. HOST .. ":" .. PORT)

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
