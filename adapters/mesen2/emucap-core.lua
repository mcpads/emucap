-- 이 파일은 제네릭 코어다. 시스템별 엔트리 스크립트(emucap-snes.lua/emucap-sms.lua)가 전역 SYS를
-- 설정한 뒤 dofile(이 파일)한다. SYS는 buttons/aliases/system/system_label/cpu_type/default_memtype/
-- reset_vector/bank_mirror/dump_regions/region_sizes를 담는다.
assert(SYS and SYS.buttons and SYS.cpu_type and SYS.default_memtype and SYS.address_space_size,
  "emucap-core: global SYS config is missing; set SYS in the entry script before loading the core")
assert(SYS.snapshot_regs,
  "emucap-core: SYS.snapshot_regs is missing; the entry script must provide it")
-- disassemble/op_is_call/op_is_return은 optional이다. Lua ISA 디코더와 SP기반 콜스택 모델이 맞는
-- 시스템(SNES=65816·GG=Z80·GB=SM83)만 제공한다. ARM(GBA)처럼 디코더가 크고 콜스택이 LR기반이라 코어의
-- SP모델과 안 맞는 ISA는 이 셋을 비우면 disassemble·call_stack을 미지원으로 광고·거부한다.
local HAS_DISASM = SYS.disassemble ~= nil
local HAS_CALLSTACK = (SYS.op_is_call ~= nil) and (SYS.op_is_return ~= nil)
local HAS_SNES_PPU_OBJ_EVENTS = SYS.system == "snes"
  and emu.eventType.snesPpuObjEvaluationStart ~= nil
  and emu.eventType.snesPpuObjScanlineHandoff ~= nil
local HAS_SNES_DEEP_EVENTS = SYS.system == "snes"
  and type(emu.getSnesObservationEvent) == "function"
  and emu.eventType.snesInstructionExecution ~= nil
  and emu.eventType.snesContentRead ~= nil
  and emu.eventType.snesTransferEnable ~= nil
  and emu.eventType.snesTransferAccess ~= nil
  and emu.eventType.snesDevicePortWrite ~= nil
  and emu.eventType.snesInterruptDelivery ~= nil
  and emu.eventType.snesPpuObjConsumptionRead ~= nil

-- emucap Mesen2 라이브 클라이언트 (능동 제어)
-- 필요 옵션: "Allow network access" + "Allow access to I/O and OS functions".
-- 먼저 emucap-mcp 서버가 떠 있어야 한다(기본 포트 47800).
--
-- freeze는 breakExecution + compatible Mesen host의 native debugger halt로 건다. codeBreak/codeBreakIdle
-- callback은 매번 bounded service 한 번만 수행해 Lua watchdog을 유지하면서 guest는 전진시키지 않는다.
-- 이 파일은 상태기계·freeze-step·읽기·쓰기·입력을 다룬다. 지연명령·세이브스테이트·
-- 브레이크포인트는 별도로 얹는다.

local socket = require("socket.core")
local Tx = require("emucap_tx")
local StateIo = require("emucap_state_io")
local Dump = require("emucap_dump")
local Deferred = require("emucap_deferred")
local FreezeState = require("emucap_freeze_state")
local Memory = require("emucap_memory")
local Recording = require("emucap_recording")

assert(emu.eventType and emu.eventType.codeBreakIdle ~= nil
    and emu.eventType.codeBreakIdleSavestate ~= nil,
  "emucap-core: this Mesen host lacks safe native-halt events; build the compatible host with adapters/mesen2/build.sh")

local HOST = "127.0.0.1"
-- 포트: 교차-ROM 2-인스턴스를 위해 EMUCAP_PORT로 덮어쓸 수 있다(없으면 47800).
local PORT = tonumber(os.getenv("EMUCAP_PORT") or "") or 47800
local PROTOCOL_VERSION = 1
local MESEN_HOST_API = 2
local MESEN_UPSTREAM_COMMIT = os.getenv("EMUCAP_MESEN_UPSTREAM_COMMIT")
local MESEN_PATCHSET_SHA256 = os.getenv("EMUCAP_MESEN_PATCHSET_SHA256")
local MESEN_BINARY_SHA256 = os.getenv("EMUCAP_MESEN_BINARY_SHA256")
local function exact_hex(value, length)
  return type(value) == "string" and #value == length and not value:find("[^0-9a-fA-F]")
end
-- Recording evidence names the exact host revision, patch stack, and produced binary. Legacy or
-- manually launched hosts retain every pre-existing method but do not advertise this capability.
local RECORDING_SUPPORTED = SYS.system == "snes"
  and exact_hex(MESEN_UPSTREAM_COMMIT, 40)
  and exact_hex(MESEN_PATCHSET_SHA256, 64)
  and exact_hex(MESEN_BINARY_SHA256, 64)
-- 데드맨은 operator opt-in이다. pause/BP 성공 뒤 agent 왕복 지연만으로 실행을 재개하면 frozen
-- persistence 계약을 깨므로 기본은 0(비활성). 양수를 명시한 launch만 그 비활동 시간 뒤 자동 resume한다.
-- hotkey freeze는 값과 무관하게 항상 데드맨 면제.
local MAX_FREEZE_MS = tonumber(os.getenv("EMUCAP_DEADMAN_MS") or "") or 0
local HALT_SERVICE_INTERVAL_MS = 10  -- native SleepUntilResume의 idle callback 최소 대기 간격
-- A zero-timeout recording socket turns normal producer/consumer scheduling jitter into a false
-- sink failure. Permit one short bounded drain interval while staying well below Mesen's one-second
-- Lua callback watchdog; an actual stalled sink still fail-stops the capture.
local RECORDING_SINK_WRITE_TIMEOUT_SECONDS = 0.1
local function wall_ms() return socket.gettime() * 1000 end
-- freeze 중 연결끊김(서버 재시작//mcp 재연결) 시 freeze를 유지한 채 재접속을 시도해 장면을 보존한다.
-- transport loss만으로 guest 실행을 바꾸지 않는 것이 기본이다. 양수를 명시한 launch만 그 시간 뒤
-- auto-resume하고, 0은 재접속까지 무기한 freeze 유지다.
local RECONNECT_GIVEUP_MS = tonumber(os.getenv("EMUCAP_RECONNECT_GIVEUP_MS") or "") or 0
local freeze_disc_ms = nil    -- freeze 중 연결끊김 시작 시각(재접속 giveup 타이머)
local last_reconnect_ms = 0   -- 마지막 재접속 시도 시각(throttle — 매 명령 connect 폭주 방지)
-- The local freeze hotkey enters the same native debugger halt used by MCP pause. Mesen already
-- transfers a regular pause into its debugger when the command-line script attaches; the compatible
-- host's recurring idle events let this adapter adopt and service that halt without further guest
-- time. EMUCAP_FREEZE_KEY changes the key (default Home); "off", "none", or "" disables it.
local FREEZE_KEY = os.getenv("EMUCAP_FREEZE_KEY")
if FREEZE_KEY == nil then FREEZE_KEY = "Home" end
do local lk = FREEZE_KEY:lower(); if lk == "" or lk == "off" or lk == "none" then FREEZE_KEY = nil end end
local freeze_key_ok = true     -- isKeyPressed가 무효 키에 에러 → 1회 보고 후 비활성
local prev_freeze_key = false  -- 라이징 에지 검출(running→freeze, frozen→resume 토글)

local STATE = "running"       -- "running" | "frozen"
local freeze_start_ms = nil   -- 마지막 명령 이후 경과 측정(데드맨). frozen 진입/명령 수신 시 갱신.
-- The current halt cause and its temporal eligibility are independent. A later step must replace
-- a recording halt's reason, while a completed frame step remains a proven recording boundary.
local freeze_state = FreezeState.halt("paused", false)
-- true only while the patched host services a main-CPU instruction-boundary halt. Frame/PPU steps,
-- read/write breakpoints, and other mid-instruction halts use the ordinary idle event and keep this
-- false, so savestate calls fail loudly instead of serializing an unsafe core state.
local halt_savestate_safe = false
-- frozen 중 get_state를 서빙하는 freeze 시점 스냅샷. freeze 진입 첫 codeBreak에서 한 번 캡처해
-- 정지 진입의 linearization point를 고정하고, 비싼 emu.getState() 직렬화를 요청마다 반복하지 않는다.
-- native halt 중 guest time은 불변이므로 명시적 step/resume에서만 이 스냅샷을 무효화한다.
local freeze_snapshot = nil
local pending_step_id = nil   -- step(n) 완료 응답 대기
local step_remaining = 0       -- step(n)의 남은 단위(청크로 나눠 진행)
local step_unit = "frames"    -- "frames"(ppuFrame) | "instructions"(stepType.step)
local STEP_CHUNK = 30          -- 프레임 step 청크(≤1s, keepalive 보장)
local INSTR_CHUNK = 20000      -- 명령 step 청크(≤1s 안에서 keepalive)
local TX_CAP = tonumber(os.getenv("EMUCAP_TX_CAP") or "") or (8 * 1024 * 1024)
if TX_CAP < 1024 then TX_CAP = 1024 end
local conn = nil
local session_epoch = 0       -- response IDs are scoped to one accepted frontend connection
local rx_buf = ""
local tx = Tx.new(TX_CAP)
local frame = 0

-- 브레이크포인트/이벤트 상태
local KEEPALIVE_FRAMES = 30
local deferred = nil           -- run_frames/press_buttons 진행 상태 { id, kind, remaining, age }
local pending_io = nil         -- save_state/load_state 진행 상태 { id, kind, path, ref }
local recording = nil          -- bounded, generation-bound host-sink recording transaction
local recording_reset = nil    -- validated/sink-bound request waiting for the native reset callback
local recording_last = nil     -- bounded terminal status only; event bytes stay in the Core bundle
local recording_hook_refs = {} -- request-owned semantic callbacks; empty on the ordinary path
local abort_inflight = nil     -- disconnect 시 새 세션에 옛 response id를 흘리지 않도록 지연 작업 폐기
local resume_from_freeze = nil -- reset reply flush에서도 frozen ownership을 반환할 수 있게 forward declaration
local reset_after_reply = false -- reset terminal response를 모두 보낸 뒤 host reset을 실행
local next_bp_id = 1
local breakpoints = {}         -- id -> { ref, kind, start, end, pause_on_hit }
local reset_bp = nil           -- break_on_reset: 리셋 핸들러 exec BP { ref, handler }
local EVENT_CAP = 256
local READ_CAP = 0x20000       -- read_memory 상한(워치독 안전: 대량 읽기가 emu 스레드를 초단위 블록하지 않게 — find_pattern SCAN_CAP과 동형)
local MAX_SYNC_ADVANCE = 5000  -- Keep one request within the transport deadline.
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
-- (call push, pop은 SP 되돌아옴으로 감지 — 스택 손상에도 robust), (b) 최근 명령 링버퍼를 채운다. 매 명령이라
-- 느리니 크래시 추적 hunting 전용. Mesen Lua엔 네이티브 콜스택 API가 없어 직접 추적한다.
-- pop을 opcode(op_is_return)로 하지 않고 SP로 한다: 조건부 RET 미성립은 opcode가 맞아도 실제 리턴 안 하고,
-- 하드웨어 인터럽트는 CALL opcode 없이 push한 걸 핸들러 RET가 pop해 진짜 프레임을 지운다. 둘 다 opcode-pop이면
-- over-pop이라 depth가 0으로 고착된다(exec 콜백은 pre-execution이라 frame.sp가 push 전 SP임을 실측 확인).
local trace_on = false
local trace_ref = nil
local TRACE_CAP = 256
local trace_ring = {}          -- 링버퍼 슬롯 -> { pc, op }
local trace_idx = 0            -- 누적 명령 수(슬롯 = (trace_idx-1)%CAP +1)
local callstack = {}           -- shadow 콜스택: 호출지(call의 pc) 리스트(바깥→안), 각 { pc, sp=호출 직전 SP }
local pending_ret_check = false -- 직전 명령이 return류(op_is_return/ED prefix)면 true — 다음 명령에서 SP로 실제 리턴 여부 확인
-- get_trace 뱅크 섀도: trace_ring 엔트리에 pc의 ROM 뱅크를 붙인다. 매 명령 getState는 비싸므로(리컨사일
-- 거부 사유) SYS.bank_write_ranges write 콜백이 banks_dirty를 세우고, trace_cb가 dirty일 때만 refresh(매퍼
-- write당 ~1회, 다음 명령에서 = write 반영 후). SYS.bank_of 없는 시스템은 cur_banks가 nil로 남아 무동작.
local cur_banks = nil          -- 현재 매핑된 뱅크 테이블(SYS.read_banks 형식), set_trace on에서 init
local banks_dirty = false      -- 매퍼 write 발생 → 다음 trace_cb에서 cur_banks refresh
local bank_cb_refs = {}         -- 등록한 write 콜백들 { ref, lo, hi }(제거 시 range 필요)

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
local function disconnect()
  local old = conn
  conn = nil
  rx_buf = ""
  Tx.reset(tx)
  if abort_inflight then abort_inflight() end
  if old then pcall(function() old:close() end) end
end

local function connect()
  local c = socket.tcp()
  c:settimeout(0)
  c:connect(HOST, PORT)
  session_epoch = session_epoch + 1
  conn = c
  rx_buf = ""
  Tx.reset(tx)
end

local function send_line(s)
  if not conn then return end
  local ok, err = Tx.enqueue(tx, s)
  if ok then return end
  emu.log("[emucap] TX " .. tostring(err) .. "; dropping the connection")
  disconnect()
end

local function flush_tx()
  if not conn or not Tx.pending(tx) then return "idle" end
  local status, err = Tx.flush(tx, conn)
  if status == "error" then
    emu.log("[emucap] TX error (" .. tostring(err) .. "); reconnecting")
    disconnect()
  elseif status == "complete" and reset_after_reply then
    -- Some Mesen systems power cycle by recreating the debugger and Lua context. Send the terminal
    -- response first; the host then waits for the replacement script session before closing reset.
    reset_after_reply = false
    emu.reset()
    if STATE == "frozen" then resume_from_freeze() end
    return "resetting"
  end
  return status
end

local function poll_line()
  -- 호스트는 한 요청씩 직렬 호출한다. 이전 응답이 남은 동안 새 요청을 읽지 않아
  -- 여러 응답 queue와 무제한 메모리 증가를 만들지 않는다.
  if not conn or Tx.pending(tx) then return nil end
  local line, err, partial = conn:receive("*l", rx_buf)
  if line then
    rx_buf = ""
    return line
  elseif err == "timeout" then
    rx_buf = partial or ""
    return nil
  else
    disconnect()
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
    emu.log("[emucap] invalid freeze key '" .. tostring(FREEZE_KEY) .. "'; disabled (set EMUCAP_FREEZE_KEY to change it)")
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
  local duplicate = {}
  for _, b in ipairs(buttons or {}) do
    local raw = tostring(b)
    local lb = raw:lower()              -- "A" → "a" 정규화
    lb = BUTTON_ALIASES[lb] or lb
    if VALID_BUTTONS[lb] then
      if t[lb] then duplicate[#duplicate + 1] = raw else t[lb] = true end
    else
      unknown[#unknown + 1] = raw
    end
  end
  if #unknown > 0 then
    return nil, "unknown " .. SYS.system_label .. " button(s): " .. table.concat(unknown, ", ")
  end
  if #duplicate > 0 then
    return nil, "duplicate " .. SYS.system_label .. " button(s): " .. table.concat(duplicate, ", ")
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

local function bounded_sync_count(value, default_value, allow_zero)
  local n = tonumber(value)
  if n == nil then n = default_value end
  if n ~= math.floor(n) then
    return nil, "frame/instruction count must be an integer"
  end
  local minimum = allow_zero and 0 or 1
  if n < minimum then n = minimum end
  if n > MAX_SYNC_ADVANCE then
    return nil, string.format(
      "frame/instruction count %s exceeds synchronous limit %d; split the request and verify each terminal response",
      tostring(n), MAX_SYNC_ADVANCE
    )
  end
  return n
end

function handlers.hello()
  local memory_regions = nil
  if emu and emu.memType then
    memory_regions = Memory.regions(emu, SYS)
  end
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
  if RECORDING_SUPPORTED then
    method_list[#method_list + 1] = "record_window"
  end
  local host_features = {
    "code_break_idle",
    "native_halt_service",
    "native_halt_savestate",
    "paused_start_service",
  }
  if HAS_SNES_PPU_OBJ_EVENTS then host_features[#host_features + 1] = "snes_ppu_obj_events" end
  if HAS_SNES_DEEP_EVENTS then host_features[#host_features + 1] = "snes_deep_observation_events" end
  local breakpoint_kinds = {
    { kind = "exec", range_unit = "address", range_mode = "inclusive", memory_type_used = true, snapshot = true },
    { kind = "read", range_unit = "address", range_mode = "inclusive", memory_type_used = true, snapshot = true },
    { kind = "write", range_unit = "address", range_mode = "inclusive", memory_type_used = true, snapshot = true },
    { kind = "nmi", range_unit = "none", range_mode = "none", memory_type_used = false, snapshot = false },
    { kind = "irq", range_unit = "none", range_mode = "none", memory_type_used = false, snapshot = false },
  }
  if SYS.dma_supported then
    breakpoint_kinds[#breakpoint_kinds + 1] =
      { kind = "dma", range_unit = "register_address", range_mode = "exact", memory_type_used = false, snapshot = false }
  end
  if HAS_SNES_PPU_OBJ_EVENTS then
    breakpoint_kinds[#breakpoint_kinds + 1] =
      { kind = "snes_obj_eval_start", range_unit = "ppu_scanline", range_mode = "inclusive", memory_type_used = false, snapshot = true }
    breakpoint_kinds[#breakpoint_kinds + 1] =
      { kind = "snes_obj_handoff", range_unit = "ppu_scanline", range_mode = "inclusive", memory_type_used = false, snapshot = true }
  end
  local result = {
    protocol_version = PROTOCOL_VERSION,
    system = SYS.system,
    adapter = "mesen2-live",
    build = os.getenv("EMUCAP_BUILD_HASH") or "unknown",  -- launch가 넘긴 emucap git hash(status.emulator_build)
    mesen_host_api = MESEN_HOST_API,
    host_features = host_features,
    methods = method_list,
    breakpoint_kinds = breakpoint_kinds,
    execution_limits = { max_sync_advance_count = MAX_SYNC_ADVANCE },
  }
  if RECORDING_SUPPORTED then
    result.recording = Recording.capability(
      as_array, HAS_SNES_PPU_OBJ_EVENTS, memory_regions ~= nil and #memory_regions > 0,
      HAS_SNES_DEEP_EVENTS, SYS.system == "snes")
  end
  local active_exceptions = { "mesen.execution.instruction-step-absent" }
  if HAS_CALLSTACK then
    active_exceptions[#active_exceptions + 1] = "mesen.call-stack.best-effort"
  end
  result.contracts = {
    catalog = "emucap-feature-contracts/v3",
    active_exceptions = active_exceptions,
  }
  if MESEN_UPSTREAM_COMMIT and MESEN_BINARY_SHA256 then
    result.host_build = {
      upstream = "https://github.com/nesdev-org/MesenCE.git",
      commit = MESEN_UPSTREAM_COMMIT,
      upstream_commit = MESEN_UPSTREAM_COMMIT,
      patchset_sha256 = MESEN_PATCHSET_SHA256 or "unknown",
      binary_sha256 = MESEN_BINARY_SHA256,
      host_api = MESEN_HOST_API,
    }
  end
  -- Advertise only memory types that belong to the active system and have a known finite range.
  -- Mesen's global enum also contains names for inactive cores, so enumerating it directly would
  -- make status.memory_types claim regions that this game cannot use.
  if memory_regions then
    result.memory_regions = memory_regions
    result.memory_types = as_array({})
    for _, region in ipairs(memory_regions) do
      result.memory_types[#result.memory_types + 1] = region.memory_type
    end
  end
  local name = os.getenv("EMUCAP_NAME")
  if name then result.name = name end
  local token = os.getenv("EMUCAP_SESSION_TOKEN")
  if token then result.session_token = token end
  local content = os.getenv("EMUCAP_CONTENT")
  if content then result.content = content end
  local launch_id = os.getenv("EMUCAP_LAUNCH_ID")
  if launch_id then result.launch_id = launch_id end
  return true, result
end

function handlers.read_memory(p)
  local length = p.length or 0
  -- 워치독 안전 상한: 멀티MB 읽기는 emu 스레드를 초단위 블록해 소켓을 굶긴다(find_pattern SCAN_CAP과 동형).
  -- 넘치면 조용히 자르지 않고 에러로 거부한다 — truncated+ok로 부분 데이터를 성공처럼 돌려주면 검증
  -- consumer(observe/regression)가 hex만 읽어 prefix만 해시/비교해 거짓 pass/fail을 낼 수 있다.
  -- 큰 영역은 dump_memory를 쓰거나 READ_CAP 이하로 나눠 읽는다.
  if length > READ_CAP then
    return false, "bad_params",
      string.format("read_memory length %d exceeds limit %d; use dump_memory or split the read", length, READ_CAP)
  end
  local region, kind, message = Memory.range(emu, SYS, p.memory_type, p.address, length)
  if not region then return false, kind, message end
  local out = {}
  for i = 0, length - 1 do
    out[#out + 1] = string.format("%02x", emu.read(p.address + i, region.memory_type, false))
  end
  return true, { hex = table.concat(out) }
end

function handlers.write_memory(p)
  local hex = p.hex
  -- 홀수 길이 hex는 마지막 single nibble을 한 바이트로 써넣는 조용한 오류를 낸다 — 거부한다.
  if type(hex) ~= "string" or #hex < 2 or #hex % 2 ~= 0 then
    return false, "bad_params", "hex must be an even-length hexadecimal string"
  end
  local n = 0
  local region, kind, message = Memory.range(emu, SYS, p.memory_type, p.address, #hex / 2)
  if not region then return false, kind, message end
  for i = 1, #hex, 2 do
    local byte = tonumber(hex:sub(i, i + 1), 16)
    if not byte then return false, "bad_params", "hex decode failed" end
    emu.write(p.address + n, byte, region.memory_type)
    n = n + 1
  end
  return true, { written = n }
end

function handlers.set_input(p)
  local tbl, err = buttons_to_table(p.buttons)
  if not tbl then return false, "bad_params", err end
  local previous_hold = input_hold
  input_hold = { port = p.port or 0, tbl = tbl }
  local ok, apply_err = apply_input()
  if not ok then
    input_hold = previous_hold
    return false, "emulator_error", "emu.setInput failed: " .. tostring(apply_err)
  end
  -- An empty list applies one neutral state, then returns input ownership to the
  -- emulator. Keeping the empty table would suppress native input on every poll.
  if next(tbl) == nil then input_hold = nil end
  return true, { applied = true }
end

function handlers.screenshot()
  return true, { png_base64 = base64(emu.takeScreenshot()) }
end

-- frozen이면 freeze 시점 스냅샷에서, running이면 live로 상태를 준다. 스냅샷은 freeze 진입 첫 codeBreak에서
-- 잡히므로(위 freeze_snapshot 주석) 정지 진입 지점 상태다. 아래 `if not freeze_snapshot`은 방어적
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
-- 스냅샷을 서빙한다. native halt 중에는 요청 사이 instruction/frame drift가 없다.
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

local bank_tag_active = nil    -- status.bank_tagging 캐시(카트 상수 — 필드 존재 여부, 1회 getState로 판정)
function handlers.status()
  local r = {
    connected = true,
    frame = frame,
    state = STATE,
    execution_limits = { max_sync_advance_count = MAX_SYNC_ADVANCE },
  }
  if STATE == "frozen" then r.reason = freeze_state.reason end
  local held_buttons = {}
  if input_hold then
    for name, pressed in pairs(input_hold.tbl) do
      if pressed then held_buttons[#held_buttons + 1] = name end
    end
    table.sort(held_buttons)
  end
  local timed_input = input_hold and deferred and deferred.kind == "press"
  r.input_override = {
    observable = true,
    engaged = input_hold ~= nil,
    mode = timed_input and "timed" or (input_hold and "persistent" or "native"),
    port = input_hold and input_hold.port or nil,
    buttons = held_buttons,
    remaining_frames = timed_input and deferred.remaining or nil,
  }
  r.freeze_policy = {
    mode = "native_halt_service",
    service_event = "codeBreakIdle",
    savestate_event = "codeBreakIdleSavestate",
    savestate_safe = halt_savestate_safe,
    service_interval_ms = HALT_SERVICE_INTERVAL_MS,
    instruction_drift = 0,
    host_pause_adoption = true,
    idle_auto_resume_ms = MAX_FREEZE_MS,
    disconnect_auto_resume_ms = RECONNECT_GIVEUP_MS,
  }
  if MESEN_UPSTREAM_COMMIT and MESEN_BINARY_SHA256 then
    r.host_build = {
      upstream = "https://github.com/nesdev-org/MesenCE.git",
      commit = MESEN_UPSTREAM_COMMIT,
      upstream_commit = MESEN_UPSTREAM_COMMIT,
      patchset_sha256 = MESEN_PATCHSET_SHA256 or "unknown",
      binary_sha256 = MESEN_BINARY_SHA256,
      host_api = MESEN_HOST_API,
    }
  end
  if RECORDING_SUPPORTED then
    r.recording = {
      capability_revision = Recording.revision(),
      active = recording ~= nil or recording_reset ~= nil,
      phase = recording_reset and "arming_reset"
        or (recording and (recording.advanced_frames < recording.warmup_frames and "warming"
          or (recording.start_on and not recording.observation_started and "aligning"
            or "recording")) or nil),
      capture_id = recording and recording.capture_id
        or (recording_reset and recording_reset.params.capture_id or nil),
      frame = recording and recording.last_frame or nil,
      frames = recording and recording.advanced_frames or nil,
      events = recording and recording.events or nil,
      bytes = recording and recording.complete_bytes or nil,
      last = recording_last,
    }
  end
  -- 핫키 진단(Home "가끔 안 됨" 분간): freeze_key=키명, armed=무장여부, down=지금 눌림 감지중.
  -- Home을 눌렀는데 down=false면 창 포커스/키명 문제(로직 아님), down=true인데 freeze 안 되면 로직 버그.
  r.freeze_key = FREEZE_KEY or "off"
  r.freeze_key_armed = (FREEZE_KEY ~= nil) and freeze_key_ok
  r.freeze_key_down = freeze_key_down()
  -- 능력 광고: 이 카트가 call_stack/get_trace/BP 이벤트 pc에 ROM 뱅크(`bank`)를 붙이는가. status로 발견
  -- (hello 아님 — Rust Capabilities는 hello에서 methods/memory_types/identity만 추출해 여분 키를 버림).
  -- SYS.bank_of 존재만으론 불충분: 매퍼가 뱅크 필드를 실제로 노출할 때만 true(비표준 매퍼는 안 낼 수 있어
  -- 그땐 뱅크가 nil로 나오므로 false로 광고한다). 값이 null인 항목은 "해당 주소 뱅크 미확정"을 뜻한다.
  if bank_tag_active == nil then
    bank_tag_active = (SYS.bank_tagging_active and SYS.bank_tagging_active(emu.getState())) or false
  end
  r.bank_tagging = bank_tag_active
  return true, r
end

-- 게임을 리셋한다(리셋 버튼 없으면 전원 재투입과 동일). 로드된 ROM 바이트는 그대로이므로
-- "처음부터 다시"엔 쓰되, 리빌드한 ROM 검증은 Mesen의 "Reload ROM" 단축키를 쓴다(Lua 미노출).
function handlers.reset()
  reset_after_reply = true
  return true, { reset = true, reconnect = true }
end

function handlers.abort_recording(p)
  if not RECORDING_SUPPORTED then
    return false, "unsupported", "record_window is not supported for " .. SYS.system
  end
  if recording and p.capture_id ~= recording.capture_id then
    return false, "bad_state", "capture_id does not match the active recording"
  end
  return true, {
    status = "completed",
    capture_id = p.capture_id,
    aborted = false,
    terminal = recording == nil,
  }
end

-- 타깃 메모리를 hex로 읽는다(probe 완료 시 사용).
local function read_target(pr)
  local out = {}
  for i = 0, (pr.len - 1) do
    out[#out + 1] = string.format("%02x", emu.read(pr.addr + i, pr.mt, false))
  end
  return table.concat(out)
end

-- ── 지연 명령 (run_frames / press_buttons / probe): 프레임마다 진행, 끝나면 응답 ──
local function apply_deferred_effect(effect)
  Deferred.apply(effect, {
    release_input = function() input_hold = nil end,
    working = function(value)
      send_line(string.format(
        '{"id":%d,"ok":true,"result":{"status":"working"}}',
        value.id))
    end,
    terminal = function(value)
      if value.operation_kind == "probe" and value.status == "completed" then
        reply_ok(value.id, { hex = read_target(value.probe), frame = value.frame })
        return
      end
      local result = { status = value.status, frame = value.frame }
      if value.reason then result.reason = value.reason end
      if value.breakpoint_id then result.breakpoint_id = value.breakpoint_id end
      reply_ok(value.id, result)
    end,
  })
end

local function tick_deferred()
  local effect
  deferred, effect = Deferred.tick(deferred, frame, KEEPALIVE_FRAMES)
  if effect and effect.kind == "working" and Tx.pending(tx) then return end
  apply_deferred_effect(effect)
end

-- 백스톱: freeze(브레이크포인트 등)가 진행 중 지연 명령(press/run_frames/probe)을 가로채면
-- frozen 동안 tick_deferred가 안 돌아 응답이 막힌다. freeze 진입 시 여기서 마무리해 클라이언트
-- 타임아웃을 막는다. press면 버튼을 뗀다.
local function flush_deferred(status, reason, bp_id)
  local effect
  deferred, effect = Deferred.interrupt(deferred, status, reason, bp_id, frame)
  apply_deferred_effect(effect)
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
      StateIo.load(emu, op.path)
    end)
    if not ok then reply_err(op.id, "io_error", err); return end
    if op.probe.frame <= 0 then
      reply_ok(op.id, { hex = read_target(op.probe), frame = frame })
    else
      deferred = Deferred.start(
        session_epoch, op.id, "probe", op.probe.frame, op.probe)
    end
    return
  end
  local ok, err = pcall(function()
    if op.kind == "save" then
      StateIo.save(emu, op.path, op.id)
    else
      StateIo.load(emu, op.path)
    end
  end)
  if ok then reply_ok(op.id, { status = "completed", path = op.path })
  else reply_err(op.id, "io_error", err) end
end

local function arm_io(kind, id, path)
  if not path then reply_err(id, "bad_params", "path is required"); return end
  pending_io = { kind = kind, id = id, path = path }
  pending_io.ref = emu.addMemoryCallback(on_io_exec, emu.callbackType.exec, IO_LO, IO_HI, CPU)
end

-- probe: 베이스 복귀 → frame프레임 전진 → 타깃 읽기, 한 명령에서 원자적으로(결정론적).
-- 의미 키: state(베이스 세이브스테이트), frame(프레임), memory_type/address/length(타깃).
local function arm_probe(id, p)
  if not p.state then reply_err(id, "bad_params", "state is required"); return end
  local frames, err = bounded_sync_count(p.frame, 0, true)
  if not frames then reply_err(id, "bad_params", err); return end
  local region, kind, message = Memory.range(
    emu, SYS, p.memory_type, p.address or 0, p.length or 1)
  if not region then reply_err(id, kind, message); return end
  pending_io = {
    kind = "probe", id = id, path = p.state,
    probe = {
      frame = frames, mem = p.memory_type, mt = region.memory_type,
      addr = p.address or 0, len = p.length or 1,
    },
  }
  pending_io.ref = emu.addMemoryCallback(on_io_exec, emu.callbackType.exec, IO_LO, IO_HI, CPU)
end

-- Frozen save/load is legal only in the host's explicit instruction-boundary idle event. The
-- operation runs before this callback returns, so no guest time or host-composition gap appears
-- between the halt point and serialization/restoration.
local function frozen_state_io(method, id, p)
  if not halt_savestate_safe then
    reply_err(id, "unsafe_halt",
      method .. " requires a main-CPU instruction-boundary halt; frame/PPU step and breakpoint halts are not savestate-safe")
    return nil
  end

  local path = method == "probe" and p.state or p.path
  if not path then
    reply_err(id, "bad_params", method == "probe" and "state is required" or "path is required")
    return nil
  end

  local probe = nil
  if method == "probe" then
    local frames, err = bounded_sync_count(p.frame, 0, true)
    if not frames then reply_err(id, "bad_params", err); return nil end
    local region, range_kind, range_message = Memory.range(
      emu, SYS, p.memory_type, p.address or 0, p.length or 1)
    if not region then reply_err(id, range_kind, range_message); return nil end
    probe = {
      frame = frames,
      mem = p.memory_type,
      mt = region.memory_type,
      addr = p.address or 0,
      len = p.length or 1,
    }
  end

  local ok, bytes_or_err = pcall(function()
    if method == "save_state" then
      return StateIo.save(emu, path, id)
    end
    return StateIo.load(emu, path)
  end)
  if not ok then
    reply_err(id, "io_error", bytes_or_err)
    return nil
  end

  if method == "save_state" then
    reply_ok(id, { status = "completed", path = path, bytes = bytes_or_err })
    return nil
  end

  -- loadSavestate changes the live CPU immediately while the native halt remains owned. Replace
  -- the old freeze projection before replying so every later frozen read observes the restored state.
  freeze_snapshot = emu.getState()
  if method == "load_state" then
    reply_ok(id, { status = "completed", path = path, bytes = bytes_or_err, state = "frozen" })
    return nil
  end

  if probe.frame <= 0 then
    reply_ok(id, { hex = read_target(probe), frame = frame })
    return nil
  end
  deferred = Deferred.start(session_epoch, id, "probe", probe.frame, probe)
  return "resume"
end

-- Recording event bytes use a dedicated authenticated loopback socket, so neither poll_events nor
-- the control response queue becomes archival storage. Core owns the listener, physical byte
-- ceiling, validation, and publication; the adapter never receives an output path.
local function open_recording_sink(p)
  local sink_params = type(p.sink) == "table" and p.sink or {}
  local endpoint = sink_params.endpoint
  local token = sink_params.token
  local port = type(endpoint) == "string" and endpoint:match("^127%.0%.0%.1:(%d+)$") or nil
  port = tonumber(port)
  if not port or port < 1 or port > 65535 then
    return nil, "sink.endpoint must be an authenticated 127.0.0.1 TCP endpoint"
  end
  if type(token) ~= "string" or #token < 16 or #token > 128 or token:find("[^%w%._~%-]") then
    return nil, "sink.token must contain 16..128 URL-safe characters"
  end

  local sock = socket.tcp()
  sock:settimeout(0.1)
  local connected, connect_err = sock:connect(HOST, port)
  if not connected then
    pcall(function() sock:close() end)
    return nil, "sink connect failed: " .. tostring(connect_err)
  end
  local auth = jvalue({ token = token, capture_id = p.capture_id }) .. "\n"
  local last, send_err, partial = sock:send(auth)
  if last ~= #auth then
    pcall(function() sock:close() end)
    return nil, "sink authentication write failed: " .. tostring(send_err or partial or "partial write")
  end
  sock:settimeout(RECORDING_SINK_WRITE_TIMEOUT_SECONDS)

  local closed = false
  return {
    write = function(line) return sock:send(line) end,
    close = function()
      if closed then return true end
      closed = true
      local ok, err = sock:close()
      if ok == nil or ok == false then return nil, err end
      return true
    end,
  }
end

local function open_recording_member_sink(p)
  if type(p.initial_snapshots) ~= "table" or #p.initial_snapshots == 0 then return nil end
  local sink_params = type(p.member_sink) == "table" and p.member_sink or {}
  local endpoint = sink_params.endpoint
  local token = sink_params.token
  local port = type(endpoint) == "string" and endpoint:match("^127%.0%.0%.1:(%d+)$") or nil
  port = tonumber(port)
  if not port or port < 1 or port > 65535 then
    return nil, "member_sink.endpoint must be an authenticated 127.0.0.1 TCP endpoint"
  end
  if type(token) ~= "string" or #token < 16 or #token > 128 or token:find("[^%w%._~%-]") then
    return nil, "member_sink.token must contain 16..128 URL-safe characters"
  end
  local sock = socket.tcp()
  sock:settimeout(0.1)
  local connected, connect_err = sock:connect(HOST, port)
  if not connected then
    pcall(function() sock:close() end)
    return nil, "member sink connect failed: " .. tostring(connect_err)
  end
  local auth = jvalue({ token = token, capture_id = p.capture_id }) .. "\n"
  local last, send_err, partial = sock:send(auth)
  if last ~= #auth then
    pcall(function() sock:close() end)
    return nil, "member sink authentication write failed: "
      .. tostring(send_err or partial or "partial write")
  end
  sock:settimeout(RECORDING_SINK_WRITE_TIMEOUT_SECONDS)
  local closed = false
  return {
    write = function(bytes)
      local offset = 1
      while offset <= #bytes do
        local sent, err, partial_sent = sock:send(bytes, offset)
        if sent then
          offset = sent + 1
        elseif partial_sent and partial_sent >= offset then
          offset = partial_sent + 1
        else
          return nil, err or "partial write", partial_sent
        end
      end
      return #bytes
    end,
    close = function()
      if closed then return true end
      closed = true
      local ok, err = sock:close()
      if ok == nil or ok == false then return nil, err end
      return true
    end,
  }
end

local function capture_initial_snapshots(state)
  local sink = state and state.member_sink
  if not sink then return nil, "member sink is unavailable" end
  for _, request in ipairs(state.initial_snapshots or {}) do
    local region, _, range_error = Memory.range(
      emu, SYS, request.memory_type, request.address, request.length)
    if not region then return nil, range_error end
    local header = jvalue({ label = request.label, bytes = request.length }) .. "\n"
    local sent, write_error = sink.write(header)
    if sent ~= #header then return nil, write_error or "member header write failed" end
    local chunk_size = 4096
    for base = 0, request.length - 1, chunk_size do
      local count = math.min(chunk_size, request.length - base)
      local bytes = {}
      for offset = 0, count - 1 do
        bytes[offset + 1] = string.char(emu.read(
          request.address + base + offset, region.memory_type, false))
      end
      local chunk = table.concat(bytes)
      sent, write_error = sink.write(chunk)
      if sent ~= #chunk then return nil, write_error or "member body write failed" end
    end
  end
  return true
end

local function remember_recording_terminal(finished, result)
  recording_last = {
    capture_id = finished.capture_id,
    operation_outcome = result.operation_outcome,
    execution_outcome = result.execution_outcome,
    integrity = result.integrity,
    final_execution_state = result.final_execution_state,
    final_frame = result.final_frame,
    counters = {
      frames = result.frames,
      events = result.events,
      bytes = result.bytes,
      dropped = result.dropped,
    },
    cleanup = result.cleanup,
    stop_event = result.stop_event,
    observation_start = result.observation_start,
    event_classes = result.event_classes,
    reason = result.reason,
  }
end

local function install_recording_input(state, port)
  if type(state) ~= "table" or port ~= 0 then return nil, "invalid recording input state" end
  input_hold = { port = port, tbl = state }
  return true
end

local function release_recording_input()
  input_hold = nil
  return true
end

local function apply_recording_effect(effect, frame_boundary_proven)
  if not (recording and effect) then return nil end
  if effect.kind == "working" then
    if not Tx.pending(tx) then reply_ok(recording.request_id, Recording.progress(recording)) end
    return nil
  end
  if effect.kind ~= "terminal" then return nil end

  local finished = recording
  local result = Recording.result(finished, wall_ms())
  recording = nil
  remember_recording_terminal(finished, result)
  STATE = "frozen"
  freeze_state = FreezeState.halt("record_window", frame_boundary_proven)
  freeze_start_ms = wall_ms()
  freeze_disc_ms = nil
  freeze_snapshot = nil
  -- This only enqueues the terminal line. Every caller invokes breakExecution before the next TX
  -- flush, and the native halt idle callback performs that flush after CodeBreak linearizes frozen.
  reply_ok(finished.request_id, result)
  return "freeze"
end

local function release_recording_hooks()
  local ok, errors, retained = true, {}, {}
  for _, hook in ipairs(recording_hook_refs) do
    local removed, err = pcall(emu.removeEventCallback, hook.ref, hook.event_type)
    if not removed then
      ok = false
      errors[#errors + 1] = tostring(err)
      retained[#retained + 1] = hook
    end
  end
  recording_hook_refs = retained
  if not ok then return nil, table.concat(errors, "; ") end
  return true
end

local function semantic_obj_payload()
  local state = emu.getState()
  local regs = SYS.snapshot_regs(state)
  local pc, bank = state["cpu.pc"], state["cpu.k"]
  regs.pc = type(pc) == "number" and type(bank) == "number"
    and pc + bank * 0x10000 or nil
  return {
    cpu = regs,
    ppu = {
      scanline = state["ppu.scanline"],
      dot = state["ppu.cycle"],
      hclock = state["ppu.hClock"],
    },
    forced_blank = state["ppu.forcedBlank"],
  }
end

local function deep_payload(event_id, event)
  if event_id == "snes_cpu_instruction" then
    return { pc = event.pc, opcode = event.opcode, a = event.a, x = event.x, y = event.y,
      sp = event.sp, d = event.d, dbr = event.dbr, k = event.k, ps = event.ps,
      emulation = event.emulation, cpu_cycle = event.cpuCycle }
  elseif event_id == "snes_content_read" then
    return { bus_address = event.address, content_offset = event.contentOffset,
      value = event.value, operation = event.operation, pc = event.pc, cpu_cycle = event.cpuCycle }
  elseif event_id == "snes_transfer_enable" then
    return { written_mask = event.value, channel = event.channel, hdma = event.hdma,
      direction_to_bus_a = event.directionToBusA, source = event.source,
      destination = event.destination, size = event.transferSize, mode = event.transferMode,
      fixed = event.fixedTransfer, decrement = event.decrement, pc = event.pc,
      table_address = event.tableAddress, hdma_bank = event.hdmaBank,
      hdma_indirect = event.hdmaIndirect, cpu_cycle = event.cpuCycle }
  elseif event_id == "snes_transfer_access" then
    return { bus_address = event.address, value = event.value, operation = event.operation,
      channel = event.channel, hdma = event.hdma }
  elseif event_id == "snes_device_port_write" then
    return { port = event.address, value = event.value, operation = event.operation,
      transfer_active = event.transferActive, channel = event.channel, hdma = event.hdma,
      pc = event.pc, cpu_cycle = event.cpuCycle }
  elseif event_id == "snes_interrupt_delivery" then
    return { nmi = event.nmi, interrupted_pc = event.address, handler_pc = event.destination,
      pc = event.pc, a = event.a, x = event.x, y = event.y, sp = event.sp, d = event.d,
      dbr = event.dbr, k = event.k, ps = event.ps, emulation = event.emulation,
      cpu_cycle = event.cpuCycle }
  elseif event_id == "snes_ppu_obj_consumption_read" then
    return { memory_kind = event.memoryKind, address = event.address, value = event.value,
      scanline = event.scanline, dot = event.dot, hclock = event.hClock }
  end
  return nil
end

local function install_recording_semantic_hooks(state)
  local candidates = {
    { id = "snes_ppu_obj_evaluation_start", event_type = emu.eventType.snesPpuObjEvaluationStart,
      payload = semantic_obj_payload },
    { id = "snes_ppu_obj_handoff", event_type = emu.eventType.snesPpuObjScanlineHandoff,
      payload = semantic_obj_payload },
    { id = "snes_cpu_instruction", event_type = emu.eventType.snesInstructionExecution, deep = true },
    { id = "snes_content_read", event_type = emu.eventType.snesContentRead, deep = true },
    { id = "snes_transfer_enable", event_type = emu.eventType.snesTransferEnable, deep = true },
    { id = "snes_transfer_access", event_type = emu.eventType.snesTransferAccess, deep = true },
    { id = "snes_device_port_write", event_type = emu.eventType.snesDevicePortWrite, deep = true },
    { id = "snes_interrupt_delivery", event_type = emu.eventType.snesInterruptDelivery, deep = true },
    { id = "snes_ppu_obj_consumption_read", event_type = emu.eventType.snesPpuObjConsumptionRead,
      deep = true },
  }
  local requested = false
  for _, candidate in ipairs(candidates) do
    if state.selected[candidate.id] then
      local event_id = candidate.id
      local event_type = candidate.event_type
      local is_deep = candidate.deep == true
      local payload_factory = candidate.payload
      requested = true
      if (is_deep and not HAS_SNES_DEEP_EVENTS)
          or (not is_deep and not HAS_SNES_PPU_OBJ_EVENTS) or event_type == nil then
        release_recording_hooks()
        return nil, "selected semantic event requires the maintained SNES observation callbacks"
      end
      local ok, ref = pcall(emu.addEventCallback, function()
        if recording ~= state or not state.active then return end
        local effect
        local event = is_deep and emu.getSnesObservationEvent() or nil
        -- Event envelopes use the adapter's recording-frame coordinate. Native observation
        -- callbacks also expose Mesen's process-global frame counter, but that counter can start
        -- before this script and therefore cannot be mixed with frame_boundary/f_start. Device
        -- time remains available through the native master clock and phase payload.
        local event_frame = frame
        local tick = event and event.masterClock or emu.getMasterClock()
        local payload = payload_factory and payload_factory() or deep_payload(event_id, event)
        state, effect = Recording.semantic_event(state, event_id, event_frame, tick, payload)
        recording = state
        if effect and apply_recording_effect(effect, false) == "freeze" then emu.breakExecution() end
      end, event_type)
      if not ok or ref == nil then
        release_recording_hooks()
        return nil, "semantic event hook installation failed: " .. tostring(ref)
      end
      recording_hook_refs[#recording_hook_refs + 1] = { ref = ref, event_type = event_type }
    end
  end
  return Recording.attach_hooks(state, requested)
end

local function resolve_recording_effect(effect, frame_boundary_proven)
  if not (recording and effect and effect.kind == "arm_observation") then
    return apply_recording_effect(effect, frame_boundary_proven)
  end
  local hooks_ok, hooks_error = install_recording_semantic_hooks(recording)
  if not hooks_ok then
    recording, effect = Recording.cancel(recording, frame, hooks_error)
  else
    effect = { kind = "working" }
  end
  return apply_recording_effect(effect, frame_boundary_proven)
end

local function recording_conflict()
  if recording or recording_reset or deferred or pending_step_id or pending_io or input_hold then
    return "a conflicting temporal operation or input override is active"
  end
  if #recording_hook_refs > 0 then
    return "a prior recording hook could not be verified as removed"
  end
  if next(breakpoints) ~= nil or trace_on or reset_bp then
    return "persistent breakpoints, reset watch, and instruction trace must be cleared before recording"
  end
  return nil
end

local function start_recording(id, p)
  if not RECORDING_SUPPORTED then
    reply_err(id, "unsupported", "record_window is not supported for " .. SYS.system)
    return nil
  end
  local conflict = recording_conflict()
  if conflict then reply_err(id, "busy", conflict); return nil end

  local callback_start = wall_ms()
  local launch_id = os.getenv("EMUCAP_LAUNCH_ID")
  local validated, kind, message = Recording.validate(p, launch_id, frame, callback_start)
  if not validated then reply_err(id, kind, message); return nil end
  local prepared
  prepared, kind, message = Recording.prepare(
    p, launch_id, frame, callback_start, buttons_to_table, validated)
  if not prepared then reply_err(id, kind, message); return nil end
  local sink, sink_err = open_recording_sink(p)
  if not sink then reply_err(id, "sink_unavailable", sink_err); return nil end
  local member_sink, member_sink_err = open_recording_member_sink(p)
  if member_sink_err then
    pcall(sink.close)
    reply_err(id, "sink_unavailable", member_sink_err)
    return nil
  end

  if p.origin == "reset_release" then
    recording_reset = {
      params = p,
      request_id = id,
      launch_id = launch_id,
      started_ms = callback_start,
      sink = sink,
      member_sink = member_sink,
      prepared = prepared,
      cancelled = false,
    }
    -- Mesen processes this queued action after the current frame. Its native Reset event runs after
    -- the console reset and before any post-reset guest tick; that callback completes the arm.
    local queued, reset_error = pcall(emu.reset)
    if not queued then
      recording_reset = nil
      pcall(sink.close)
      if member_sink then pcall(member_sink.close) end
      reply_err(id, "emulator_error", "reset queue failed: " .. tostring(reset_error))
      return nil
    end
    return "started"
  end

  local state, effect
  state, effect, kind, message = Recording.start(
    p, id, launch_id, frame, callback_start, sink, buttons_to_table,
    install_recording_input, release_recording_input, prepared, release_recording_hooks,
    member_sink, capture_initial_snapshots, wall_ms)
  if not state then
    pcall(sink.close)
    if member_sink then pcall(member_sink.close) end
    reply_err(id, kind, message)
    return nil
  end
  recording = state
  local action = resolve_recording_effect(effect, true)
  return action or "started"
end

local function handle_recording_control(line)
  local id, method, p = parse_request(line)
  id = id or 0
  if method ~= "abort_recording" then
    reply_err(id, "busy", "record_window owns guest progress until its terminal response")
    return nil
  end
  if p.capture_id ~= recording.capture_id or p.launch_id ~= recording.launch_id then
    reply_err(id, "bad_state", "abort identity does not match the active recording")
    return nil
  end

  local finished_id = recording.request_id
  local effect
  recording, effect = Recording.cancel(recording, frame, "request_cancelled")
  reply_ok(id, { status = "completed", capture_id = p.capture_id, aborted = true })
  -- apply_recording_effect replies to the original request id after the abort acknowledgement.
  if recording then recording.request_id = finished_id end
  return apply_recording_effect(effect, true)
end

-- A TCP session is also the response-id namespace. Once it is gone, an unfinished response cannot
-- be delivered on the next session: doing so puts an old id ahead of the new hello and can keep the
-- replacement MCP from ever completing its handshake. Preserve emulator/freeze state, but cancel
-- only request-scoped work and release a transient press hold.
abort_inflight = function()
  local cancelled = deferred ~= nil or pending_step_id ~= nil or pending_io ~= nil
    or recording ~= nil or recording_reset ~= nil
  local deferred_effect
  deferred, deferred_effect = Deferred.cancel(deferred)
  apply_deferred_effect(deferred_effect)
  pending_step_id = nil
  step_remaining = 0
  if pending_io and pending_io.ref then
    pcall(emu.removeMemoryCallback, pending_io.ref, emu.callbackType.exec, IO_LO, IO_HI, CPU)
  end
  pending_io = nil
  if recording then
    local finished = recording
    local effect
    finished, effect = Recording.cancel(finished, frame, "connection_closed")
    local result = Recording.result(finished, wall_ms())
    recording = nil
    remember_recording_terminal(finished, result)
    STATE = "frozen"
    freeze_state = FreezeState.halt("record_window", false)
    freeze_start_ms = wall_ms()
    freeze_disc_ms = freeze_start_ms
    freeze_snapshot = nil
    emu.breakExecution()
  end
  if recording_reset then
    -- The system reset is already queued and has no safe public unqueue operation. Preserve the
    -- exact request until the native reset callback, then close it before the first post-reset tick.
    recording_reset.cancelled = true
  end
  reset_after_reply = false
  if cancelled then pcall(emu.log, "[emucap] connection closed; cancelled unfinished requests and waiting to reconnect") end
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

local function parse_snapshot_specs(raw)
  if raw == nil then return nil end
  if type(raw) ~= "table" then return nil, "snapshot must be an array of memory_type:address:length strings" end
  local specs = {}
  for _, raw_spec in ipairs(raw) do
    local mt_name, addr_text, len_text = tostring(raw_spec):match("^%s*([^:]+):([^:]+):([^:]+)%s*$")
    if not mt_name then return nil, "invalid snapshot specification: " .. tostring(raw_spec) end
    local addr, len = snum(addr_text), snum(len_text)
    if addr == nil then return nil, "invalid snapshot address: " .. addr_text end
    if len == nil or len < 1 then return nil, "invalid snapshot length: " .. len_text end
    if len > READ_CAP then
      return nil, string.format("snapshot length %d exceeds limit %d", len, READ_CAP)
    end
    local region, _, range_message = Memory.range(emu, SYS, mt_name, addr, len)
    if not region then return nil, range_message end
    specs[#specs + 1] = {
      mt = region.memory_type, mt_name = mt_name, addr = addr, len = len,
    }
  end
  return specs
end

local function capture_snapshot_specs(specs)
  if specs == nil then return nil end
  local snapshots = {}
  for _, spec in ipairs(specs) do
    local bytes = {}
    for offset = 0, spec.len - 1 do
      bytes[#bytes + 1] = string.format("%02x", emu.read(spec.addr + offset, spec.mt, false))
    end
    snapshots[#snapshots + 1] = {
      memory_type = spec.mt_name,
      address = spec.addr,
      hex = table.concat(bytes),
    }
  end
  return as_array(snapshots)
end

-- ROM 뱅크 태깅: pc가 페이징된 ROM 뱅크(GG/GB). SYS.bank_of 있는 시스템만, 없으면 nil(SNES는 뱅크가 pc
-- 안, NES 등 미해당). pc는 16비트 실행주소(full_pc가 GG/GB에선 cpu.pc로 축약이라 st["cpu.pc"] 사용).
-- 모든 breakpoint_hit 생성 사이트(record_hit + 인라인 nmi/irq/dma)가 공유해 동일 이벤트타입 내 균일 보장.
local function bank_for_pc(st)
  if not SYS.read_banks then return nil end
  return SYS.bank_of(st["cpu.pc"], SYS.read_banks(st))
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
    ev.bank = bank_for_pc(st)              -- pc의 ROM 뱅크(GG/GB), 아니면 nil. addr 아닌 pc 기준
    if bp.snapshot_specs then ev.snapshot = capture_snapshot_specs(bp.snapshot_specs) end
    events[#events + 1] = ev
  end
  if bp.pause_on_hit and STATE ~= "frozen" then
    flush_deferred("interrupted", "breakpoint", bp.id)   -- 진행 중 지연 명령 마무리
    STATE = "frozen"; freeze_state = FreezeState.halt("breakpoint", false)
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
    STATE = "frozen"; freeze_state = FreezeState.halt("register_break", false)
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

-- 값-조건 BP가 비교할 접근 값(little-endian). value_len=1이면 콜백이 준 접근 바이트(read=읽힌 값,
-- write=쓰일 값)를 그대로 쓴다. value_len>1일 때:
--   • write BP: write 콜백은 pre-write·per-byte라 emu.read(addr+i)는 아직 안 쓰인 옛 바이트를 준다 →
--     상위바이트를 메모리에서 읽으면 stale 값으로 재구성돼 미스/오발화한다. 대신 실제로 쓰이는 바이트를
--     각 per-byte write 콜백의 value로 누적해 재구성한다(폭 전체를 콜백에 등록해야 상위 바이트 콜백이
--     닿는다 — set_breakpoint의 mirror 폭 확장 참조). 폭 전체가 관측되기 전엔 nil(미결정)을 돌려 on_access가
--     hit를 미룬다. burst 정체성: 각 write는 진행 중 burst의 기대 다음 오프셋(wnext)이어야 이어 붙고,
--     아니면(비연속·역순·저바이트 재시작) burst를 버린다 — 시간적으로 무관한 산발 write가 슬롯을 채워
--     존재한 적 없는 값을 재구성하는 오발화를 막는다. 그래서 폭 전체가 '한 store로 연속' 쓰였을 때만 매치한다.
--     가정: 타깃 CPU(65816·Z80·ARM)는 워드 store를 저바이트부터(little-endian, low→high) 쓴다. 저바이트를
--     나중에 쓰는 store(예 GB PUSH의 high→low)는 재구성되지 않아 값-BP가 발화하지 않는다(오발화보다 미발화 선택).
--   • read BP: 상위바이트는 이미 메모리에 있으므로(비파괴 read) addr+i를 little-endian으로 읽는다.
local function access_value(bp, addr, value)
  local len = bp.value_len or 1
  if len <= 1 then return value end
  if bp.kind == "write" and not bp.is_vram_recon then
    local off = addr - bp.start
    -- 뱅크 미러($80) 콜백은 절대주소(예 0x802118)로 발화하므로 bp.start 기준 span으로
    -- 되돌린다 — 안 그러면 off가 0x800000+가 되어 폭>1 write 누적이 영영 완결되지 않는다. 미러는 +0x800000.
    if off >= 0x800000 then off = off - 0x800000 end
    if off < 0 or off >= len then return nil end
    local buf = bp.wbytes
    if off == 0 then
      buf = { [0] = value }; bp.wbytes = buf; bp.wnext = 1                -- 저바이트 → 새 burst 시작
    elseif buf ~= nil and off == bp.wnext then
      buf[off] = value; bp.wnext = off + 1                               -- 기대 다음 오프셋 → 이어 붙임
    else
      bp.wbytes = nil; bp.wnext = nil; return nil                        -- 비연속·역순 → burst 폐기, 판정 보류
    end
    if bp.wnext < len then return nil end                                -- 폭 미완결 → 판정 보류
    local v = 0
    for i = 0, len - 1 do v = v + buf[i] * (256 ^ i) end
    bp.wbytes = nil; bp.wnext = nil                                      -- 소비 완료 → 다음 burst용 리셋
    return v
  end
  -- read(및 vram_recon value_len>1 폴백): 상위바이트 읽기용 memory_type. 버스주소로 변환한 BP는 addr이
  -- 버스주소이므로 RAM-상대 memory_type(예 smsWorkRam)으로 addr+i를 읽으면 범위 밖을 읽는다 — 버스 memtype으로.
  local mt = (bp.bus_translated and emu.memType[SYS.default_memtype])
    or emu.memType[bp.memory_type] or emu.memType[SYS.default_memtype]
  local v = value
  for i = 1, len - 1 do
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
  -- auto_savestate: BP 히트는 read/write/이벤트(nmi·irq·dma) 콜백 안에서
  -- 일어나는데 emu.createSavestate는 exec 콜백 컨텍스트 전용이고 이벤트·codeBreak 컨텍스트에선
  -- 실패하므로 히트 순간 세이브스테이트를 원자적으로 못 뜬다. 조용히 받아 no-op하면
  -- 호출자가 상태가 캡처된 줄 오인하므로 명시적으로 거부한다. 히트 순간 원자 캡처는 `snapshot` 스펙
  -- (record_hit이 read로 이벤트에 담음). 전체 세이브스테이트는 실행 중 `save_state`로 만들 수 있지만
  -- breakpoint 히트 시점과 원자적이지 않다.
  if p.auto_savestate then
    return false, "unsupported",
      "auto_savestate is unavailable on breakpoint hits because createSavestate is valid only in an exec callback. "
      .. "Use snapshot specs for atomic hit-time capture; save_state while running is not atomic with the hit."
  end
  local id = next_bp_id; next_bp_id = next_bp_id + 1
  if p.kind == "snes_obj_eval_start" or p.kind == "snes_obj_handoff" then
    if not HAS_SNES_PPU_OBJ_EVENTS then
      return false, "unsupported", "SNES OBJ boundary events require host feature snes_ppu_obj_events"
    end
    local start_scanline, end_scanline = snum(p.start), snum(p["end"])
    if start_scanline == nil or end_scanline == nil
        or start_scanline < 0 or end_scanline < start_scanline
        or start_scanline ~= math.floor(start_scanline) or end_scanline ~= math.floor(end_scanline)
        or end_scanline > 0xFFFF then
      return false, "bad_params", "SNES OBJ event start/end must be ascending integer scanlines in 0..65535"
    end
    local snapshot_specs, snapshot_error = parse_snapshot_specs(p.snapshot)
    if snapshot_error then return false, "bad_params", snapshot_error end
    local is_start = p.kind == "snes_obj_eval_start"
    local event_type = is_start and emu.eventType.snesPpuObjEvaluationStart
      or emu.eventType.snesPpuObjScanlineHandoff
    local bp = {
      id = id,
      kind = p.kind,
      is_event = true,
      evtype = event_type,
      start = start_scanline,
      end_ = end_scanline,
      pause_on_hit = p.pause_on_hit,
      snapshot_specs = snapshot_specs,
    }
    bp.ref = emu.addEventCallback(function()
      local state = emu.getState()
      local scanline = state["ppu.scanline"]
      if scanline == nil or scanline < bp.start or scanline > bp.end_ then return end
      if #events >= EVENT_CAP then
        if not bp.pause_on_hit then dropped = dropped + 1; return end
        table.remove(events, 1)
        dropped = dropped + 1
      end
      local event = {
        type = "device_event",
        breakpoint_id = id,
        kind = bp.kind,
        device = "snes_ppu_obj",
        ppu = {
          frame = state["ppu.frameCount"],
          scanline = scanline,
          dot = state["ppu.cycle"],
          hclock = state["ppu.hClock"],
          master_clock = emu.getMasterClock(),
        },
        forced_blank = state["ppu.forcedBlank"] or false,
      }
      if bp.snapshot_specs then event.snapshot = capture_snapshot_specs(bp.snapshot_specs) end
      events[#events + 1] = event
      if bp.pause_on_hit and STATE ~= "frozen" then
        flush_deferred("interrupted", bp.kind, id)
        STATE = "frozen"; freeze_state = FreezeState.halt(bp.kind, false); emu.breakExecution()
      end
    end, event_type)
    breakpoints[id] = bp
    return true, { id = id, mechanism = "snes_ppu_obj_event" }
  end
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
          address = 0, value = 0, pc = full_pc(st), bank = bank_for_pc(st), frame = frame }
      else dropped = dropped + 1 end
      if bp.pause_on_hit and STATE ~= "frozen" then
        flush_deferred("interrupted", p.kind, id)
        STATE = "frozen"; freeze_state = FreezeState.halt(p.kind, false); emu.breakExecution()
      end
    end, evtype)
    breakpoints[id] = bp
    return true, { id = id }
  end
  -- DMA: MDMAEN($420B, 또는 start로 지정) write 시 채널 스냅샷을 dma 이벤트로. 매 프레임 발생이라
  -- 기본은 freeze 안 함(pause_on_hit=true면 freeze). poll_events로 "이번 프레임 DMA"를 드레인.
  if p.kind == "dma" then
    -- 사전 조건: dma BP는 SNES MDMAEN($420B) 컨트롤러 전용이다. 그런 DMA가 없는 시스템(GG/Z80 등)은
    -- unsupported를 낸다(break_on_reset의 `if not SYS.reset_vector` 패턴 — garbage 대신 명시적 에러).
    if not SYS.dma_supported then return false, "unsupported", "dma breakpoints not supported for " .. SYS.system end
    -- $420B(MDMAEN)는 banks $00-$3F·$80-$BF에 미러된다. 게임이 어느 뱅크에서 STA $420B 하든 잡으려면
    -- 미러를 등록해야 한다(Mesen 메모리 콜백은 뱅크별 절대주소 — bank $00 등록은 bank $80 접근을 못 잡음).
    -- 기본은 슬로우($00:420B)·패스트($80:420B) 뱅크 둘. p.start로 특정 뱅크 미러만 지정 가능.
    -- start=$420B(기본 주소) 또는 미지정/0이면 슬로우($00)·패스트($80) 뱅크 미러 둘 다 자동 등록.
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
          channels = as_array(chans), pc = full_pc(st), bank = bank_for_pc(st), frame = frame }
      else dropped = dropped + 1 end
      if bp.pause_on_hit and STATE ~= "frozen" then
        flush_deferred("interrupted", "dma", id)
        STATE = "frozen"; freeze_state = FreezeState.halt("dma", false); emu.breakExecution()
      end
    end
    for _, reg in ipairs(regs) do
      bp.dma_refs[#bp.dma_refs + 1] = { ref = emu.addMemoryCallback(on_dma, bp.cbtype, reg, reg, CPU), reg = reg }
    end
    breakpoints[id] = bp
    return true, { id = id }
  end
  local cbtype = emu.callbackType[p.kind]
  if not cbtype then
    return false, "bad_params", "unsupported breakpoint kind: " .. tostring(p.kind)
  end
  local bp_start, bp_end = p.start or 0, p["end"] or p.start or 0
  if bp_end < bp_start then
    return false, "bad_params", "breakpoint end must be greater than or equal to start"
  end
  local has_value = (p.value ~= nil) and (p.kind ~= "exec")
  local value_len = math.max(1, math.min(4, p.value_len or 1))
  local value_tail = has_value and value_len - 1 or 0
  local bp_region, range_kind, range_message = Memory.range(
    emu, SYS, p.memory_type, bp_start, bp_end - bp_start + 1 + value_tail)
  if not bp_region then return false, range_kind, range_message end
  -- 값-조건: read/write BP에서 접근 값이 (value & value_mask)와 같을 때만 발화.
  -- exec BP엔 접근 값 개념이 없어 무시. value 미지정이면 종전대로 모든 접근에 발화.
  local bp = {
    id = id, kind = p.kind, memory_type = p.memory_type,
    pause_on_hit = p.pause_on_hit,
    start = bp_start, end_ = bp_end,
    cbtype = cbtype, pc_min = p.pc_min, pc_max = p.pc_max,   -- pc 조건(선택)
    has_value = has_value, value = p.value or 0,
    value_mask = p.value_mask or 0xFFFFFFFF, value_len = value_len,
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
    if base then
      -- 뱅크된 memtype(GB CGB VRAM/WRAM/카트RAM)은 뱅크당 고정 버스 window(예 0x2000)만 CPU 버스에 보인다 —
      -- 그 밖 offset은 안정된 버스주소가 없다(gbVideoRam:0x2000 → base+off=0xA000=카트RAM에 오발화). base+offset을
      -- 조용히 걸면 무관 메모리에 걸리므로, bp_bus_window 지정 시 window 밖 offset은 명확히 거부한다(read_memory는 여전히 offset으로 됨).
      local window = SYS.bp_bus_window and SYS.bp_bus_window[bp.memory_type]
      if window and bp.end_ + value_tail >= window then
        return false, "bad_params", string.format(
          "%s BP offset 0x%X is outside its CPU-bus window (0x%X); banked regions have no stable BP address outside that window. "
            .. "Use an offset within the window or a CPU-bus memory type.",
          bp.memory_type, bp.end_, window)
      end
      bp.start = bp.start + base; bp.end_ = bp.end_ + base; bp.bus_translated = true
    end
  end
  -- snapshot: 히트 순간 atomic 캡처할 메모리 스펙 리스트("mt:addr:len", addr는 0x/$/10진). record_hit이
  -- 레지스터(항상)와 함께 이벤트에 싣는다 → 워치독 드리프트/데드맨 무관하게 히트 순간 보존.
  local snapshot_specs, snapshot_error = parse_snapshot_specs(p.snapshot)
  if snapshot_error then return false, "bad_params", snapshot_error end
  bp.snapshot_specs = snapshot_specs
  -- non-CPU-버스 memtype 라우팅: VDP VRAM/CRAM 등은 CPU 버스에 없어 Mesen memory
  -- 콜백이 절대 안 잡는다(실측). SYS.non_bus_write_memtypes로 (a) 재구성 경로 또는 (b) 에러로 보낸다 —
  -- 조용히 ROM 주소에 걸려 영영 미발동하는 것을 막는다. 선언 안 한 시스템(SNES 등)은 종전 CPU-버스 경로 그대로.
  if (p.kind == "write" or p.kind == "read") and SYS.non_bus_write_memtypes then
    local disp = SYS.non_bus_write_memtypes[bp.memory_type]
    if disp == "vram_recon" then
      if p.kind ~= "write" then
        return false, "unsupported", bp.memory_type .. " read BP reconstruction is unavailable; only write reconstruction is implemented"
      end
      local budget = math.max(1, p.max_instructions or VRAM_RECON_BUDGET)
      setup_vram_recon_bp(bp, budget)
      breakpoints[id] = bp
      return true, { id = id, mechanism = "vdp_write_reconstruction", max_instructions = budget }
    elseif disp then
      return false, "unsupported", bp.memory_type
        .. " is not on the CPU bus and cannot be observed by a memory " .. p.kind .. " breakpoint; reconstruction is unavailable"
    end
  end
  local function on_access(addr, value)
    if bp.has_value then
      local v = access_value(bp, addr, value)
      if v == nil then return end                                                -- 폭>1 write burst 미완결 → 판정 보류
      if band(v, bp.value_mask) ~= band(bp.value, bp.value_mask) then return end  -- 값 불일치 → 무시
      if bp.value_len > 1 then
        -- 폭>1: 히트 주소·값을 개별 바이트가 아니라 베이스 주소와 재구성한 전체 값으로 보고한다.
        -- write=완결 콜백은 상위 바이트라 베이스는 등록 저주소(bp.start), read=접근 저바이트 주소(addr).
        record_hit(bp, (bp.kind == "write") and bp.start or addr, v); return
      end
    end
    record_hit(bp, addr, value)
  end
  -- 뱅크 미러 자동등록: snesMemory의 system 레지스터/IO 영역($2000-$7FFF: PPU/CPU 레지스터)을 뱅크 없이
  -- ($2117 등) 준 단일 주소면, 게임이 어느 뱅크($00 슬로우/$80 패스트)에서 접근하든 잡게 $00·$80 미러를
  -- 둘 다 등록한다(Mesen 콜백은 뱅크별 절대주소 — bank $00만 걸면 bank $80 실행 게임의 $2117 등을 놓침
  -- ). 범위 BP·뱅크 명시 주소($XX0000+)·LowRAM($0000-$1FFF, snesWorkRam 권장)은 그대로.
  --
  -- 폭 확장과 뱅크 미러는 직교한다: 폭>1 write 값-BP는 len 연속 바이트가 각각 per-byte write 콜백으로
  -- 오므로 상위 바이트 콜백까지 닿게 폭 전체(span)를 등록해야 한다(안 그러면 저바이트만 트리거돼
  -- access_value 누적이 영영 완결 안 됨 → 값-BP 미발화). $2000-$7FFF의 폭>1 write 값-BP는 상위 바이트
  -- 주소'와' 그 뱅크 미러가 둘 다 필요하므로 두 조건을 함께 적용한다 — span을 먼저 구해 각 미러가 폭 전체를 덮게.
  local span = bp.end_
  if p.kind == "write" and bp.has_value and bp.value_len > 1 and bp.start == bp.end_ then
    span = bp.start + bp.value_len - 1
  end
  local mirrors = { { bp.start, span } }
  if SYS.bank_mirror and (p.kind == "read" or p.kind == "write") and p.memory_type == SYS.default_memtype
     and bp.start == bp.end_ and bp.start >= 0x2000 and bp.start < 0x8000 then
    mirrors = { { bp.start, span }, { bp.start + 0x800000, span + 0x800000 } }
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
  if not bp then return false, "not_found", "breakpoint not found" end
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
      pause_on_hit = bp.pause_on_hit,
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
        STATE = "frozen"; freeze_state = FreezeState.halt("crash", false); emu.breakExecution()
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
  -- 뱅크 섀도 refresh: dirty일 때만(매퍼 write 후 다음 명령 = write 반영 후). 링 엔트리 태그보다 먼저여야
  -- 스위치 직후 첫 명령이 옛 뱅크로 안 틀린다. cur_banks nil(비-뱅크 시스템)이면 bank=nil, getState 없음.
  if banks_dirty then banks_dirty = false; cur_banks = SYS.read_banks(emu.getState()) end
  trace_ring[(trace_idx % TRACE_CAP) + 1] =
    { pc = addr, op = op, bank = cur_banks and SYS.bank_of(addr, cur_banks) or nil }
  trace_idx = trace_idx + 1
  -- 콜스택 shadow-track은 op_is_call/op_is_return이 있는 ISA만 갱신한다(GBA엔 없어 트레이스 링만 채운다).
  if not HAS_CALLSTACK then return end
  -- 지연 prompt-pop: 직전 명령이 return류였다 → 이제 그 return이 실행돼 SP가 (taken이면) 올라와 있다.
  -- 복원된 SP로 reconcile: 미성립 조건부 RET는 SP 불변이라 pop 안 되고, taken이면 호출자가 아직 push하기 전
  -- 리턴 지점에서 pop돼 masking을 막고, 인터럽트 리턴은 유저 프레임보다 낮은 SP라 유저 프레임을 안 건드린다.
  if pending_ret_check then
    pending_ret_check = false
    reconcile_callstack(emu.getState()["cpu.sp"])
  end
  if SYS.op_is_call(op) then                            -- 호출 명령: SP로 이전 프레임을 정리한 뒤 호출지 push
    local st = emu.getState()                            -- SP + 뱅크를 한 번에(추가 getState 없음)
    local sp = st["cpu.sp"]
    reconcile_callstack(sp)                             -- 이미 리턴한 프레임(JMP-리턴 포함) 정리
    local bank = SYS.read_banks and SYS.bank_of(addr, SYS.read_banks(st)) or nil  -- addr = 호출지 pc
    callstack[#callstack + 1] = { pc = addr, sp = sp, bank = bank }
  elseif SYS.op_is_return(op) or op == 0xED then        -- return류(RET/조건부 RET) 또는 ED 프리픽스(RETI/RETN)
    pending_ret_check = true                             -- opcode로 pop하지 않고 다음 명령에서 SP로 실제 리턴 여부 확인
  end
end

-- 실행추적 on/off. 켜면 매 명령 콜백 등록(느림 — hunting 전용). 켤 때 링·콜스택 초기화.
function handlers.set_trace(p)
  local on = p.enabled and true or false
  if on and not trace_on then
    trace_ring = {}; trace_idx = 0; callstack = {}; pending_ret_check = false
    trace_ref = emu.addMemoryCallback(trace_cb, emu.callbackType.exec, 0, EXEC_MAX, CPU)
    -- 뱅크 섀도(GG/GB): 초기 뱅크를 읽고, 매퍼 write 범위에 write 콜백을 걸어 banks_dirty를 세운다.
    if SYS.bank_write_ranges and SYS.read_banks then
      cur_banks = SYS.read_banks(emu.getState()); banks_dirty = false
      for _, r in ipairs(SYS.bank_write_ranges) do
        local lo, hi = r[1], r[2]
        bank_cb_refs[#bank_cb_refs + 1] = {
          ref = emu.addMemoryCallback(function() banks_dirty = true end, emu.callbackType.write, lo, hi, CPU),
          lo = lo, hi = hi,
        }
      end
    end
    trace_on = true
  elseif not on and trace_on then
    emu.removeMemoryCallback(trace_ref, emu.callbackType.exec, 0, EXEC_MAX, CPU)
    trace_ref = nil; trace_on = false
    for _, c in ipairs(bank_cb_refs) do
      emu.removeMemoryCallback(c.ref, emu.callbackType.write, c.lo, c.hi, CPU)
    end
    bank_cb_refs = {}; cur_banks = nil; banks_dirty = false
  end
  return true, { tracing = trace_on }
end

-- 최근 count개 명령을 시간순(오래된→최신). 엔트리 { pc, op, bank? }. pc는 실행주소(SNES 24비트, GG/GB 등
-- 16비트), op는 opcode 바이트, bank은 pc가 페이징된 ROM 뱅크(GG/GB만; 없으면 생략).
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

-- 콜스택: 호출지 프레임 { pc, bank } 리스트, 바깥→안(안쪽이 마지막). "어떻게 여기 왔나" 즉답. bank은 pc가
-- 페이징된 ROM 뱅크(GG/GB), 아니면 nil(SNES는 뱅크가 pc 안). 모든 Mesen 시스템이 균일하게 { pc, bank } 객체.
-- 조회 시 SP와 한 번 더 맞춘다(마지막 호출 이후 리턴한 프레임 정리). frozen이면 freeze 시점 스냅샷 SP를 쓴다
-- (live getState를 다시 호출하지 않아도 freeze 진입 시점과 같은 SP를 쓰도록 get_state와 frozen_state를 공유).
function handlers.call_stack()
  if not HAS_CALLSTACK then
    return false, "unsupported", "call_stack not supported for " .. SYS.system .. " (no SP-based call-stack model for this ISA)"
  end
  reconcile_callstack(frozen_state()["cpu.sp"])
  local out = {}
  for _, f in ipairs(callstack) do out[#out + 1] = { pc = f.pc, bank = f.bank } end
  return true, { call_stack = as_array(out), depth = #callstack, tracing = trace_on }
end

-- ── 메모리 덤프 (emucap diff 입력) ───────────────────────────
-- 표준 리전을 .bin과 regions.json으로 디렉토리에 쓴다. 콘솔 변경 시 목록을 바꾼다.
local DUMP_REGIONS = SYS.dump_regions

function handlers.dump_memory(p)
  return Dump.write(p, DUMP_REGIONS, emu, io.open)
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
  local hex = p.hex
  if type(hex) ~= "string" or #hex < 2 or #hex % 2 ~= 0 then
    return false, "bad_params", "hex must contain at least one byte and have even length"
  end
  local pat = {}
  for i = 1, #hex, 2 do
    local b = tonumber(hex:sub(i, i + 1), 16)
    if not b then return false, "bad_params", "hex decode failed" end
    pat[#pat + 1] = string.char(b)
  end
  pat = table.concat(pat)
  local start = p.start or 0
  local len = p.length
  if not len then
    local rs = REGION_SIZE[p.memory_type]
    if rs then len = rs - start
    else return false, "bad_params", "length is required for an unknown memory_type" end
  end
  local region, kind, message = Memory.range(emu, SYS, p.memory_type, start, len)
  if not region then return false, kind, message end
  local truncated_scan = false
  if len > SCAN_CAP then len = SCAN_CAP; truncated_scan = true end   -- 워치독 안전 상한
  -- 영역을 바이너리 문자열로 1회 적재
  local buf = {}
  for i = 0, len - 1 do
    buf[i + 1] = string.char(emu.read(start + i, region.memory_type, false))
  end
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
  if method == "record_window" then
    return start_recording(id, p)
  end
  if method == "pause" then
    STATE = "frozen"; freeze_state = FreezeState.halt("paused", false)
    reply_ok(id, { state = "frozen" })
    emu.breakExecution()   -- 실제 freeze는 codeBreak에서
    return
  end
  if method == "step" or method == "resume" then
    reply_err(id, "not_paused", "step and resume require frozen state")
    return
  end
  -- 지연 명령(즉시 응답 안 함; 프레임 경과/exec 후 응답)
  if method == "run_frames" then
    local frames, err = bounded_sync_count(p.n, 1, false)
    if not frames then reply_err(id, "bad_params", err); return end
    deferred = Deferred.start(session_epoch, id, "run", frames)
    return
  end
  if method == "press_buttons" then
    local frames, frame_err = bounded_sync_count(p.frames, 1, false)
    if not frames then reply_err(id, "bad_params", frame_err); return end
    local tbl, err = buttons_to_table(p.buttons)
    if not tbl then reply_err(id, "bad_params", err); return end
    input_hold = { port = p.port or 0, tbl = tbl }
    deferred = Deferred.start(session_epoch, id, "press", frames)
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
  if method == "record_window" then
    if not FreezeState.can_start_recording(freeze_state) then
      reply_err(id, "unsafe_halt", "the current frozen position is not a proven frame boundary")
    else
      local action = start_recording(id, p)
      if action == "started" then return "resume" end
    end
  elseif method == "resume" then
    reply_ok(id, { state = "running" })
    return "resume"
  elseif method == "step" then
    local count, err = bounded_sync_count(p.frames, 1, false)
    if not count then reply_err(id, "bad_params", err); return nil end
    step_unit = (p.unit == "instructions") and "instructions" or "frames"
    step_remaining = count
    pending_step_id = id   -- 완료 응답은 청크들이 끝난 뒤
    freeze_state = FreezeState.halt("step", false)
    return "step"
  elseif method == "pause" then
    reply_ok(id, { state = "frozen" })   -- 멱등
  elseif method == "run_frames" then
    -- frozen이면 원자적으로 resume하며 진행한다 — deferred를 세팅하고 "resume"을 반환해 freeze 루프를 빠져나가
    -- 게임을 재개한다. Rust는 이제 별도 ensure_running(resume)을 안 보낸다(별도 resume은 run_frames 도착 전
    -- free-run으로 one-shot watch/BP를 조기 소진시키는 레이스라 제거됨 — Mednafen과 동일 원자 resume 규약).
    local frames, err = bounded_sync_count(p.n, 1, false)
    if not frames then reply_err(id, "bad_params", err); return nil end
    deferred = Deferred.start(session_epoch, id, "run", frames)
    return "resume"
  elseif method == "press_buttons" then
    local frames, frame_err = bounded_sync_count(p.frames, 1, false)
    if not frames then reply_err(id, "bad_params", frame_err); return nil end
    local tbl, err = buttons_to_table(p.buttons)
    if not tbl then reply_err(id, "bad_params", err); return nil end
    input_hold = { port = p.port or 0, tbl = tbl }
    deferred = Deferred.start(session_epoch, id, "press", frames)
    return "resume"
  elseif method == "save_state" or method == "load_state" or method == "probe" then
    return frozen_state_io(method, id, p)
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

resume_from_freeze = function()
  STATE = "running"
  freeze_start_ms = nil
  freeze_disc_ms = nil
  freeze_snapshot = nil
  emu.resume()
end

-- A command-line script can be loaded after Mesen is already paused. In that case the initial
-- CodeBreak callback predates this Lua context, but the patched host keeps emitting CodeBreakIdle.
-- Adopt that observed native halt as the adapter's frozen state instead of waiting for a frame that
-- cannot occur. This is also the reconciliation path for a GUI pause while the adapter is running.
local function adopt_host_halt(reason)
  if STATE == "frozen" then return end
  STATE = "frozen"
  freeze_state = FreezeState.halt(reason, false)
  freeze_start_ms = wall_ms()
  freeze_disc_ms = nil
  freeze_snapshot = nil
  if #events < EVENT_CAP then
    events[#events + 1] = { type = "user_freeze", reason = reason, frame = frame }
  else
    dropped = dropped + 1
  end
end

-- Native halt callback 한 번의 작업량은 항상 bounded다. TX flush, request, reconnect, deadman을
-- 각각 최대 한 번만 처리하고 즉시 반환한다. 다음 기회는 Mesen의 SleepUntilResume가 guest 실행 없이
-- codeBreakIdle을 다시 발생시킨다.
local function service_frozen_once()
  if STATE ~= "frozen" then return end

  local now = wall_ms()
  if not freeze_start_ms then freeze_start_ms = now end
  if not freeze_snapshot then freeze_snapshot = emu.getState() end

  -- 미완성 response cursor를 한 번만 전진시킨다. would-block이면 다음 idle event까지 기다린다.
  if conn and Tx.pending(tx) and flush_tx() == "resetting" then return end

  -- 직전 explicit step 청크가 다시 halt한 지점. response가 막혀 있으면 guest를 더 진행하지 않는다.
  if pending_step_id then
    if Tx.pending(tx) then return end
    if step_remaining <= 0 then
      freeze_state = FreezeState.after_step(step_unit)
      reply_ok(pending_step_id, { status = "completed", frame = frame })
      pending_step_id = nil
    else
      send_line(string.format('{"id":%d,"ok":true,"result":{"status":"working"}}', pending_step_id))
      do_step_chunk()
    end
    return
  end

  -- 로컬 resume 핫키(frozen→running 토글). native idle 간격이 poll throttle 역할을 한다.
  if FREEZE_KEY and freeze_key_ok then
    local fk = freeze_key_down()
    if fk and not prev_freeze_key then
      prev_freeze_key = fk
      if #events < EVENT_CAP then events[#events + 1] = { type = "user_resume", reason = "hotkey", frame = frame }
      else dropped = dropped + 1 end
      resume_from_freeze()
      return
    end
    prev_freeze_key = fk
  end

  -- 호스트는 request를 직렬화한다. idle callback 하나는 완성된 NDJSON request 한 건만 소비한다.
  local line = poll_line()
  if line then
    freeze_start_ms = wall_ms()
    freeze_disc_ms = nil
    local act = handle_in_freeze(line)
    if act == "resume" then resume_from_freeze(); return end
    if act == "step" then do_step_chunk(); return end
  end

  now = wall_ms()
  if not conn then
    -- transport loss는 freeze ownership을 해제하지 않는다. operator가 명시한 giveup만 release event다.
    freeze_disc_ms = freeze_disc_ms or now
    if RECONNECT_GIVEUP_MS > 0 and now - freeze_disc_ms >= RECONNECT_GIVEUP_MS then
      resume_from_freeze()
      return
    end
    if now - last_reconnect_ms >= 500 then
      last_reconnect_ms = now
      connect()
    end
  elseif freeze_state.reason ~= "hotkey" and MAX_FREEZE_MS > 0
      and now - freeze_start_ms >= MAX_FREEZE_MS then
    -- operator opt-in deadman. hotkey freeze와 기본값(0)은 명시적 release까지 영속한다.
    resume_from_freeze()
  end
end

-- 최초 codeBreak는 freeze 진입 또는 explicit step 청크 완료의 linearization point다.
emu.addEventCallback(function()
  if STATE ~= "frozen" then return end
  if not freeze_start_ms then freeze_start_ms = wall_ms() end
  if not freeze_snapshot then freeze_snapshot = emu.getState() end
end, emu.eventType.codeBreak)

-- Unsafe halt kinds are still serviceable, but savestate operations remain disabled.
emu.addEventCallback(function()
  halt_savestate_safe = false
  adopt_host_halt("host_break")
  service_frozen_once()
end, emu.eventType.codeBreakIdle)

-- Main-CPU Pause/CpuStep halts use a distinct host event whose scripting context permits
-- createSavestate/loadSavestate. This is the only frozen callback that enables state I/O.
emu.addEventCallback(function()
  halt_savestate_safe = true
  adopt_host_halt("host_halt")
  service_frozen_once()
  halt_savestate_safe = false
end, emu.eventType.codeBreakIdleSavestate)

-- Emulator::Reset emits this after the console, input devices, and master clock are reset and before
-- returning to the emulation loop. The reset-origin transaction was fully validated, movie-decoded,
-- and sink-bound before emu.reset() was queued, so this callback performs only the boundary arm.
emu.addEventCallback(function()
  if not recording_reset then return end
  local pending = recording_reset
  recording_reset = nil

  -- A reset release is a new adapter frame boundary even though Mesen's regular StartFrame callback
  -- is emitted only when this new frame completes and the PPU wraps back to scanline zero.
  frame = frame + 1
  local state, effect, kind, message = Recording.start(
    pending.params, pending.request_id, pending.launch_id, frame, pending.started_ms,
    pending.sink, buttons_to_table, install_recording_input, release_recording_input,
    pending.prepared, release_recording_hooks, pending.member_sink,
    capture_initial_snapshots, wall_ms)
  if not state then
    pcall(pending.sink.close)
    if pending.member_sink then pcall(pending.member_sink.close) end
    reply_err(pending.request_id, kind or "adapter_error", message or "reset recording arm failed")
    STATE = "frozen"
    freeze_state = FreezeState.halt("record_window", true)
    emu.breakExecution()
    return
  end

  recording = state
  if pending.cancelled then
    recording, effect = Recording.cancel(recording, frame, "connection_closed")
  end
  if resolve_recording_effect(effect, true) == "freeze" then emu.breakExecution() end
end, emu.eventType.reset)

-- 입력 적용: ROM이 읽기 직전인 inputPolled에서 주입한 입력을 덮어쓴다.
emu.addEventCallback(function()
  local ok, err = apply_input()
  if not ok and recording then
    local effect
    recording, effect = Recording.fail_input(recording, frame, err)
    if apply_recording_effect(effect, false) == "freeze" then emu.breakExecution() end
  end
end, emu.eventType.inputPolled)

-- RUNNING 프레임 루프
emu.addEventCallback(function()
  halt_savestate_safe = false
  frame = frame + 1
  if not conn then connect(); return end
  if Tx.pending(tx) and flush_tx() == "resetting" then return end
  -- A startFrame while frozen means the host was resumed outside the adapter. Explicit adapter
  -- steps are the exception: they may cross a frame boundary before returning to the native halt.
  if STATE == "frozen" then
    if pending_step_id then return end
    STATE = "running"
    freeze_start_ms = nil
    freeze_disc_ms = nil
    freeze_snapshot = nil
    freeze_state = FreezeState.halt("paused", false)
    if #events < EVENT_CAP then
      events[#events + 1] = { type = "user_resume", reason = "host", frame = frame }
    else
      dropped = dropped + 1
    end
  end
  -- A recording transaction completes the prior interval before deciding whether to stop, apply
  -- the next full input state, and publish the next frame boundary.
  -- Control traffic is limited to an identity-bound abort until the original request terminates.
  if recording then
    local effect
    recording, effect = Recording.tick(recording, frame, wall_ms())
    if resolve_recording_effect(effect, true) == "freeze" then
      emu.breakExecution()
      return
    end
    local control = poll_line()
    if not recording then return end
    if control and handle_recording_control(control) == "freeze" then
      emu.breakExecution()
    end
    return
  end
  -- The local freeze hotkey remains an explicit adapter-owned alternative to Mesen's regular Pause.
  -- Deferred operations already own their execution interval, so do not let a host key interrupt one.
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
      STATE = "frozen"; freeze_state = FreezeState.halt("hotkey", false)
      pcall(emu.drawString, 8, 8, "emucap FROZEN (hotkey)", 0xFFFFFF, 0x000000, 0, 180)
      emu.breakExecution()
      return
    end
    prev_freeze_key = fk
  end
  -- 지연 명령(run_frames/press_buttons) 진행 중이면 그것만 진행(에이전트는 대기 중).
  if deferred then tick_deferred(); return end
  local line = poll_line()
  if line and dispatch(line) == "freeze" then
    emu.breakExecution()
  end
end, emu.eventType.startFrame)

CPU = emu.cpuType[SYS.cpu_type]   -- 브레이크포인트/세이브스테이트 exec 콜백용

if emu.getScriptDataFolder() == "" then
  emu.displayMessage("emucap", "I/O access is disabled; enable it in Script Settings")
end
emu.log("emucap-core active control loaded: " .. HOST .. ":" .. PORT)

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
      emu.log("[emucap] pre-armed DMA capture (bp " .. tostring(info.id) .. "): " .. prearm)
    else
      emu.log("[emucap] failed to pre-arm DMA capture: " .. prearm .. " (" .. tostring(info) .. ")")
    end
  end
end
