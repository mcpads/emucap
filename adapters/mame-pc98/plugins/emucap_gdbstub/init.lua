-- license:BSD-3-Clause
-- Derived from MAME's bundled gdbstub plugin.

local exports = {
  name = "emucap_gdbstub",
  version = "0.1.0",
  description = "emucap MAME GDB bridge helper",
  license = "BSD-3-Clause",
  author = { name = "emucap" }
}

local emucap_gdbstub = exports

local regmaps = {
  i386 = {
    togdb = {
      EAX = 1, ECX = 2, EDX = 3, EBX = 4, ESP = 5, EBP = 6, ESI = 7, EDI = 8, EIP = 9, EFLAGS = 10,
      CS = 11, SS = 12, DS = 13, ES = 14, FS = 15, GS = 16
    },
    fromgdb = {
      "EAX", "ECX", "EDX", "EBX", "ESP", "EBP", "ESI", "EDI", "EIP", "EFLAGS", "CS", "SS", "DS", "ES", "FS", "GS"
    },
    regsize = 4,
    addrsize = 4,
    pcreg = "EIP"
  }
}

regmaps.m68000 = {
  togdb = {
    D0 = 1, D1 = 2, D2 = 3, D3 = 4, D4 = 5, D5 = 6, D6 = 7, D7 = 8,
    A0 = 9, A1 = 10, A2 = 11, A3 = 12, A4 = 13, A5 = 14, A6 = 15, SP = 16,
    SR = 17, PC = 18
  },
  fromgdb = {
    "D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7",
    "A0", "A1", "A2", "A3", "A4", "A5", "A6", "SP", "SR", "PC"
  },
  regsize = 4,
  addrsize = 4,
  pcreg = "PC"
}

regmaps.i386sx = regmaps.i386
regmaps.i486 = regmaps.i386
regmaps.pentium = regmaps.i386

local reset_subscription, stop_subscription, frame_subscription
local mame_profile = os.getenv("EMUCAP_MAME_PROFILE") or "pc98"
local input_aliases = {
  escape = "esc",
  return_key = "enter",
  ["return"] = "enter",
  start = "enter",
  select = "space",
  delete = "del",
  insert = "ins",
  pageup = "pgup",
  pagedown = "pgdn",
  back_space = "backspace",
  bksp = "backspace",
  bs = "backspace"
}

local function chksum(str)
  local sum = 0
  str:gsub(".", function(s) sum = sum + s:byte() end)
  return string.format("%.2x", sum & 0xff)
end

local function makele(val, len)
  local str = ""
  for count = 0, len - 1 do
    str = str .. string.format("%.2x", (val >> (count * 8)) & 0xff)
  end
  return str
end

local function fromle(str)
  local val = 0
  for count = 0, (#str // 2) - 1 do
    local byte = tonumber(str:sub(count * 2 + 1, count * 2 + 2), 16) or 0
    val = val | (byte << (count * 8))
  end
  return val
end

local function hex_to_string(hex)
  local out = {}
  for i = 1, #hex, 2 do
    local b = tonumber(hex:sub(i, i + 1), 16)
    if not b then return nil end
    out[#out + 1] = string.char(b)
  end
  return table.concat(out)
end

local function trim(str)
  return tostring(str):gsub("^%s+", ""):gsub("%s+$", "")
end

local function norm_key(str)
  local s = trim(str):lower()
  s = s:gsub("%s+", " ")
  if mame_profile == "neogeo" and s == "start" then
    return "start"
  end
  return input_aliases[s] or s
end

local function neogeo_input_name(field)
  if field.player ~= 0 then
    return nil
  end
  local name = trim(field.name):lower()
  local button = name:match("^p1 ([abcd])$")
  if button then return button end
  local direction = name:match("^p1 (%a+)$")
  if direction == "up" or direction == "down" or direction == "left" or direction == "right" then
    return direction
  end
  if name == "1 player start" then return "start" end
  if name == "coin 1" then return "coin" end
  if name == "service 1" then return "service" end
  return nil
end

local function split_csv(str)
  local out = {}
  for raw in tostring(str):gmatch("([^,]+)") do
    local key = norm_key(raw)
    if key ~= "" then
      out[#out + 1] = key
    end
  end
  return out
end

local function packet(socket, payload)
  socket:write("$" .. payload .. "#" .. chksum(payload))
end

local function ack_packet(socket, payload)
  socket:write("+")
  packet(socket, payload)
end

function emucap_gdbstub.startplugin()
  local debugger
  local cpu
  local breaks
  local watches
  local regpoints
  local consolelog
  local consolelast = 0
  local running = false
  -- freeze 홀드 의도 플래그: note_stop이 execution_state="stop"을 봐도 *명시적 freeze 의도가 있을 때만* 홀드하고,
  -- 없으면(부팅 temp BP 등 MAME 내부/자동 스톱) go()로 넘긴다. 의도를 execution_state에서 추론하지 않는다 —
  -- emu.pause()가 execution_state를 stop→run으로 뒤집어 추론이 신뢰불가라 부팅 temp BP를 홀드해 racy하게 정지한다.
  -- explicit pause(\x03)·step 관측·frame-target-with-pause·user BP(pause_on_hit)만 이 의도를 세팅한다.
  local hold_requested = false
  local rxbuf = ""
  local input_fields = {}
  local active_input_fields = {}
  local release_input_frame
  local frame_wait_target
  local frame_wait_stop
  local frame_wait_probe
  local frame_wait_release_input = false
  local clear_inputs
  local break_on_reset_enabled = false
  local pending_reset
  local socket
  local regs_payload
  -- 진단 계측(env-gated): EMUCAP_GDBSTUB_TRACE=1일 때만 freeze 흐름을 MAME 로그로 찍는다.
  -- 끄면 no-op. freeze 흐름 진단용.
  local trace_enabled = os.getenv("EMUCAP_GDBSTUB_TRACE") == "1"
  local trace_last_state = nil
  local function trace(msg)
    if trace_enabled then print("emucap_gdbstub: TRACE " .. tostring(msg)) end
  end

  -- frame-target 명령은 응답 수명주기와 임시 입력 소유권을 함께 끝낸다. press가 완료되거나
  -- breakpoint/stop/reset으로 중단될 때 입력만 남아 네이티브 키보드를 덮지 않게 한 곳에서 해제한다.
  local function clear_frame_wait()
    local release_input = frame_wait_release_input
    frame_wait_target = nil
    frame_wait_stop = false
    frame_wait_probe = nil
    frame_wait_release_input = false
    if release_input and clear_inputs then
      clear_inputs()
    end
  end

  reset_subscription = emu.add_machine_reset_notifier(function()
    debugger = manager.machine.debugger
    if not debugger then
      print("emucap_gdbstub: debugger not enabled")
      return
    end
    cpu = manager.machine.devices[":maincpu"]
    if not cpu then
      print("emucap_gdbstub: maincpu not found")
      return
    end
    if not regmaps[cpu.shortname] then
      print("emucap_gdbstub: no register map for cpu " .. cpu.shortname)
      cpu = nil
      return
    end
    consolelog = debugger.consolelog
    consolelast = 0
    breaks = { byaddr = {}, byidx = {}, pause = {} }
    watches = { byaddr = {}, byidx = {} }
    regpoints = { byidx = {} }
    running = false
    rxbuf = ""
    -- A reset can interrupt a deferred press before the frame notifier reaches its target.
    -- Release that transient override while its field references are still available.
    clear_frame_wait()
    input_fields = {}
    active_input_fields = {}
    release_input_frame = nil
    frame_wait_target = nil
    frame_wait_stop = false
    frame_wait_probe = nil
    frame_wait_release_input = false
    if break_on_reset_enabled and socket and debugger and cpu then
      local map = regmaps[cpu.shortname]
      if map then
        running = false
        debugger.execution_state = "stop"
        pending_reset = makele(cpu.state[map.pcreg].value, map.addrsize) .. "|" .. regs_payload(map)
      end
    end
    for _, port in pairs(manager.machine.ioport.ports) do
      for _, field in pairs(port.fields) do
        if mame_profile == "neogeo" then
          local key = neogeo_input_name(field)
          if key and not input_fields[key] then
            input_fields[key] = field
          end
        elseif field.type_class == "keyboard" then
          local names = { field.name, field.default_name }
          for _, name in ipairs(names) do
            local n = norm_key(name)
            if n ~= "" and not input_fields[n] then
              input_fields[n] = field
            end
            for part in tostring(name):gmatch("([^/]+)") do
              local p = norm_key(part)
              if p ~= "" and not input_fields[p] then
                input_fields[p] = field
              end
            end
          end
        end
      end
    end
    print("emucap_gdbstub: ready cpu=" .. cpu.shortname)
  end)

  stop_subscription = emu.add_machine_stop_notifier(function()
    consolelog = nil
    cpu = nil
    debugger = nil
  end)

  local port = os.getenv("MAME_GDB_PORT") or "2159"
  socket = emu.file("", 7)
  local err = socket:open("socket.127.0.0.1:" .. port)
  if err then
    print("emucap_gdbstub: socket open returned " .. tostring(err))
  end
  print("emucap_gdbstub: listening on 127.0.0.1:" .. port)

  local run_debugger_command
  local handle
  local service_frozen_socket
  local in_frozen_socket_service = false

  local function read_program_hex(addr, len)
    local out = {}
    local space = cpu.spaces["program"]
    for count = 1, len do
      out[count] = string.format("%.2x", space:readv_u8(addr))
      addr = addr + 1
    end
    return table.concat(out)
  end

  local function stop_debugger()
    if debugger then
      debugger.execution_state = "stop"
    end
    running = false
    clear_frame_wait()
  end

  local function apply_regs_hex(regs_hex)
    local map = regmaps[cpu.shortname]
    local count = 0
    tostring(regs_hex):gsub(string.rep("%x", map.regsize * 2), function(s)
      count = count + 1
      local reg = map.fromgdb[count]
      if reg then
        cpu.state[reg].value = fromle(s)
      end
    end)
    if debugger then
      debugger.execution_state = "stop"
    end
    running = false
    clear_frame_wait()
    return regs_payload(map)
  end

  local function note_stop()
    -- detect stop independently of running flag (also catches frame-wait stops)
    if not debugger or debugger.execution_state ~= "stop" then
      return false
    end
    -- 이미 홀드 중(machine.paused)이면 재통지/재홀드 없음 — svcfrozen 스핀 내에서 명령을 처리한다.
    if manager.machine.paused then
      return running or frame_wait_target
    end
    -- go-vs-hold를 *통지 전에* 결정한다. 명시적 freeze 의도(hold_requested)가 없으면(부팅 temp BP 등 MAME 내부/자동
    -- 스톱) *통지 없이* go()로 넘긴다 — S05를 먼저 보내면 브리지가 "멈췄다"고 오해하나 실제론 계속 진행이라 통지를
    -- 안 한다. 명시적 의도 없이 모든 stop을 홀드하면 부팅 temp BP를 잡아 racy하게 정지한다(note_bp가 "temporary
    -- breakpoint"를 정규식 no-match→note_stop 홀드). 의도는 explicit pause(\x03)·step·frame-target·user BP만 세팅한다.
    if not hold_requested then
      cpu.debug:go()
      return true
    end
    hold_requested = false
    -- 홀드 확정: 새 stop(running/frame_wait서 전환)이면 S05(+pc/regs) 통지하고, emu.pause로 실제 halt한다.
    -- (pause는 \x03 핸들러가·frame-target은 frame notifier가 이미 통지했으므로 여기선 is_new_stop만 추가 통지 —
    -- 중복 없음. step은 running=true라 여기서 통지.)
    local is_new_stop = running or frame_wait_target
    if is_new_stop then
      local reply_to_frame_wait = frame_wait_target ~= nil
      clear_frame_wait()
      local map = cpu and regmaps[cpu.shortname]
      local stop_payload
      if map then
        stop_payload = "T05pc:" .. makele(cpu.state[map.pcreg].value, map.addrsize) .. ";regs:" .. regs_payload(map)
      else
        stop_payload = "S05"
      end
      if reply_to_frame_wait then
        ack_packet(socket, stop_payload)
      else
        packet(socket, stop_payload)
      end
      running = false
    end
    service_frozen_socket()
    return true
  end

  regs_payload = function(map)
    local regs = {}
    for reg, idx in pairs(map.togdb) do
      regs[idx] = makele(cpu.state[reg].value, map.regsize)
    end
    return table.concat(regs)
  end

  local function note_breakpoint()
    -- detect stop by execution_state, not running flag; consolelast prevents duplicates
    if not debugger or debugger.execution_state ~= "stop" then
      return false
    end
    if not consolelog or not cpu then
      return false
    end
    local last = consolelast
    local msg = consolelog[#consolelog]
    consolelast = #consolelog
    if #consolelog <= last or not msg or not msg:find("Stopped at", 1, true) then
      return false
    end

    trace("note_bp detected msg=[" .. tostring(msg) .. "] execstate=" .. tostring(debugger.execution_state))
    local map = regmaps[cpu.shortname]
    local point = tonumber(msg:match("Stopped at breakpoint ([0-9]+)"))
    if point then
      running = false
      local reply_to_frame_wait = frame_wait_target ~= nil
      clear_frame_wait()
      local pause_on_hit = breaks.pause and breaks.pause[point]
      local addr = breaks.byidx[point]
      local payload
      if addr then
        -- use bpclear (not bpdisable) so re-arm at same address succeeds
        run_debugger_command("bpclear " .. tostring(point))
        breaks.byaddr[addr] = nil
        breaks.byidx[point] = nil
        if breaks.pause then breaks.pause[point] = nil end
        payload = "T05hwbreak:" .. makele(addr, map.addrsize) .. ";idx:" .. tostring(point) .. ";regs:" .. regs_payload(map)
      else
        payload = "S05"
      end
      if reply_to_frame_wait then
        ack_packet(socket, payload)
      else
        packet(socket, payload)
      end
      trace("note_bp done bp idx=" .. tostring(point) .. " execstate=" .. tostring(debugger.execution_state) .. " running=" .. tostring(running) .. " fwt=" .. tostring(frame_wait_target))
      -- freeze hold: pause_on_hit이면 다른 stop 경로처럼 stop 재확정 후 frozen 소켓 서비스로
      -- 잡아둔다(이 단계가 없으면 go() 상태 MAME가 재개).
      -- pause_on_hit=false면 이 hold를 건너뛰어 MAME가 그대로 진행한다(트레이스포인트; bridge continue 불요).
      if pause_on_hit then
        debugger.execution_state = "stop"
        trace("note_bp hold bp idx=" .. tostring(point))
        service_frozen_socket()
      else
        -- pause_on_hit=false(트레이스포인트/collect-mode)는 히트만 기록하고 계속 실행해야 하나,
        -- BP 히트로 debugger가 stop 상태라 다음 틱 note_stop이 emu.pause로 잡아버린다(회귀). 명시적으로 go()해 재개.
        cpu.debug:go()
      end
      return true
    end

    point = tonumber(msg:match("Stopped at watchpoint ([0-9]+)"))
    if point then
      running = false
      local reply_to_frame_wait = frame_wait_target ~= nil
      clear_frame_wait()
      local wp = watches.byidx[point]
      local pause_on_hit = wp and wp.pause_on_hit
      local payload
      if wp then
        -- use wpclear (not wpdisable) so re-arm at same address succeeds
        run_debugger_command("wpclear " .. tostring(point))
        watches.byidx[point] = nil
        watches.byaddr[wp.key] = nil
        payload = "T05" .. wp.type .. ":" .. makele(wp.addr, map.addrsize) .. ";idx:" .. tostring(point) .. ";regs:" .. regs_payload(map)
      else
        payload = "S05"
      end
      if reply_to_frame_wait then
        ack_packet(socket, payload)
      else
        packet(socket, payload)
      end
      trace("note_bp done wp idx=" .. tostring(point) .. " execstate=" .. tostring(debugger.execution_state) .. " running=" .. tostring(running) .. " fwt=" .. tostring(frame_wait_target))
      if pause_on_hit then
        debugger.execution_state = "stop"
        trace("note_bp hold wp idx=" .. tostring(point))
        service_frozen_socket()
      else
        cpu.debug:go()  -- pause_on_hit=false 워치포인트: 히트 기록 후 재개(note_stop이 잡지 않게)
      end
      return true
    end

    point = tonumber(msg:match("Stopped at registerpoint ([0-9]+)"))
    if point then
      running = false
      local reply_to_frame_wait = frame_wait_target ~= nil
      clear_frame_wait()
      local rp = regpoints.byidx[point]
      local pause_on_hit = rp and rp.pause_on_hit
      local payload
      if rp then
        -- use rpclear (not rpdisable) so the registerpoint can be re-set
        run_debugger_command("rpclear " .. tostring(point))
        regpoints.byidx[point] = nil
        payload = "T05regwatch:" .. makele(cpu.state[map.pcreg].value, map.addrsize) .. ";idx:" .. tostring(point) .. ";regs:" .. regs_payload(map)
      else
        payload = "S05"
      end
      if reply_to_frame_wait then
        ack_packet(socket, payload)
      else
        packet(socket, payload)
      end
      if pause_on_hit then
        debugger.execution_state = "stop"
        trace("note_bp hold rp idx=" .. tostring(point))
        service_frozen_socket()
      else
        cpu.debug:go()  -- pause_on_hit=false 레지스터포인트: 히트 기록 후 재개(note_stop이 잡지 않게)
      end
      return true
    end
    return false
  end

  local function read_socket()
    local data = ""
    repeat
      local read = socket:read(1024)
      data = data .. read
    until #read == 0
    if #data > 0 then
      rxbuf = rxbuf .. data
    end
  end

  local function next_packet()
    local interrupt_at = rxbuf:find("\x03", 1, true)
    if interrupt_at then
      rxbuf = rxbuf:sub(1, interrupt_at - 1) .. rxbuf:sub(interrupt_at + 1)
      return "\x03"
    end
    local start = rxbuf:find("%$")
    if not start then
      if #rxbuf > 4096 then rxbuf = "" end
      return nil
    end
    local hash = rxbuf:find("#", start + 1, true)
    if not hash or #rxbuf < hash + 2 then
      if start > 1 then rxbuf = rxbuf:sub(start) end
      return nil
    end
    local payload = rxbuf:sub(start + 1, hash - 1)
    rxbuf = rxbuf:sub(hash + 3)
    payload = payload:gsub("}(.)", function(s) return string.char(string.byte(s) ~ 0x20) end)
    return payload
  end

  local function idle_briefly()
    -- freeze 스핀이 소켓 데이터를 기다리는 짧은 대기. os.execute("sleep")은 (a) Unix에서 idle마다
    -- fork+exec 폭풍을 내고 (b) Windows엔 sleep 바이너리가 없어 즉시 실패→대기 없는 100% 스핀이
    -- 된다(pcall이 에러를 삼킴). 프로세스를 스폰하지 않는 osd_ticks 바운드 대기로 대체해 양
    -- 플랫폼 공통으로 폴 간격을 둔다(register_periodic 콜백 안이라 코루틴 yield/sleep API는 없음).
    local ticks = emu.osd_ticks
    local hz = emu.osd_ticks_per_second
    if ticks and hz then
      local rate = hz()
      if rate and rate > 0 then
        local deadline = ticks() + rate // 200  -- ~5ms
        while ticks() < deadline do end
        return
      end
    end
    -- osd_ticks 미노출 시 표준 Lua os.clock 폴백(여전히 프로세스 스폰 없음).
    local deadline = os.clock() + 0.005
    while os.clock() < deadline do end
  end

  service_frozen_socket = function()
    if in_frozen_socket_service or not handle then
      return
    end
    in_frozen_socket_service = true
    -- soft-pause: execution_state="stop"만으론 PC-98 CPU가 실제로 안 멈춘다
    -- (register_periodic를 이 스핀이 블록해도 emulation은 계속 — frozen 중 pc 드리프트·trace 증가·타이머 진행).
    -- emu.pause()로 머신을 실제 halt한다(검증: pc 완전 고정·크래시 없음; register_periodic은 paused여도 발화해
    -- 이 스핀의 소켓 서비스가 유지됨). 스핀 탈출(=step/continue/frame로 재개)에서 emu.unpause()한다. 우리가 pause한
    -- 경우만 unpause(외부/중첩 pause 상태 보존).
    local paused_here = false
    if manager.machine and not manager.machine.paused then
      emu.pause()
      paused_here = true
    end
    trace("svcfrozen enter execstate=" .. tostring(debugger and debugger.execution_state) .. " running=" .. tostring(running) .. " fwt=" .. tostring(frame_wait_target) .. " paused_here=" .. tostring(paused_here))
    -- 홀드 가드는 machine.paused로 판단한다(execution_state가 아님) — emu.pause()가 debugger.execution_state를
    -- stop→run으로 뒤집어(디버그 트레이스), execution_state를 가드로 쓰면 첫 이터레이션에 즉시 탈출하는 자기모순이
    -- 된다. 재개(continue/step/run_frames)는 running/frame_wait_target로 신호되고 그때 아래서 emu.unpause한다.
    while cpu and debugger and manager.machine.paused and not running and not frame_wait_target do
      -- 소켓 read/handle 에러(bridge 끊김)가 스핀 밖으로 전파하면 아래 emu.unpause·가드 리셋을
      -- 우회해 머신을 영구 stuck-paused + in_frozen_socket_service를 래치시킨다(재접속해도 복구 불가). pcall로 감싸 에러 시 루프를 탈출해 cleanup이 항상 돌게 한다.
      local handled = false
      local iter_ok = pcall(function()
        read_socket()
        while true do
          local payload = next_packet()
          if not payload then
            break
          end
          handled = true
          handle(payload)
          if running or frame_wait_target or not debugger or not manager.machine.paused then
            break
          end
        end
      end)
      if not iter_ok or running or frame_wait_target or not debugger or not manager.machine.paused then
        break
      end
      if not handled then
        idle_briefly()
      end
    end
    -- 스핀 탈출 = 재개(step/continue/frame-wait). 우리가 pause했으면 실제 머신을 unpause해 진행시킨다
    -- (debugger execution_state="run"/go만으론 emu.pause된 머신이 안 도므로 emu.unpause가 필수).
    if paused_here and manager.machine and manager.machine.paused then
      emu.unpause()
    end
    trace("svcfrozen exit execstate=" .. tostring(debugger and debugger.execution_state) .. " running=" .. tostring(running) .. " fwt=" .. tostring(frame_wait_target) .. " paused_here=" .. tostring(paused_here))
    in_frozen_socket_service = false
  end

  run_debugger_command = function(command)
    local before = consolelog and #consolelog or 0
    debugger:command(command)
    if consolelog then
      for i = before + 1, #consolelog do
        print("emucap_gdbstub: debugger: " .. tostring(consolelog[i]))
      end
    end
  end

  local function set_bp(addr, condition, reply_index, pause_on_hit)
    if breaks.byaddr[addr] then
      ack_packet(socket, "E00")
      return
    end
    local before = consolelog and #consolelog or 0
    local command = string.format("bpset %X", addr)
    if condition and condition ~= "" then
      command = command .. "," .. condition
    end
    run_debugger_command(command)
    local idx
    if consolelog then
      for i = before + 1, #consolelog do
        idx = tonumber(tostring(consolelog[i]):match("Breakpoint (%d+) set"))
        if idx then break end
      end
    end
    if not idx then
      ack_packet(socket, "E0A")
      return
    end
    breaks.byaddr[addr] = idx
    breaks.byidx[idx] = addr
    breaks.pause[idx] = pause_on_hit and true or false
    if reply_index then
      ack_packet(socket, "BP:" .. tostring(idx))
    else
      ack_packet(socket, "OK")
    end
  end

  local function first_screen()
    return manager.machine.screens and manager.machine.screens:at(1)
  end

  local function current_frame()
    local screen = first_screen()
    if screen then
      local frame = screen.frame_number
      if type(frame) == "function" then
        return frame(screen)
      end
      return frame
    end
    return 0
  end

  local function start_frame_wait(frames, stop_on_done, release_input_on_done)
    if frame_wait_target then
      return false
    end
    frames = math.max(tonumber(frames) or 1, 1)
    frame_wait_target = current_frame() + frames
    frame_wait_stop = stop_on_done and true or false
    frame_wait_release_input = release_input_on_done and true or false
    trace("framewait start frames=" .. tostring(frames) .. " stop_on_done=" .. tostring(stop_on_done) .. " execstate_was=" .. tostring(debugger and debugger.execution_state))
    hold_requested = false  -- 프레임 진행은 재개 상태 — 홀드 의도 없음(stop_on_done이면 target 도달 시 frame notifier가 세팅)
    if debugger and debugger.execution_state == "stop" then
      cpu.debug:go()
    end
    running = true
    trace("framewait go done execstate=" .. tostring(debugger and debugger.execution_state))
    return true
  end

  frame_subscription = emu.add_machine_frame_notifier(function()
    if not frame_wait_target then
      return
    end
    if current_frame() < frame_wait_target then
      return
    end

    local should_stop = frame_wait_stop
    local probe = frame_wait_probe
    clear_frame_wait()
    if should_stop and debugger then
      running = false
      debugger.execution_state = "stop"
      hold_requested = true  -- frame-target 도달 + stop 요청 → note_stop이 홀드(내부 스톱과 구분)
    end
    if probe then
      local map = regmaps[cpu.shortname]
      ack_packet(socket, "HEX:" .. read_program_hex(probe.addr, probe.len) .. "|FRAME:" .. tostring(current_frame()) .. "|REGS:" .. regs_payload(map))
    else
      ack_packet(socket, "OK")
    end
  end)

  clear_inputs = function()
    for field, _ in pairs(active_input_fields) do
      field:clear_value()
    end
    active_input_fields = {}
    release_input_frame = nil
  end

  local function set_inputs(keys)
    clear_inputs()
    for _, key in ipairs(keys) do
      local field = input_fields[norm_key(key)]
      if not field then
        -- 미해결 키를 호출자에 돌려줘 브리지가 어느 버튼이 없는지 이름을 붙일 수 있게 한다.
        clear_inputs()
        return false, norm_key(key)
      end
      field:set_value(1)
      active_input_fields[field] = true
    end
    return true
  end

  local function check_input_release()
    if release_input_frame and current_frame() >= release_input_frame then
      clear_inputs()
    end
  end

  local function clear_bp(addr)
    if not breaks.byaddr[addr] then
      ack_packet(socket, "E00")
      return
    end
    local idx = breaks.byaddr[addr]
    run_debugger_command("bpclear " .. tostring(idx))
    breaks.byaddr[addr] = nil
    breaks.byidx[idx] = nil
    ack_packet(socket, "OK")
  end

  local function wp_key(kind, addr, len)
    return tostring(kind) .. ":" .. tostring(addr) .. ":" .. tostring(len)
  end

  local function watch_type_for_rsp(btype)
    if btype == "2" then
      return "w", "watch"
    elseif btype == "3" then
      return "r", "rwatch"
    elseif btype == "4" then
      return "rw", "awatch"
    end
    return nil, nil
  end

  local function set_wp(btype, addr, len, condition, reply_index, pause_on_hit)
    local kind, stop_type = watch_type_for_rsp(btype)
    if not kind then
      ack_packet(socket, "E00")
      return
    end
    len = math.max(tonumber(len) or 1, 1)
    local key = wp_key(kind, addr, len)
    if watches.byaddr[key] then
      ack_packet(socket, "E00")
      return
    end
    local before = consolelog and #consolelog or 0
    local command = string.format("wp %X,%X,%s", addr, len, kind)
    if condition and condition ~= "" then
      command = command .. "," .. condition
    end
    run_debugger_command(command)
    local idx
    if consolelog then
      for i = before + 1, #consolelog do
        idx = tonumber(tostring(consolelog[i]):match("Watchpoint (%d+) set"))
        if idx then break end
      end
    end
    if not idx then
      ack_packet(socket, "E0B")
      return
    end
    watches.byaddr[key] = idx
    watches.byidx[idx] = { addr = addr, len = len, kind = kind, type = stop_type, key = key, condition = condition or "", pause_on_hit = pause_on_hit and true or false }
    if reply_index then
      ack_packet(socket, "WP:" .. tostring(idx))
    else
      ack_packet(socket, "OK")
    end
  end

  local function clear_wp(btype, addr, len)
    local kind = watch_type_for_rsp(btype)
    if not kind then
      ack_packet(socket, "E00")
      return
    end
    len = math.max(tonumber(len) or 1, 1)
    local key = wp_key(kind, addr, len)
    if not watches.byaddr[key] then
      ack_packet(socket, "E00")
      return
    end
    local idx = watches.byaddr[key]
    run_debugger_command("wpclear " .. tostring(idx))
    watches.byaddr[key] = nil
    watches.byidx[idx] = nil
    ack_packet(socket, "OK")
  end

  local function clear_bp_idx(idx)
    idx = tonumber(idx or "")
    if not idx or not breaks.byidx[idx] then
      ack_packet(socket, "E00")
      return
    end
    local addr = breaks.byidx[idx]
    run_debugger_command("bpclear " .. tostring(idx))
    breaks.byidx[idx] = nil
    breaks.byaddr[addr] = nil
    ack_packet(socket, "OK")
  end

  local function clear_wp_idx(idx)
    idx = tonumber(idx or "")
    if not idx or not watches.byidx[idx] then
      ack_packet(socket, "E00")
      return
    end
    local wp = watches.byidx[idx]
    run_debugger_command("wpclear " .. tostring(idx))
    watches.byidx[idx] = nil
    watches.byaddr[wp.key] = nil
    ack_packet(socket, "OK")
  end

  local function set_rp(condition, pause_on_hit)
    if not condition or condition == "" then
      ack_packet(socket, "E00")
      return
    end
    local before = consolelog and #consolelog or 0
    run_debugger_command("rpset " .. condition)
    local idx
    if consolelog then
      for i = before + 1, #consolelog do
        idx = tonumber(tostring(consolelog[i]):match("Registerpoint (%d+) set"))
        if idx then break end
      end
    end
    if not idx then
      ack_packet(socket, "E0D")
      return
    end
    regpoints.byidx[idx] = { condition = condition, pause_on_hit = pause_on_hit and true or false }
    ack_packet(socket, "RP:" .. tostring(idx))
  end

  local function clear_rp_idx(idx)
    idx = tonumber(idx or "")
    if not idx or not regpoints.byidx[idx] then
      ack_packet(socket, "E00")
      return
    end
    run_debugger_command("rpclear " .. tostring(idx))
    regpoints.byidx[idx] = nil
    ack_packet(socket, "OK")
  end

  local function save_item_supported(size)
    return size == 1 or size == 2 or size == 4 or size == 8
  end

  local function save_items_to_dir(path)
    local manifest_path = path .. "/manifest.txt"
    local manifest, manifest_err = io.open(manifest_path, "wb")
    if not manifest then
      return nil, "manifest open failed: " .. tostring(manifest_err)
    end

    local saved = 0
    local skipped = 0
    local idx = 0
    while true do
      local item = emu.item(idx)
      if not item or item.size == 0 or item.count == 0 then
        break
      end

      local filename = string.format("item_%06d.bin", idx)
      local bytes_len = item.size * item.count
      local ok, data_or_err = pcall(function() return item:read_block(0, bytes_len) end)
      if not ok then
        manifest:close()
        return nil, "item read failed at " .. tostring(idx) .. ": " .. tostring(data_or_err)
      end
      local f, file_err = io.open(path .. "/" .. filename, "wb")
      if not f then
        manifest:close()
        return nil, "item file open failed: " .. tostring(file_err)
      end
      f:write(data_or_err)
      f:close()
      manifest:write(string.format("%d|%d|%d|%d|%s\n", idx, item.size, item.count, bytes_len, filename))
      saved = saved + 1
      if not save_item_supported(item.size) then
        skipped = skipped + 1
      end
      idx = idx + 1
    end

    manifest:close()
    return saved, skipped
  end

  local function value_from_bytes(data, pos, size)
    local value = 0
    for offset = 0, size - 1 do
      value = value | ((data:byte(pos + offset) or 0) << (offset * 8))
    end
    return value
  end

  local function load_items_from_dir(path)
    local manifest, manifest_err = io.open(path .. "/manifest.txt", "rb")
    if not manifest then
      return nil, "manifest open failed: " .. tostring(manifest_err)
    end

    local restored = 0
    local skipped = 0
    for line in manifest:lines() do
      local idx_s, size_s, count_s, bytes_s, filename = line:match("^(%d+)|(%d+)|(%d+)|(%d+)|([^|]+)$")
      local idx = tonumber(idx_s or "")
      local size = tonumber(size_s or "")
      local count = tonumber(count_s or "")
      local bytes_len = tonumber(bytes_s or "")
      if not idx or not size or not count or not bytes_len or not filename then
        manifest:close()
        return nil, "bad manifest line: " .. tostring(line)
      end

      local item = emu.item(idx)
      if not item or item.size ~= size or item.count ~= count then
        manifest:close()
        return nil, "save item mismatch at " .. tostring(idx)
      end

      local f, file_err = io.open(path .. "/" .. filename, "rb")
      if not f then
        manifest:close()
        return nil, "item file open failed: " .. tostring(file_err)
      end
      local data = f:read("*a")
      f:close()
      if #data ~= bytes_len then
        manifest:close()
        return nil, "item data length mismatch at " .. tostring(idx)
      end

      if save_item_supported(size) then
        for entry = 0, count - 1 do
          item:write(entry, value_from_bytes(data, (entry * size) + 1, size))
        end
        restored = restored + 1
      else
        skipped = skipped + 1
      end
    end

    manifest:close()
    return restored, skipped
  end

  local function handle_emucap(payload)
    local name, rest = payload:match("^qEmucap,([^,]*),?(.*)$")
    if not name then
      return false
    end

    if name == "frame" then
      ack_packet(socket, tostring(current_frame()))
      return true
    elseif name == "snapshot" then
      local path = hex_to_string(rest or "")
      if not path or path == "" then
        ack_packet(socket, "E00")
        return true
      end
      local screen = manager.machine.screens:at(1)
      if not screen then
        ack_packet(socket, "E01")
        return true
      end
      local ok, err = pcall(function() return screen:snapshot(path) end)
      if ok and err == nil then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: snapshot failed " .. tostring(err))
        ack_packet(socket, "E02")
      end
      return true
    elseif name == "save" then
      local path = hex_to_string(rest or "")
      if not path or path == "" then
        ack_packet(socket, "E00")
        return true
      end
      local ok, err = pcall(function() manager.machine:save(path) end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: save failed " .. tostring(err))
        ack_packet(socket, "E03")
      end
      return true
    elseif name == "statesave" then
      local slot = hex_to_string(rest or "")
      if not slot or slot == "" then
        ack_packet(socket, "E00")
        return true
      end
      local ok, err = pcall(function() run_debugger_command("statesave " .. slot) end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: statesave failed " .. tostring(err))
        ack_packet(socket, "E06")
      end
      return true
    elseif name == "stateload" then
      local slot = hex_to_string(rest or "")
      if not slot or slot == "" then
        ack_packet(socket, "E00")
        return true
      end
      local ok, err = pcall(function() run_debugger_command("stateload " .. slot) end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: stateload failed " .. tostring(err))
        ack_packet(socket, "E07")
      end
      return true
    elseif name == "load" then
      local path = hex_to_string(rest or "")
      if not path or path == "" then
        ack_packet(socket, "E00")
        return true
      end
      local ok, err = pcall(function() manager.machine:load(path) end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: load failed " .. tostring(err))
        ack_packet(socket, "E04")
      end
      return true
    elseif name == "saveitems" then
      local path = hex_to_string(rest or "")
      if not path or path == "" then
        ack_packet(socket, "E00")
        return true
      end
      local ok, saved_or_err, skipped_or_err = pcall(function() return save_items_to_dir(path) end)
      if ok and saved_or_err then
        ack_packet(socket, "OK|" .. tostring(saved_or_err) .. "|" .. tostring(skipped_or_err or 0))
      else
        print("emucap_gdbstub: saveitems failed " .. tostring(ok and skipped_or_err or saved_or_err))
        ack_packet(socket, "E15")
      end
      return true
    elseif name == "loaditems" then
      local path = hex_to_string(rest or "")
      if not path or path == "" then
        ack_packet(socket, "E00")
        return true
      end
      local ok, restored_or_err, skipped_or_err = pcall(function() return load_items_from_dir(path) end)
      if ok and restored_or_err then
        ack_packet(socket, "OK|" .. tostring(restored_or_err) .. "|" .. tostring(skipped_or_err or 0))
      else
        print("emucap_gdbstub: loaditems failed " .. tostring(ok and skipped_or_err or restored_or_err))
        ack_packet(socket, "E16")
      end
      return true
    elseif name == "regload" then
      local regs_hex = hex_to_string(rest or "")
      if not regs_hex or regs_hex == "" then
        ack_packet(socket, "E00")
        return true
      end
      local ok, regs_or_err = pcall(function() return apply_regs_hex(regs_hex) end)
      if ok then
        ack_packet(socket, "OK|" .. regs_or_err)
        service_frozen_socket()
      else
        print("emucap_gdbstub: regload failed " .. tostring(regs_or_err))
        ack_packet(socket, "E14")
      end
      return true
    elseif name == "regprobe" then
      local spec = hex_to_string(rest or "")
      if not spec then
        ack_packet(socket, "E00")
        return true
      end
      local regs_hex, frames, addr_hex, len_hex = spec:match("^([0-9a-fA-F]+)|(%d+)|([0-9a-fA-F]+)|([0-9a-fA-F]+)$")
      local frame_count = tonumber(frames or "")
      local addr = tonumber(addr_hex or "", 16)
      local len = tonumber(len_hex or "", 16)
      if not regs_hex or not frame_count or not addr or not len or len < 0 then
        ack_packet(socket, "E00")
        return true
      end
      local ok, regs_or_err = pcall(function() return apply_regs_hex(regs_hex) end)
      if not ok then
        print("emucap_gdbstub: regprobe register load failed " .. tostring(regs_or_err))
        ack_packet(socket, "E14")
        return true
      end
      if frame_count == 0 then
        local map = regmaps[cpu.shortname]
        ack_packet(socket, "HEX:" .. read_program_hex(addr, len) .. "|FRAME:" .. tostring(current_frame()) .. "|REGS:" .. regs_payload(map))
      else
        frame_wait_probe = { addr = addr, len = len }
        if not start_frame_wait(frame_count, true) then
          frame_wait_probe = nil
          ack_packet(socket, "E09")
        end
      end
      return true
    elseif name == "inputstatus" then
      if next(active_input_fields) == nil then
        ack_packet(socket, "0")
      elseif release_input_frame then
        ack_packet(socket, tostring(math.max(release_input_frame - current_frame(), 0)))
      else
        ack_packet(socket, "-1")
      end
      return true
    elseif name == "setinput" then
      local buttons = hex_to_string(rest or "")
      if not buttons then
        ack_packet(socket, "E00")
        return true
      end
      local ok, unresolved = set_inputs(split_csv(buttons))
      if ok then
        ack_packet(socket, "OK")
      else
        ack_packet(socket, "E08:" .. tostring(unresolved or ""))
      end
      return true
    elseif name == "press" then
      local spec = hex_to_string(rest or "")
      if not spec then
        ack_packet(socket, "E00")
        return true
      end
      local frames, buttons = spec:match("^(%d+):(.*)$")
      frames = tonumber(frames or "")
      if not frames or frames < 1 or not buttons then
        ack_packet(socket, "E00")
        return true
      end
      local ok, unresolved = set_inputs(split_csv(buttons))
      if ok then
        release_input_frame = current_frame() + frames
        if start_frame_wait(frames, false, true) then
          -- Reply is deferred until every requested frame elapsed and clear_frame_wait released input.
        else
          clear_inputs()
          ack_packet(socket, "E09")
        end
      else
        ack_packet(socket, "E08:" .. tostring(unresolved or ""))
      end
      return true
    elseif name == "framestep" or name == "runframes" then
      local frames = tonumber(hex_to_string(rest or "") or "")
      if not frames or frames < 1 then
        ack_packet(socket, "E00")
        return true
      end
      if start_frame_wait(frames, name == "framestep") then
        -- Reply is deferred until the frame notifier reaches the target.
      else
        ack_packet(socket, "E09")
      end
      return true
    elseif name == "stop" then
      stop_debugger()
      ack_packet(socket, "OK")
      service_frozen_socket()
      return true
    elseif name == "dasm" then
      local spec = hex_to_string(rest or "")
      if not spec then
        ack_packet(socket, "E00")
        return true
      end
      local path, addr, len = spec:match("^([^|]+)|([0-9a-fA-F]+)|([0-9a-fA-F]+)$")
      addr = tonumber(addr or "", 16)
      len = tonumber(len or "", 16)
      if not path or not addr or not len or len < 1 then
        ack_packet(socket, "E00")
        return true
      end
      local ok, err = pcall(function()
        run_debugger_command(string.format("dasm %s,%X,%X,1", path, addr, len))
      end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: dasm failed " .. tostring(err))
        ack_packet(socket, "E0C")
      end
      return true
    elseif name == "tracestart" then
      local path = hex_to_string(rest or "")
      if not path or path == "" then
        ack_packet(socket, "E00")
        return true
      end
      local ok, err = pcall(function() run_debugger_command("trace " .. path .. ",,noloop") end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: trace start failed " .. tostring(err))
        ack_packet(socket, "E0E")
      end
      return true
    elseif name == "tracestop" then
      local ok, err = pcall(function() run_debugger_command("trace off") end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: trace stop failed " .. tostring(err))
        ack_packet(socket, "E0F")
      end
      return true
    elseif name == "traceflush" then
      local ok, err = pcall(function() run_debugger_command("traceflush") end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: trace flush failed " .. tostring(err))
        ack_packet(socket, "E10")
      end
      return true
    elseif name == "setpoint" then
      local spec = hex_to_string(rest or "")
      if not spec then
        ack_packet(socket, "E00")
        return true
      end
      -- 새 형식: btype|addr|len|pause|condition (condition은 마지막이라 '|' 포함 가능).
      local btype, addr_hex, len_hex, pause_s, condition =
        spec:match("^([^|]+)|([^|]+)|([^|]+)|([^|]+)|(.*)$")
      if not btype then
        -- 하위호환: pause 필드 없는 옛 형식(btype|addr|len|condition)
        btype, addr_hex, len_hex, condition = spec:match("^([^|]+)|([^|]+)|([^|]+)|(.*)$")
        pause_s = "1"
      end
      local addr = tonumber(addr_hex or "", 16)
      local len = tonumber(len_hex or "", 16)
      local pause_on_hit = pause_s ~= "0"
      if not btype or not addr or not len then
        ack_packet(socket, "E00")
        return true
      end
      if btype == "0" or btype == "1" then
        set_bp(addr, condition or "", true, pause_on_hit)
      elseif btype == "2" or btype == "3" or btype == "4" then
        set_wp(btype, addr, len, condition or "", true, pause_on_hit)
      else
        ack_packet(socket, "E00")
      end
      return true
    elseif name == "clearpoint" then
      local spec = hex_to_string(rest or "")
      if not spec then
        ack_packet(socket, "E00")
        return true
      end
      local kind, idx = spec:match("^([^|]+)|(%d+)$")
      if kind == "bp" then
        clear_bp_idx(idx)
      elseif kind == "wp" then
        clear_wp_idx(idx)
      elseif kind == "rp" then
        clear_rp_idx(idx)
      else
        ack_packet(socket, "E00")
      end
      return true
    elseif name == "setregpoint" then
      local raw = hex_to_string(rest or "")
      if not raw then
        ack_packet(socket, "E00")
        return true
      end
      -- 새 형식: pause|condition (pause는 0/1; condition은 '||' 포함 가능이라 prefix로 둔다).
      -- 옛 형식: condition만(pause 기본 freeze).
      local pause_s, condition = raw:match("^([01])|(.*)$")
      if not pause_s then
        pause_s = "1"
        condition = raw
      end
      set_rp(condition, pause_s ~= "0")
      return true
    elseif name == "reset" then
      local ok, err = pcall(function() manager.machine:soft_reset() end)
      if ok then
        ack_packet(socket, "OK")
      else
        print("emucap_gdbstub: reset failed " .. tostring(err))
        ack_packet(socket, "E05")
      end
      return true
    elseif name == "breakonreset" then
      local enabled = hex_to_string(rest or "")
      break_on_reset_enabled = enabled == "1" or enabled == "true"
      ack_packet(socket, "OK")
      return true
    elseif name == "pollreset" then
      if pending_reset then
        ack_packet(socket, "RESET:" .. pending_reset)
        pending_reset = nil
      else
        ack_packet(socket, "NONE")
      end
      return true
    elseif name == "inputfields" then
      -- 이 머신이 런타임에 실제 등록한 키보드 ioport 필드 이름을 정렬해 돌려준다. 브리지는
      -- 이를 status.input_buttons.available로 노출하고 미가용 버튼 에러에 가용 목록을 붙인다.
      local keys = {}
      for k, _ in pairs(input_fields) do
        keys[#keys + 1] = k
      end
      table.sort(keys)
      ack_packet(socket, table.concat(keys, ","))
      return true
    end

    ack_packet(socket, "")
    return true
  end

  handle = function(payload)
    if payload == "\x03" then
      debugger.execution_state = "stop"
      running = false
      hold_requested = true  -- explicit pause 의도 → note_stop이 홀드한다(내부 스톱과 구분)
      packet(socket, "S05")
      return
    end

    local cmd = payload:sub(1, 1)
    local map = regmaps[cpu.shortname]

    if handle_emucap(payload) then
      return
    elseif cmd == "?" then
      ack_packet(socket, "S05")
    elseif cmd == "g" then
      ack_packet(socket, regs_payload(map))
    elseif cmd == "G" then
      local count = 0
      payload:sub(2):gsub(string.rep("%x", map.regsize * 2), function(s)
        count = count + 1
        cpu.state[map.fromgdb[count]].value = fromle(s)
      end)
      if debugger then
        debugger.execution_state = "stop"
      end
      running = false
      ack_packet(socket, "OK")
    elseif cmd == "m" then
      local addr, len = payload:match("m(%x+),(%x+)")
      if addr and len then
        addr = tonumber(addr, 16)
        len = tonumber(len, 16)
        ack_packet(socket, read_program_hex(addr, len))
      else
        ack_packet(socket, "E00")
      end
    elseif cmd == "M" then
      local count = 0
      local addr, len, data = payload:match("M(%x+),(%x+):(%x+)")
      if addr and len and data then
        addr = tonumber(addr, 16)
        local space = cpu.spaces["program"]
        data:gsub("%x%x", function(s)
          space:writev_u8(addr + count, tonumber(s, 16))
          count = count + 1
        end)
        ack_packet(socket, "OK")
      else
        ack_packet(socket, "E00")
      end
    elseif cmd == "s" then
      if #payload == 1 then
        socket:write("+")
        -- emu.pause() 홀드 중이면 스케줄러가 멈춰 cpu.debug:step()이 1명령을 못 돈다(step 무동작인데
        -- S05 응답해 bridge가 completed로 오인). running=true로 스핀을 탈출(→emu.unpause)시킨 뒤 debugger step을 걸면,
        -- 머신이 1명령 진행하고 debugger가 재정지 → note_stop이 is_new_stop(running=true)로 S05 응답 + 다시 emu.pause로
        -- 홀드한다. 응답을 여기서 보내지 않는다(note_stop이 step 완료 후 정확히 1회 보냄 = 사후관측 가능).
        hold_requested = true  -- step 완료(temp BP 스톱) 후 홀드 의도 → note_stop이 홀드(부팅 temp BP와 구분)
        cpu.debug:step()
        running = true
      else
        ack_packet(socket, "E00")
      end
    elseif cmd == "c" then
      if #payload == 1 then
        socket:write("+")
        hold_requested = false  -- 재개 — 이후 내부/자동 스톱은 홀드 말고 넘긴다(user BP만 note_breakpoint가 홀드)
        cpu.debug:go()
        running = true
      else
        ack_packet(socket, "E00")
      end
    elseif cmd == "Z" then
      local btype, addr, len = payload:match("Z([0-4]),(%x+),(%x+)")
      addr = tonumber(addr or "", 16)
      if addr and (btype == "0" or btype == "1") then
        set_bp(addr, "", false, true)
      elseif addr and (btype == "2" or btype == "3" or btype == "4") then
        set_wp(btype, addr, tonumber(len or "1", 16), "", false, true)
      else
        ack_packet(socket, "E00")
      end
    elseif cmd == "z" then
      local btype, addr, len = payload:match("z([0-4]),(%x+),(%x+)")
      addr = tonumber(addr or "", 16)
      if addr and (btype == "0" or btype == "1") then
        clear_bp(addr)
      elseif addr and (btype == "2" or btype == "3" or btype == "4") then
        clear_wp(btype, addr, tonumber(len or "1", 16))
      else
        ack_packet(socket, "E00")
      end
    else
      ack_packet(socket, "")
    end
  end

  emu.register_periodic(function()
    if not cpu or not debugger then
      return
    end
    check_input_release()
    if trace_enabled then
      local st = debugger.execution_state
      if st ~= trace_last_state then
        trace("tick execstate=" .. tostring(st) .. " running=" .. tostring(running) .. " fwt=" .. tostring(frame_wait_target))
        trace_last_state = st
      end
    end
    if note_breakpoint() or note_stop() then
      return
    end
    if debugger.execution_state == "run" then
      running = true
    end
    read_socket()
    while true do
      local payload = next_packet()
      if not payload then
        break
      end
      handle(payload)
    end
  end)
end

return exports
