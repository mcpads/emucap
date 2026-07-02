-- emucap PCSX-Redux 라이브 클라이언트 (PS1) — 스파이크
-- PCSX-Redux Lua(LuaJIT FFI) + 번들 luv(libuv)로 emucap-mcp(NDJSON/TCP)에 접속한다.
-- Mednafen 포크(C++)·Mesen(Lua) 어댑터와 같은 공통 프로토콜을 서비스 — Rust 측(TcpLink·tools·MCP)
-- 무수정. 실행: pcsx-redux -no-ui -interpreter -iso <disc> -dofile <이 파일> -run
--
-- 트랜스포트가 Mesen과 다르다: luasocket 블로킹이 아니라 luv(비동기). freeze는 spin이 아니라
-- pauseEmulator()로 코어만 멈추고 luv 이벤트 루프는 계속 돌아 명령을 서비스한다(워치독 불필요).
-- 프레임 카운트·지연명령 완료는 GPU::Vsync 리스너에서 처리한다.

local uv = luv    -- PCSX-Redux는 luv(libuv)를 전역 테이블로 노출(require 아님)
-- ffi도 전역 제공

local HOST = '127.0.0.1'
local PORT = tonumber(os.getenv('EMUCAP_PORT') or '') or 47800
local PROTOCOL_VERSION = 1

local client = nil
local connected = false
local rx = ''
local frame = 0
local STATE = 'running'           -- 'running' | 'frozen'

-- 지연 명령(run_frames)·step: Vsync 리스너가 카운트다운
local deferred = nil               -- { id, kind='run', remaining }
local step_remaining = 0
local step_id = nil

-- ── JSON (Mesen 어댑터에서 포팅 — Rust 측과 검증된 프레이밍) ──
local ESC_MAP = {
  ['"'] = '\\"', ['\\'] = '\\\\', ['\b'] = '\\b', ['\f'] = '\\f',
  ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t',
}
local function esc(s)
  return (s:gsub('[%c"\\]', function(c) return ESC_MAP[c] or string.format('\\u%04x', string.byte(c)) end))
end
local ARRAY_MT = {}
local function as_array(t) return setmetatable(t or {}, ARRAY_MT) end
local function jvalue(v)
  local t = type(v)
  if t == 'number' then
    return (v == math.floor(v)) and string.format('%d', v) or tostring(v)
  elseif t == 'boolean' then
    return tostring(v)
  elseif t == 'table' then
    local n = #v
    local is_arr
    if getmetatable(v) == ARRAY_MT then is_arr = true
    else
      is_arr = n > 0
      if is_arr then local c = 0; for _ in pairs(v) do c = c + 1 end; is_arr = (c == n) end
    end
    if is_arr then
      local parts = {}; for i = 1, n do parts[i] = jvalue(v[i]) end
      return '[' .. table.concat(parts, ',') .. ']'
    end
    local parts = {}
    for k, val in pairs(v) do parts[#parts + 1] = '"' .. esc(tostring(k)) .. '":' .. jvalue(val) end
    return '{' .. table.concat(parts, ',') .. '}'
  else
    return '"' .. esc(tostring(v)) .. '"'
  end
end
local function json_decode(s)
  local i = 1
  local parse_value
  local function skip_ws() while i <= #s and s:sub(i, i):match('%s') do i = i + 1 end end
  local function parse_string()
    i = i + 1; local out = {}
    while i <= #s do
      local c = s:sub(i, i)
      if c == '"' then i = i + 1; return table.concat(out)
      elseif c == '\\' then
        local n = s:sub(i + 1, i + 1)
        local map = { ['"'] = '"', ['\\'] = '\\', ['/'] = '/', b = '\b', f = '\f', n = '\n', r = '\r', t = '\t' }
        if n == 'u' then out[#out + 1] = string.char(tonumber(s:sub(i + 4, i + 5), 16) or 0); i = i + 6
        else out[#out + 1] = map[n] or n; i = i + 2 end
      else out[#out + 1] = c; i = i + 1 end
    end
    error('unterminated string')
  end
  local function parse_number()
    local j = i
    while i <= #s and s:sub(i, i):match('[%d%.eE%+%-]') do i = i + 1 end
    return tonumber(s:sub(j, i - 1))
  end
  local function parse_object()
    i = i + 1; local obj = {}; skip_ws()
    if s:sub(i, i) == '}' then i = i + 1; return obj end
    while true do
      skip_ws(); local key = parse_string(); skip_ws(); i = i + 1  -- ':'
      obj[key] = parse_value(); skip_ws()
      local c = s:sub(i, i)
      if c == ',' then i = i + 1 elseif c == '}' then i = i + 1; return obj else error('bad obj') end
    end
  end
  local function parse_array()
    i = i + 1; local arr = {}; skip_ws()
    if s:sub(i, i) == ']' then i = i + 1; return arr end
    while true do
      arr[#arr + 1] = parse_value(); skip_ws()
      local c = s:sub(i, i)
      if c == ',' then i = i + 1 elseif c == ']' then i = i + 1; return arr else error('bad arr') end
    end
  end
  parse_value = function()
    skip_ws(); local c = s:sub(i, i)
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

-- ── 전송 ─────────────────────────────────────────────────────
local function send_line(s)
  if client and connected then client:write(s .. '\n') end
end
local function reply_ok(id, result)
  send_line(string.format('{"id":%d,"ok":true,"result":%s}', id, jvalue(result)))
end
local function reply_err(id, kind, msg)
  send_line(string.format('{"id":%d,"ok":false,"error":{"kind":"%s","message":"%s"}}', id, kind, esc(tostring(msg))))
end

-- ── 메모리 ───────────────────────────────────────────────────
-- PS1 주소 → (포인터, 오프셋). KUSEG/KSEG0/KSEG1 미러는 하위비트로 접는다.
local function mem_region(addr)
  local a = bit.band(addr, 0x1fffffff)            -- KSEG 마스킹(상위 3비트 제거)
  if a < 0x00800000 then                          -- 메인 RAM(2MB, 8MB까지 미러)
    return PCSX.getMemPtr(), bit.band(addr, 0x7fffff)
  elseif a >= 0x1f800000 and a < 0x1f800400 then  -- 스크래치패드 1KB
    return PCSX.getScratchPtr(), a - 0x1f800000
  elseif a >= 0x1fc00000 and a < 0x1fc80000 then  -- BIOS 512KB
    return PCSX.getRomPtr(), a - 0x1fc00000
  end
  return nil, 0
end

local function handle_read_memory(p)
  local ptr, off = mem_region(p.address)
  if not ptr then return false, 'bad_params', '지원 안 하는 주소 영역(ram/scratch/bios만)' end
  local out = {}
  for k = 0, (p.length - 1) do out[#out + 1] = string.format('%02x', ptr[off + k]) end
  return true, { hex = table.concat(out) }
end

local function handle_write_memory(p)
  local hex = p.hex
  if type(hex) ~= 'string' or #hex % 2 ~= 0 then return false, 'bad_params', 'hex는 짝수 길이' end
  local ptr, off = mem_region(p.address)
  if not ptr then return false, 'bad_params', '지원 안 하는 주소 영역' end
  local n = 0
  for k = 1, #hex, 2 do
    local b = tonumber(hex:sub(k, k + 1), 16)
    if not b then return false, 'bad_params', 'hex 디코드 실패' end
    ptr[off + n] = b; n = n + 1
  end
  return true, { written = n }
end

-- ── 레지스터(get_state) ──────────────────────────────────────
local GPR_NAMES = { 'r0','at','v0','v1','a0','a1','a2','a3','t0','t1','t2','t3','t4','t5','t6','t7',
  's0','s1','s2','s3','s4','s5','s6','s7','t8','t9','k0','k1','gp','sp','s8','ra','lo','hi' }
local function handle_get_state()
  local r = PCSX.getRegisters()
  local st = {}
  st['cpu.pc'] = tonumber(r.pc)
  local gpr = r.GPR.r
  for idx = 0, 33 do st['cpu.' .. GPR_NAMES[idx + 1]] = tonumber(gpr[idx]) end
  for idx = 0, 31 do st['cop0.r' .. idx] = tonumber(r.CP0.r[idx]) end
  return true, { state = st }
end

-- ── 핸들러 ───────────────────────────────────────────────────
local handlers = {}
function handlers.hello()
  local r = { protocol_version = PROTOCOL_VERSION,
    system = 'psx',
    adapter = 'pcsx-redux-lua',
    methods = as_array({ 'hello', 'status', 'read_memory', 'write_memory', 'get_state',
      'pause', 'resume', 'step', 'run_frames', 'reset' }) }
  local name = os.getenv('EMUCAP_NAME')
  if name then r.name = name end
  local token = os.getenv('EMUCAP_SESSION_TOKEN')
  if token then r.session_token = token end
  local content = os.getenv('EMUCAP_CONTENT')
  if content then r.content = content end
  return true, r
end
function handlers.status()
  return true, { connected = true, frame = frame, state = STATE }
end
handlers.read_memory = function(p) return handle_read_memory(p) end
handlers.write_memory = function(p) return handle_write_memory(p) end
handlers.get_state = function() return handle_get_state() end
function handlers.reset() PCSX.softResetEmulator(); return true, { reset = true } end

-- 디스패치. run_frames/step/pause/resume는 상태 전이라 별도.
local function dispatch(line)
  local ok, env = pcall(json_decode, line)
  if not ok or type(env) ~= 'table' then return end
  local id = env.id or 0
  local method = env.method
  local p = type(env.params) == 'table' and env.params or {}

  if method == 'pause' then
    STATE = 'frozen'; PCSX.pauseEmulator(); reply_ok(id, { state = 'frozen' }); return
  elseif method == 'resume' then
    STATE = 'running'; step_remaining = 0; step_id = nil; PCSX.resumeEmulator()
    reply_ok(id, { state = 'running' }); return
  elseif method == 'step' then
    local n = p.frames or 1; if n < 1 then n = 1 end
    step_remaining = n; step_id = id; STATE = 'running'; PCSX.resumeEmulator(); return  -- Vsync가 카운트 후 재정지
  elseif method == 'run_frames' then
    local n = p.n or 1
    if STATE == 'frozen' then STATE = 'running'; PCSX.resumeEmulator() end
    deferred = { id = id, kind = 'run', remaining = n }; return
  end

  local h = handlers[method]
  if not h then reply_err(id, 'unknown_method', tostring(method)); return end
  local hok, a, b, c = pcall(h, p)
  if not hok then reply_err(id, 'emulator_error', a)
  elseif a == true then reply_ok(id, b) else reply_err(id, b, c) end
end

-- ── 프레임 훅(GPU::Vsync): 카운트 + 지연명령/step 완료 ──
PCSX.Events.createEventListener('GPU::Vsync', function()
  frame = frame + 1
  if deferred then
    deferred.remaining = deferred.remaining - 1
    if deferred.remaining <= 0 then
      reply_ok(deferred.id, { status = 'completed', frame = frame }); deferred = nil
    end
    return
  end
  if step_remaining > 0 then
    step_remaining = step_remaining - 1
    if step_remaining <= 0 then
      STATE = 'frozen'; PCSX.pauseEmulator()
      if step_id then reply_ok(step_id, { status = 'completed', frame = frame }); step_id = nil end
    end
  end
end)

-- ── luv TCP 연결(+ 재시도) ───────────────────────────────────
local connect_timer = nil
local function try_connect()
  if connected then return end
  local c = uv.new_tcp()
  c:connect(HOST, PORT, function(err)
    if err then c:close(); return end       -- 다음 타이머에서 재시도
    client = c; connected = true; rx = ''
    c:read_start(function(rerr, chunk)
      if rerr or not chunk then               -- EOF/에러 → 끊김, 재연결
        connected = false; if client then client:close(); client = nil end
        return
      end
      rx = rx .. chunk
      while true do
        local nl = rx:find('\n')
        if not nl then break end
        local l = rx:sub(1, nl - 1); rx = rx:sub(nl + 1)
        if #l > 0 then dispatch(l) end
      end
    end)
  end)
end

connect_timer = uv.new_timer()
connect_timer:start(200, 1000, function()    -- 200ms 후 시작, 1s마다 재시도(끊기면 자동 재연결)
  if not connected then try_connect() end
end)

PCSX.log('emucap-live(PCSX-Redux) 로드됨: ' .. HOST .. ':' .. PORT)
