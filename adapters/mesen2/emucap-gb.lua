-- Game Boy / Game Boy Color(Mesen) 엔트리. Mesen은 GB/GBC를 하나의 gameboy 콘솔(cpuType "gameboy",
-- SM83 CPU)로 다룬다(GG가 sms 콘솔로 SMS/GG를 다루듯). 시스템별 SYS config 설정 후 제네릭 코어를 require.
-- memType·레지스터 키·CPU는 실측 확정(gameboy/gba probe): cpu_type=gameboy, F=cpu.flags, 버스=gameboyMemory,
-- RAM=gbWorkRam/gbVideoRam/gbSpriteRam/gbHighRam/gbCartRam/gbPrgRom. 콘솔 밖 read는 zero-fill(wrap 아님, 실측).
SYS = {
  system = "gb",
  system_label = "Game Boy",
  cpu_type = "gameboy",              -- emu.cpuType.gameboy (GB/GBC 공통, SM83)
  default_memtype = "gameboyMemory", -- emu.memType.gameboyMemory (SM83 CPU 버스 $0000-$FFFF)
  -- Mesen GameboyController setInput 키(소문자): a/b/start/select + 방향키.
  buttons = {
    a = true, b = true, start = true, select = true,
    up = true, down = true, left = true, right = true,
  },
  aliases = {
    enter = "start", ["return"] = "start",
  },
  -- SM83은 SNES식 리셋벡터 포인터 테이블이 없다(리셋 시 부트ROM→$0100로 직행, $0100은 포인터가 아니라 코드).
  -- break_on_reset의 read16-포인터 모델이 안 맞아 미구현(TODO — 고칠 것)(GG와 동일 판단).
  reset_vector = nil,
  bank_mirror = false,   -- SM83엔 SNES식 $00/$80 뱅크 미러 없음
  dma_supported = false, -- SNES식 MDMAEN DMA 컨트롤러 없음 → dma kind BP는 미구현(TODO — 고칠 것)(GB OAM DMA는 별개)
  -- read/write BP 주소 변환 맵: RAM offset을 SM83 버스 base로 변환. 안 하면 addMemoryCallback이 버스
  -- $00xx(ROM)에 걸려 미발동한다. 고정 매핑만 등록(work RAM $C000, HRAM $FF80) — offset < $2000 유효
  -- (CGB WRAM 뱅크2~7·switchable은 고정 버스주소 없음). 이미 버스인 gameboyMemory는 맵에 없어 그대로.
  bp_bus_base = { gbWorkRam = 0xC000, gbHighRam = 0xFF80 },
  -- 덤프 리전(emucap diff 입력). base는 버스주소(참조용), 실제 read는 memType offset 0부터. CGB-최대 크기로
  -- 잡는다(DMG는 남는 뱅크가 zero-fill로 안전, 실측). wram=CGB 32KB, vram=CGB 16KB, oam=160B, hram=127B.
  dump_regions = {
    { name = "wram", mt = "gbWorkRam",   base = 0xC000, size = 0x8000 },
    { name = "vram", mt = "gbVideoRam",  base = 0x8000, size = 0x4000 },
    { name = "oam",  mt = "gbSpriteRam", base = 0xFE00, size = 0xA0 },
    { name = "hram", mt = "gbHighRam",   base = 0xFF80, size = 0x7F },
  },
  region_sizes = {
    gbWorkRam = 0x8000, gbVideoRam = 0x4000, gbSpriteRam = 0xA0,
    gbHighRam = 0x7F, gbCartRam = 0x8000, gbPrgRom = 0x800000,
  },
}

-- ── SM83 디스어셈블러 + 콜/리턴 분류 (Game Boy 전용 ISA) ──
-- Mesen2 Lua엔 디스어셈블 API가 없어 SM83 디코더를 직접 구현한다. 코어가 gameboyMemory 버스에서 읽는
-- read_byte 클로저를 넘긴다. ⚠ SM83은 Z80이 아니다: IX/IY·DD/FD/ED 프리픽스 없음, CB만 존재.
-- GB 고유: $08=LD(a16),SP $10=STOP $22/$2A=LD(HL±)A $32/$3A LD A/(HL-) $E0/$F0=LDH $E2/$F2=LD(C),A
-- $E8=ADD SP,r8 $EA/$FA=LD (a16)/A $F8=LD HL,SP+r8 $D9=RETI. CB그룹 idx6=SWAP(Z80의 SLL 아님).

-- 호출: CALL nn($CD)·조건부 CALL($C4/CC/D4/DC)·RST($C7..$FF). 리턴: RET($C9)·조건부 RET($C0/C8/D0/D8)·RETI($D9).
SYS.op_is_call = function(op)
  return op == 0xCD
    or op == 0xC4 or op == 0xCC or op == 0xD4 or op == 0xDC
    or op == 0xC7 or op == 0xCF or op == 0xD7 or op == 0xDF
    or op == 0xE7 or op == 0xEF or op == 0xF7 or op == 0xFF
end
SYS.op_is_return = function(op)
  return op == 0xC9 or op == 0xD9
    or op == 0xC0 or op == 0xC8 or op == 0xD0 or op == 0xD8
end

-- record_hit이 히트 순간 잡는 레지스터(SM83=a/f/b/c/d/e/h/l/sp). pc는 코어가 full_pc(=cpu.pc)로 따로 싣는다.
-- F 레지스터 키는 cpu.flags(실측 — cpu.f 아님).
SYS.snapshot_regs = function(st)
  return {
    a = st["cpu.a"], f = st["cpu.flags"],
    b = st["cpu.b"], c = st["cpu.c"],
    d = st["cpu.d"], e = st["cpu.e"],
    h = st["cpu.h"], l = st["cpu.l"],
    sp = st["cpu.sp"],
  }
end

-- GB VDP(포트 $FF40~)는 SNES식 데이터-포트→VRAM 워드주소 매핑을 런타임 상태로 노출하지 않는다 → nil(누수 방지).
SYS.port_semantics = nil

local function s8(v) return (v >= 0x80) and (v - 0x100) or v end

-- 8비트 레지스터 순서(opcode 3비트 필드). 6=(HL) 메모리 간접.
local SM83R = { [0] = "B", "C", "D", "E", "H", "L", "(HL)", "A" }
-- CB x=0 롤/시프트 그룹. idx6=SWAP(SM83 고유 — Z80의 SLL 자리).
local SM83CB = { [0] = "RLC", "RRC", "RL", "RR", "SLA", "SRA", "SWAP", "SRL" }

-- base opcode → {fmt, t}. t: 0=오퍼랜드 없음, 1=즉시 바이트(a8/d8, %s에 $XX), 2=즉시 워드(d16/a16, $XXXX),
-- 3=상대(JR, 타깃=다음pc+s8), 4=부호 즉시 바이트(ADD SP,r8 / LD HL,SP+r8; %s에 +$XX/-$XX). $CB는 별도.
local SM83BASE = {}
do
  -- 0x40-0x7F: LD r,r' (0x76=HALT)
  for op = 0x40, 0x7F do
    if op == 0x76 then SM83BASE[op] = { "HALT", 0 }
    else
      local dst = SM83R[math.floor((op - 0x40) / 8)]
      local src = SM83R[(op - 0x40) % 8]
      SM83BASE[op] = { "LD " .. dst .. "," .. src, 0 }
    end
  end
  -- 0x80-0xBF: A와 r 산술/논리
  local alu = { [0] = "ADD A,", "ADC A,", "SUB ", "SBC A,", "AND ", "XOR ", "OR ", "CP " }
  for op = 0x80, 0xBF do
    local g = math.floor((op - 0x80) / 8)
    local r = SM83R[(op - 0x80) % 8]
    SM83BASE[op] = { alu[g] .. r, 0 }
  end
end
-- 0x00-0x3F
SM83BASE[0x00] = { "NOP", 0 };            SM83BASE[0x01] = { "LD BC,%s", 2 }
SM83BASE[0x02] = { "LD (BC),A", 0 };      SM83BASE[0x03] = { "INC BC", 0 }
SM83BASE[0x04] = { "INC B", 0 };          SM83BASE[0x05] = { "DEC B", 0 }
SM83BASE[0x06] = { "LD B,%s", 1 };        SM83BASE[0x07] = { "RLCA", 0 }
SM83BASE[0x08] = { "LD (%s),SP", 2 };     SM83BASE[0x09] = { "ADD HL,BC", 0 }
SM83BASE[0x0A] = { "LD A,(BC)", 0 };      SM83BASE[0x0B] = { "DEC BC", 0 }
SM83BASE[0x0C] = { "INC C", 0 };          SM83BASE[0x0D] = { "DEC C", 0 }
SM83BASE[0x0E] = { "LD C,%s", 1 };        SM83BASE[0x0F] = { "RRCA", 0 }
SM83BASE[0x10] = { "STOP", 1 };           SM83BASE[0x11] = { "LD DE,%s", 2 }
SM83BASE[0x12] = { "LD (DE),A", 0 };      SM83BASE[0x13] = { "INC DE", 0 }
SM83BASE[0x14] = { "INC D", 0 };          SM83BASE[0x15] = { "DEC D", 0 }
SM83BASE[0x16] = { "LD D,%s", 1 };        SM83BASE[0x17] = { "RLA", 0 }
SM83BASE[0x18] = { "JR %s", 3 };          SM83BASE[0x19] = { "ADD HL,DE", 0 }
SM83BASE[0x1A] = { "LD A,(DE)", 0 };      SM83BASE[0x1B] = { "DEC DE", 0 }
SM83BASE[0x1C] = { "INC E", 0 };          SM83BASE[0x1D] = { "DEC E", 0 }
SM83BASE[0x1E] = { "LD E,%s", 1 };        SM83BASE[0x1F] = { "RRA", 0 }
SM83BASE[0x20] = { "JR NZ,%s", 3 };       SM83BASE[0x21] = { "LD HL,%s", 2 }
SM83BASE[0x22] = { "LD (HL+),A", 0 };     SM83BASE[0x23] = { "INC HL", 0 }
SM83BASE[0x24] = { "INC H", 0 };          SM83BASE[0x25] = { "DEC H", 0 }
SM83BASE[0x26] = { "LD H,%s", 1 };        SM83BASE[0x27] = { "DAA", 0 }
SM83BASE[0x28] = { "JR Z,%s", 3 };        SM83BASE[0x29] = { "ADD HL,HL", 0 }
SM83BASE[0x2A] = { "LD A,(HL+)", 0 };     SM83BASE[0x2B] = { "DEC HL", 0 }
SM83BASE[0x2C] = { "INC L", 0 };          SM83BASE[0x2D] = { "DEC L", 0 }
SM83BASE[0x2E] = { "LD L,%s", 1 };        SM83BASE[0x2F] = { "CPL", 0 }
SM83BASE[0x30] = { "JR NC,%s", 3 };       SM83BASE[0x31] = { "LD SP,%s", 2 }
SM83BASE[0x32] = { "LD (HL-),A", 0 };     SM83BASE[0x33] = { "INC SP", 0 }
SM83BASE[0x34] = { "INC (HL)", 0 };       SM83BASE[0x35] = { "DEC (HL)", 0 }
SM83BASE[0x36] = { "LD (HL),%s", 1 };     SM83BASE[0x37] = { "SCF", 0 }
SM83BASE[0x38] = { "JR C,%s", 3 };        SM83BASE[0x39] = { "ADD HL,SP", 0 }
SM83BASE[0x3A] = { "LD A,(HL-)", 0 };     SM83BASE[0x3B] = { "DEC SP", 0 }
SM83BASE[0x3C] = { "INC A", 0 };          SM83BASE[0x3D] = { "DEC A", 0 }
SM83BASE[0x3E] = { "LD A,%s", 1 };        SM83BASE[0x3F] = { "CCF", 0 }
-- 0xC0-0xFF (프리픽스 0xCB 제외; 무효 opcode는 .DB)
SM83BASE[0xC0] = { "RET NZ", 0 };         SM83BASE[0xC1] = { "POP BC", 0 }
SM83BASE[0xC2] = { "JP NZ,%s", 2 };       SM83BASE[0xC3] = { "JP %s", 2 }
SM83BASE[0xC4] = { "CALL NZ,%s", 2 };     SM83BASE[0xC5] = { "PUSH BC", 0 }
SM83BASE[0xC6] = { "ADD A,%s", 1 };       SM83BASE[0xC7] = { "RST 00H", 0 }
SM83BASE[0xC8] = { "RET Z", 0 };          SM83BASE[0xC9] = { "RET", 0 }
SM83BASE[0xCA] = { "JP Z,%s", 2 };        SM83BASE[0xCC] = { "CALL Z,%s", 2 }
SM83BASE[0xCD] = { "CALL %s", 2 };        SM83BASE[0xCE] = { "ADC A,%s", 1 }
SM83BASE[0xCF] = { "RST 08H", 0 }
SM83BASE[0xD0] = { "RET NC", 0 };         SM83BASE[0xD1] = { "POP DE", 0 }
SM83BASE[0xD2] = { "JP NC,%s", 2 };       SM83BASE[0xD4] = { "CALL NC,%s", 2 }
SM83BASE[0xD5] = { "PUSH DE", 0 };        SM83BASE[0xD6] = { "SUB %s", 1 }
SM83BASE[0xD7] = { "RST 10H", 0 };        SM83BASE[0xD8] = { "RET C", 0 }
SM83BASE[0xD9] = { "RETI", 0 };           SM83BASE[0xDA] = { "JP C,%s", 2 }
SM83BASE[0xDC] = { "CALL C,%s", 2 };      SM83BASE[0xDE] = { "SBC A,%s", 1 }
SM83BASE[0xDF] = { "RST 18H", 0 }
SM83BASE[0xE0] = { "LDH (%s),A", 1 };     SM83BASE[0xE1] = { "POP HL", 0 }
SM83BASE[0xE2] = { "LD (C),A", 0 };       SM83BASE[0xE5] = { "PUSH HL", 0 }
SM83BASE[0xE6] = { "AND %s", 1 };         SM83BASE[0xE7] = { "RST 20H", 0 }
SM83BASE[0xE8] = { "ADD SP,%s", 4 };      SM83BASE[0xE9] = { "JP HL", 0 }
SM83BASE[0xEA] = { "LD (%s),A", 2 };      SM83BASE[0xEE] = { "XOR %s", 1 }
SM83BASE[0xEF] = { "RST 28H", 0 }
SM83BASE[0xF0] = { "LDH A,(%s)", 1 };     SM83BASE[0xF1] = { "POP AF", 0 }
SM83BASE[0xF2] = { "LD A,(C)", 0 };       SM83BASE[0xF3] = { "DI", 0 }
SM83BASE[0xF5] = { "PUSH AF", 0 };        SM83BASE[0xF6] = { "OR %s", 1 }
SM83BASE[0xF7] = { "RST 30H", 0 };        SM83BASE[0xF8] = { "LD HL,SP%s", 4 }
SM83BASE[0xF9] = { "LD SP,HL", 0 };       SM83BASE[0xFA] = { "LD A,(%s)", 2 }
SM83BASE[0xFB] = { "EI", 0 };             SM83BASE[0xFE] = { "CP %s", 1 }
SM83BASE[0xFF] = { "RST 38H", 0 }

-- t형 오퍼랜드를 fmt에 채워 최종 텍스트·소비 바이트 수를 낸다. base0=오퍼랜드 시작 오프셋(op 뒤=1).
local function sm83_operand(fmt, t, read_byte, pc, base0)
  if t == 0 then return fmt, base0 end
  if t == 1 then                       -- 즉시 바이트(a8/d8)
    local n = read_byte(pc + base0)
    return string.format(fmt, string.format("$%02X", n)), base0 + 1
  elseif t == 2 then                   -- 즉시 워드(d16/a16, little-endian)
    local lo = read_byte(pc + base0)
    local hi = read_byte(pc + base0 + 1)
    return string.format(fmt, string.format("$%04X", lo + hi * 256)), base0 + 2
  elseif t == 3 then                   -- 상대(JR): 타깃 = 다음 명령 주소 + s8
    local d = read_byte(pc + base0)
    local target = (pc + base0 + 1 + s8(d)) % 0x10000
    return string.format(fmt, string.format("$%04X", target)), base0 + 1
  else                                 -- t == 4: 부호 즉시 바이트(+$XX/-$XX)
    local sd = s8(read_byte(pc + base0))
    local disp = (sd < 0) and string.format("-$%02X", -sd) or string.format("+$%02X", sd)
    return string.format(fmt, disp), base0 + 1
  end
end

-- CB 서브opcode → 니모닉. x=0 롤/시프트(SWAP 포함), x=1 BIT, x=2 RES, x=3 SET.
local function sm83_cb_text(sub)
  local reg = SM83R[sub % 8]
  local x = math.floor(sub / 0x40)
  local y = math.floor(sub / 8) % 8
  if x == 0 then return SM83CB[y] .. " " .. reg
  elseif x == 1 then return "BIT " .. y .. "," .. reg
  elseif x == 2 then return "RES " .. y .. "," .. reg
  else return "SET " .. y .. "," .. reg end
end

-- 한 명령 디코드 → text, len(바이트 수). read_byte(addr)로만 읽는다.
local function sm83_decode(read_byte, pc)
  local op = read_byte(pc)
  if op == 0xCB then
    return sm83_cb_text(read_byte(pc + 1)), 2
  end
  local e = SM83BASE[op]
  if not e then return string.format(".DB $%02X", op), 1 end
  return sm83_operand(e[1], e[2], read_byte, pc, 1)
end

-- disassemble(read_byte, start, count): start에서 count개 SM83 명령. 반환 [{addr,text,bytes}].
SYS.disassemble = function(read_byte, start, count)
  local out = {}
  local pc = start
  for _ = 1, count do
    local text, len = sm83_decode(read_byte, pc)
    local raw = {}
    for i = 0, len - 1 do raw[#raw + 1] = string.format("%02X", read_byte(pc + i)) end
    out[#out + 1] = {
      addr = string.format("0x%06X", pc),
      text = text,
      bytes = table.concat(raw, " "),
    }
    pc = pc + len
  end
  return out
end

local dir = os.getenv("EMUCAP_ADAPTER_DIR")
if not dir or dir == "" then
  -- 폴백: env가 없으면(수동 Script Window 로드 등) 이 스크립트 파일 경로에서 어댑터 디렉터리를 도출한다.
  local src = debug.getinfo(1, "S").source
  if src and src:sub(1, 1) == "@" then dir = src:sub(2):match("^(.*)[/\\][^/\\]+$") end
end
assert(dir and dir ~= "", "emucap-gb: EMUCAP_ADAPTER_DIR 미설정 + 스크립트 경로 도출 실패 — launch로 띄우거나 파일에서 로드하라")
package.path = dir .. "/?.lua;" .. package.path
require("emucap-core")
