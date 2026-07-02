-- emucap Mesen2 어댑터 (회고 덤프)
-- "Allow access to I/O and OS functions" 옵션이 켜져 있어야 파일 출력이 된다.
--
-- 세이브스테이트는 exec 메모리 콜백 컨텍스트에서만 만들 수 있다(이벤트 콜백에서는
-- 실패). 따라서 startFrame에서 샘플 시점을 정하고, 그 프레임의 다음 명령에서
-- 발화하는 1회용 exec 콜백 안에서 createSavestate를 호출한 뒤 콜백을 즉시 해제한다.

local OUTPUT_ROOT = "bundles"        -- 작업 디렉토리 기준 상대 경로
local ROM_PATH    = "roms/game.sfc"  -- 자동 추론 실패 시 폴백 경로
local PLATFORM    = "snes"
local CPU_TYPE    = emu.cpuType.snes -- 콘솔 변경 시 함께 바꾼다
local ADDR_LO     = 0
local ADDR_HI     = 0xFFFFFF         -- SNES CPU 주소공간(24비트)
local INTERVAL    = 30               -- 샘플 간격(프레임)
local DEPTH       = 8                -- 링 깊이(슬라이스 수)
-- 호스트 키 조합(Ctrl+Shift+C). 반드시 유효한 Mesen2 키 이름이어야 한다 — 모디파이어는
-- 공백 포함 표기다("Left Ctrl"/"Left Shift", "LeftCtrl"·"Ctrl"은 무효). 유효 이름은
-- 키 이름 프로브로 확인한다.
local TRIGGER_KEYS = { "Left Ctrl", "Left Shift", "C" }

local frame = 0
local ring = {}        -- { {frame=, state=, screen=, input=}, ... } 최신이 뒤
local prev_combo = false
local sample_pending = false
local sample_frame = 0
local exec_ref = nil
local logged_first = false

-- 잘못된 키 이름은 isKeyPressed가 에러를 던진다. pcall로 감싸 "안 눌림"으로 처리한다.
-- 유효한 키 이름은 키 이름 프로브로 확인한다.
local function key_down(k)
  local ok, pressed = pcall(emu.isKeyPressed, k)
  return ok and pressed
end

local function combo_pressed()
  for _, k in ipairs(TRIGGER_KEYS) do
    if not key_down(k) then return false end
  end
  return true
end

local function write_file(path, data)
  local f = assert(io.open(path, "wb"))
  f:write(data)
  f:close()
end

local function json_escape(s) return (s:gsub('[\\"]', '\\%0')) end

-- 실행 중인 ROM 경로를 getRomInfo로 자동 추론한다(반환: { name, path, fileSha1Hash }).
-- 실패하거나 비면 ROM_PATH 폴백. finalize는 이 경로로 SHA-1을 계산하므로 보통 --rom이
-- 필요 없다(경로가 틀리면 emucap finalize --rom 으로 덮어쓴다).
local function detect_rom_path()
  local ok, info = pcall(emu.getRomInfo)
  if ok and type(info) == "table" and type(info.path) == "string" and #info.path > 0 then
    return info.path
  end
  return ROM_PATH
end

-- input 테이블을 "frame:button,button" 한 줄로 직렬화(자체 최소 포맷)
local function input_line(fr, input)
  local parts = {}
  for k, v in pairs(input) do
    if v == true then parts[#parts + 1] = tostring(k) end
  end
  table.sort(parts)
  return fr .. ":" .. table.concat(parts, ",")
end

local function dump_bundle()
  local ts = os.time()
  local rom = detect_rom_path()
  local dir = OUTPUT_ROOT .. "/" .. ts .. "-retrospective"
  os.execute('mkdir -p "' .. dir .. '/slices"')

  local slices_json = {}
  local movie_lines = {}
  for _, s in ipairs(ring) do
    local fname = string.format("f%05d", s.frame)
    local sdir = dir .. "/slices/" .. fname
    os.execute('mkdir -p "' .. sdir .. '"')
    write_file(sdir .. "/state.mss", s.state)
    write_file(sdir .. "/screen.png", s.screen)
    movie_lines[#movie_lines + 1] = input_line(s.frame, s.input)
    slices_json[#slices_json + 1] = string.format(
      '{ "frame": %d, "artifacts": [' ..
      '{ "kind": "savestate", "path": "slices/%s/state.mss" },' ..
      '{ "kind": "screenshot", "path": "slices/%s/screen.png" } ] }',
      s.frame, fname, fname)
  end
  write_file(dir .. "/input.movie", table.concat(movie_lines, "\n"))

  local raw = string.format(
    '{\n  "format_version": 1,\n  "platform": "%s",\n  "rom_path": "%s",\n' ..
    '  "adapter": { "name": "mesen2", "version": "0.1" },\n' ..
    '  "emulator": { "name": "Mesen2", "version": "unknown" },\n' ..
    '  "trigger": { "kind": "retrospective", "at_unix_ms": %d, "at_frame": %d },\n' ..
    '  "ring_policy": { "interval_frames": %d, "depth": %d },\n' ..
    '  "slices": [%s],\n  "input_movie": "input.movie"\n}',
    json_escape(PLATFORM), json_escape(rom), ts * 1000, frame,
    INTERVAL, DEPTH, table.concat(slices_json, ","))
  write_file(dir .. "/_raw.json", raw)

  emu.displayMessage("emucap", "캡처됨 → " .. dir)
  emu.drawString(8, 8, "EMUCAP CAPTURED", 0xFFFFFF, 0x000000, 0, 120)
end

-- 1회용 exec 콜백: 샘플 시점의 다음 명령에서 발화 → 세이브스테이트 후 자기 해제
local function on_exec_sample()
  if not sample_pending then return end
  sample_pending = false
  if exec_ref then
    emu.removeMemoryCallback(exec_ref, emu.callbackType.exec, ADDR_LO, ADDR_HI, CPU_TYPE)
    exec_ref = nil
  end

  local state = emu.createSavestate()
  ring[#ring + 1] = {
    frame  = sample_frame,
    state  = state,
    screen = emu.takeScreenshot(),
    input  = emu.getInput(0),
  }
  while #ring > DEPTH do table.remove(ring, 1) end

  if not logged_first then
    logged_first = true
    emu.log("emucap: 첫 샘플 저장 (state " .. #state .. " bytes)")
  end
end

emu.addEventCallback(function()
  frame = frame + 1

  -- 샘플 예약: 다음 명령에서 발화할 exec 콜백을 등록
  if frame % INTERVAL == 0 and not sample_pending then
    sample_pending = true
    sample_frame = frame
    exec_ref = emu.addMemoryCallback(on_exec_sample, emu.callbackType.exec, ADDR_LO, ADDR_HI, CPU_TYPE)
  end

  -- 트리거: 키 조합 상승 에지에서 회고 덤프
  local now = combo_pressed()
  if now and not prev_combo and #ring > 0 then
    dump_bundle()
  end
  prev_combo = now
end, emu.eventType.startFrame)

-- I/O 접근이 꺼져 있으면 트리거가 발화해도 파일을 못 쓴다. 로드 시 미리 경고한다.
-- (getScriptDataFolder는 I/O 접근이 꺼져 있으면 빈 문자열을 반환한다.)
if emu.getScriptDataFolder() == "" then
  emu.displayMessage("emucap", "I/O 접근 꺼짐 — Script Settings에서 켜야 캡처 저장됨")
  emu.log("emucap 경고: I/O 접근이 꺼져 있어 캡처를 저장할 수 없습니다.")
  emu.log("  Script -> Settings -> Script Window -> Restrictions ->")
  emu.log("  'Allow access to I/O and OS functions' 를 켜고 스크립트를 다시 로드하세요.")
end

emu.log("emucap: ROM 경로 = " .. detect_rom_path())
emu.log("emucap 어댑터 로드됨: " .. #TRIGGER_KEYS .. "키 조합으로 회고 덤프")
