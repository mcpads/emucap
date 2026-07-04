-- Game Gear / Master System(Mesen) 엔트리. Mesen은 SMS/GG를 하나의 sms 콘솔로 다룬다(cpuType "sms").
-- 시스템별 SYS config 설정 후 제네릭 코어(emucap-core.lua)를 require 한다.
-- 버튼 키는 실측으로 확정 중 — 우선 후보를 넓게 받아 게임 반응으로 진짜 키를 가린다.
SYS = {
  system = "gamegear",
  system_label = "Game Gear",
  cpu_type = "sms",               -- emu.cpuType.sms (SMS/GG 공통)
  default_memtype = "smsMemory",  -- emu.memType.smsMemory (Z80 버스)
  -- MesenCE SmsController의 setInput 키(소스 확정): up/down/left/right + one(버튼1=B)·two(버튼2=A)·
  -- pause(=Game Gear Start / SMS Pause 버튼).
  buttons = {
    up = true, down = true, left = true, right = true,
    one = true, two = true, pause = true,
  },
  -- 에이전트 친화 별칭 → Mesen 키. GG Start=pause, 버튼1/2=one/two(A=two, B=one).
  aliases = {
    start = "pause", enter = "pause", ["return"] = "pause",
    a = "two", b = "one",
    ["1"] = "one", ["2"] = "two", button1 = "one", button2 = "two",
  },
  reset_vector = nil,   -- Z80은 리셋 시 PC=0으로 직행(SNES식 벡터 테이블 없음) → break_on_reset 미지원
  bank_mirror = false,  -- Z80/GG는 SNES식 $00/$80 뱅크 미러 없음
  dma_supported = false,  -- Z80/GG엔 SNES식 MDMAEN DMA 컨트롤러가 없다 → dma kind BP는 미지원
  -- read/write BP 주소 변환 맵: smsWorkRam offset을 Z80 버스 base(0xC000-0xDFFF)로 변환. 안 하면
  -- addMemoryCallback이 버스 0x000B(ROM)에 걸어 영원히 미발동한다. smsMemory(이미 버스)는 맵에 없어 그대로.
  bp_bus_base = { smsWorkRam = 0xC000 },
  dump_regions = {
    { name = "wram", mt = "smsWorkRam",    base = 0, size = 0x2000 },  -- Z80 work RAM 8KB
    { name = "vram", mt = "smsVideoRam",   base = 0, size = 0x4000 },  -- VRAM 16KB
    { name = "cram", mt = "smsPaletteRam", base = 0, size = 0x40 },    -- GG 팔레트(CRAM)
  },
  region_sizes = {
    smsWorkRam = 0x2000, smsVideoRam = 0x4000, smsPaletteRam = 0x40,
    smsCartRam = 0x8000, smsSpriteRam = 0x100,
  },
}

-- ── Z80 디스어셈블러 + 콜/리턴 분류 (GG/SMS 전용 ISA) ──
-- Mesen2 Lua엔 디스어셈블 API가 없어 Z80 디코더를 직접 구현한다. 코어가 smsMemory 버스에서 읽는
-- read_byte 클로저를 넘긴다. 테이블 구동: base(0x00-0xFF) + CB/ED/DD/FD/DDCB 프리픽스.

-- 호출: CALL nn(0xCD)·조건부 CALL·RST. 리턴: RET(0xC9)·조건부 RET.
-- (RETI/RETN은 ED 프리픽스라 제외 — 코어의 SP 기반 정리가 놓친 pop을 덮는다.)
SYS.op_is_call = function(op)
  return op == 0xCD
    or op == 0xC4 or op == 0xCC or op == 0xD4 or op == 0xDC
    or op == 0xE4 or op == 0xEC or op == 0xF4 or op == 0xFC
    or op == 0xC7 or op == 0xCF or op == 0xD7 or op == 0xDF
    or op == 0xE7 or op == 0xEF or op == 0xF7 or op == 0xFF
end
SYS.op_is_return = function(op)
  return op == 0xC9
    or op == 0xC0 or op == 0xC8 or op == 0xD0 or op == 0xD8
    or op == 0xE0 or op == 0xE8 or op == 0xF0 or op == 0xF8
end

-- record_hit이 히트 순간 잡는 레지스터 세트(ISA별). GG/Z80=a/b/c/d/e/h/l/f/ix/iy/sp. pc는 코어가 full_pc로
-- 따로 실는다(Z80은 뱅크 없어 cpu.pc로 축약). Mesen SmsCpuState는 IX/IY를 상·하위 바이트(ixh/ixl·iyh/iyl)로
-- 노출하므로 조합하고(cpu.ix 직접 키가 있으면 우선), flags 키가 버전에 따라 다를 수 있어 폴백 체인을 둔다.
SYS.snapshot_regs = function(st)
  local function first(...)
    for _, k in ipairs({ ... }) do local v = st[k]; if v ~= nil then return v end end
    return nil
  end
  local ixl, ixh = first("cpu.ixl"), first("cpu.ixh")
  local iyl, iyh = first("cpu.iyl"), first("cpu.iyh")
  local ix = first("cpu.ix"); if ix == nil and ixl and ixh then ix = ixh * 256 + ixl end
  local iy = first("cpu.iy"); if iy == nil and iyl and iyh then iy = iyh * 256 + iyl end
  return {
    a = first("cpu.a"), b = first("cpu.b"), c = first("cpu.c"), d = first("cpu.d"),
    e = first("cpu.e"), h = first("cpu.h"), l = first("cpu.l"),
    f = first("cpu.flags", "cpu.f", "cpu.status"),
    ix = ix, iy = iy, sp = first("cpu.sp"),
  }
end

-- GG/SMS VDP는 SNES식 데이터-포트→VRAM 워드주소 매핑을 런타임 상태로 노출하지 않는다(포트 0xBE/0xBF).
-- write BP 대상 주소도 평범한 Z80 메모리라 SNES식 포트 라벨을 달지 않는다 → nil(하드코딩 누수 방지).
SYS.port_semantics = nil

-- SMS/GG VDP VRAM은 CPU 버스에 없다 — Z80은 VDP 데이터포트(0xBE) OUT으로만 쓰고, Mesen memory 콜백은 CPU
-- 버스 접근만 잡으므로 smsVideoRam write는 절대 발화하지 않는다(실측: 같은 구간 WRAM write 32k회 ↔ VRAM 0회).
-- 그래서 write BP를 exec 콜백에서 재구성한다(코어 setup_vram_recon_bp): 이 훅이 명령을 보고 VRAM 쓰기면
-- (워드주소, 데이터)를 준다. OUT(0xBE)/블록I/O(OTIR 등, 포트=레지스터 C=0xBE)를 opcode로 감지하고
-- vdp.addressReg(목적지 워드주소)·vdp.codeReg(1=VRAM write)를 읽는다. getState는 데이터포트 후보에서만 부른다.
SYS.non_bus_write_memtypes = {
  smsVideoRam   = "vram_recon",  -- VDP 데이터포트 write 재구성으로 지원
  smsPaletteRam = "error",       -- CRAM(codeReg=3)도 포트 write지만 재구성 미구현(TODO)
}
SYS.vram_write_target = function(pc, opcode)
  local bus = emu.memType.smsMemory
  local op = opcode or emu.read(pc, bus)
  local blockio = false
  if op == 0xD3 then                                  -- OUT (n),A
    if emu.read(pc + 1, bus) ~= 0xBE then return nil end
  elseif op == 0xED then                              -- OUTI/OTIR/OUTD/OTDR (포트=레지스터 C)
    local sub = emu.read(pc + 1, bus)
    if sub ~= 0xA3 and sub ~= 0xB3 and sub ~= 0xAB and sub ~= 0xBB then return nil end
    blockio = true
  else
    return nil
  end
  local st = emu.getState()                            -- OUT $BE / 블록I/O 후보에서만(플러드 최소화)
  if blockio and st["cpu.c"] ~= 0xBE then return nil end
  if st["vdp.codeReg"] ~= 1 then return nil end        -- code 1 = VRAM write(0=read/2=reg/3=CRAM)
  -- 쓰이는 데이터바이트: OUT ($BE),A는 A지만 블록 I/O(OUTI/OTIR/OUTD/OTDR = OUT (C),(HL))는 (HL)이다.
  -- 콜백은 실행 전이라 (HL)이 아직 소스 바이트를 가리킨다(OUTI는 이후 INC HL, OUTD는 DEC HL). value 필터·
  -- event.value가 맞으려면 여기서 정확히 구해야 한다(대량 VRAM 복사는 거의 다 블록 I/O).
  local data = blockio
    and emu.read((st["cpu.h"] or 0) * 256 + (st["cpu.l"] or 0), bus)
    or st["cpu.a"]
  return st["vdp.addressReg"], data
end

local function z80_s8(v) return (v >= 0x80) and (v - 0x100) or v end
-- (IX+d)/(IY+d) 변위: 부호 있는 +$XX/-$XX.
local function z80_disp(d)
  local sd = z80_s8(d)
  if sd < 0 then return string.format("-$%02X", -sd) else return string.format("+$%02X", sd) end
end

-- 8비트 레지스터 순서(opcode 3비트 필드). 6=(HL) 메모리 간접.
local Z80R = { [0]="B", "C", "D", "E", "H", "L", "(HL)", "A" }
-- CB x=0 롤/시프트 그룹(SLL은 미문서화지만 흔함).
local Z80CB = { [0]="RLC", "RRC", "RL", "RR", "SLA", "SRA", "SLL", "SRL" }

-- base opcode → {fmt, t}. t: 0=오퍼랜드 없음, 1=즉시 바이트(#$%02X는 fmt의 %s에), 2=즉시 워드,
-- 3=상대(JR/DJNZ, 타깃=pc+2+s8). 프리픽스 0xCB/0xDD/0xED/0xFD는 여기 없다(별도 처리).
local Z80BASE = {}
do
  -- 0x40-0x7F: LD r,r' (0x76=HALT)
  for op = 0x40, 0x7F do
    if op == 0x76 then Z80BASE[op] = { "HALT", 0 }
    else
      local dst = Z80R[math.floor((op - 0x40) / 8)]
      local src = Z80R[(op - 0x40) % 8]
      Z80BASE[op] = { "LD " .. dst .. "," .. src, 0 }
    end
  end
  -- 0x80-0xBF: A와 r 산술/논리
  local alu = { [0]="ADD A,", "ADC A,", "SUB ", "SBC A,", "AND ", "XOR ", "OR ", "CP " }
  for op = 0x80, 0xBF do
    local g = math.floor((op - 0x80) / 8)
    local r = Z80R[(op - 0x80) % 8]
    Z80BASE[op] = { alu[g] .. r, 0 }
  end
end
-- 0x00-0x3F
Z80BASE[0x00] = { "NOP", 0 };          Z80BASE[0x01] = { "LD BC,%s", 2 }
Z80BASE[0x02] = { "LD (BC),A", 0 };    Z80BASE[0x03] = { "INC BC", 0 }
Z80BASE[0x04] = { "INC B", 0 };        Z80BASE[0x05] = { "DEC B", 0 }
Z80BASE[0x06] = { "LD B,%s", 1 };      Z80BASE[0x07] = { "RLCA", 0 }
Z80BASE[0x08] = { "EX AF,AF'", 0 };    Z80BASE[0x09] = { "ADD HL,BC", 0 }
Z80BASE[0x0A] = { "LD A,(BC)", 0 };    Z80BASE[0x0B] = { "DEC BC", 0 }
Z80BASE[0x0C] = { "INC C", 0 };        Z80BASE[0x0D] = { "DEC C", 0 }
Z80BASE[0x0E] = { "LD C,%s", 1 };      Z80BASE[0x0F] = { "RRCA", 0 }
Z80BASE[0x10] = { "DJNZ %s", 3 };      Z80BASE[0x11] = { "LD DE,%s", 2 }
Z80BASE[0x12] = { "LD (DE),A", 0 };    Z80BASE[0x13] = { "INC DE", 0 }
Z80BASE[0x14] = { "INC D", 0 };        Z80BASE[0x15] = { "DEC D", 0 }
Z80BASE[0x16] = { "LD D,%s", 1 };      Z80BASE[0x17] = { "RLA", 0 }
Z80BASE[0x18] = { "JR %s", 3 };        Z80BASE[0x19] = { "ADD HL,DE", 0 }
Z80BASE[0x1A] = { "LD A,(DE)", 0 };    Z80BASE[0x1B] = { "DEC DE", 0 }
Z80BASE[0x1C] = { "INC E", 0 };        Z80BASE[0x1D] = { "DEC E", 0 }
Z80BASE[0x1E] = { "LD E,%s", 1 };      Z80BASE[0x1F] = { "RRA", 0 }
Z80BASE[0x20] = { "JR NZ,%s", 3 };     Z80BASE[0x21] = { "LD HL,%s", 2 }
Z80BASE[0x22] = { "LD (%s),HL", 2 };   Z80BASE[0x23] = { "INC HL", 0 }
Z80BASE[0x24] = { "INC H", 0 };        Z80BASE[0x25] = { "DEC H", 0 }
Z80BASE[0x26] = { "LD H,%s", 1 };      Z80BASE[0x27] = { "DAA", 0 }
Z80BASE[0x28] = { "JR Z,%s", 3 };      Z80BASE[0x29] = { "ADD HL,HL", 0 }
Z80BASE[0x2A] = { "LD HL,(%s)", 2 };   Z80BASE[0x2B] = { "DEC HL", 0 }
Z80BASE[0x2C] = { "INC L", 0 };        Z80BASE[0x2D] = { "DEC L", 0 }
Z80BASE[0x2E] = { "LD L,%s", 1 };      Z80BASE[0x2F] = { "CPL", 0 }
Z80BASE[0x30] = { "JR NC,%s", 3 };     Z80BASE[0x31] = { "LD SP,%s", 2 }
Z80BASE[0x32] = { "LD (%s),A", 2 };    Z80BASE[0x33] = { "INC SP", 0 }
Z80BASE[0x34] = { "INC (HL)", 0 };     Z80BASE[0x35] = { "DEC (HL)", 0 }
Z80BASE[0x36] = { "LD (HL),%s", 1 };   Z80BASE[0x37] = { "SCF", 0 }
Z80BASE[0x38] = { "JR C,%s", 3 };      Z80BASE[0x39] = { "ADD HL,SP", 0 }
Z80BASE[0x3A] = { "LD A,(%s)", 2 };    Z80BASE[0x3B] = { "DEC SP", 0 }
Z80BASE[0x3C] = { "INC A", 0 };        Z80BASE[0x3D] = { "DEC A", 0 }
Z80BASE[0x3E] = { "LD A,%s", 1 };      Z80BASE[0x3F] = { "CCF", 0 }
-- 0xC0-0xFF (프리픽스 0xCB/0xDD/0xED/0xFD 제외)
Z80BASE[0xC0] = { "RET NZ", 0 };       Z80BASE[0xC1] = { "POP BC", 0 }
Z80BASE[0xC2] = { "JP NZ,%s", 2 };     Z80BASE[0xC3] = { "JP %s", 2 }
Z80BASE[0xC4] = { "CALL NZ,%s", 2 };   Z80BASE[0xC5] = { "PUSH BC", 0 }
Z80BASE[0xC6] = { "ADD A,%s", 1 };     Z80BASE[0xC7] = { "RST 00H", 0 }
Z80BASE[0xC8] = { "RET Z", 0 };        Z80BASE[0xC9] = { "RET", 0 }
Z80BASE[0xCA] = { "JP Z,%s", 2 };      Z80BASE[0xCC] = { "CALL Z,%s", 2 }
Z80BASE[0xCD] = { "CALL %s", 2 };      Z80BASE[0xCE] = { "ADC A,%s", 1 }
Z80BASE[0xCF] = { "RST 08H", 0 }
Z80BASE[0xD0] = { "RET NC", 0 };       Z80BASE[0xD1] = { "POP DE", 0 }
Z80BASE[0xD2] = { "JP NC,%s", 2 };     Z80BASE[0xD3] = { "OUT (%s),A", 1 }
Z80BASE[0xD4] = { "CALL NC,%s", 2 };   Z80BASE[0xD5] = { "PUSH DE", 0 }
Z80BASE[0xD6] = { "SUB %s", 1 };       Z80BASE[0xD7] = { "RST 10H", 0 }
Z80BASE[0xD8] = { "RET C", 0 };        Z80BASE[0xD9] = { "EXX", 0 }
Z80BASE[0xDA] = { "JP C,%s", 2 };      Z80BASE[0xDB] = { "IN A,(%s)", 1 }
Z80BASE[0xDC] = { "CALL C,%s", 2 };    Z80BASE[0xDE] = { "SBC A,%s", 1 }
Z80BASE[0xDF] = { "RST 18H", 0 }
Z80BASE[0xE0] = { "RET PO", 0 };       Z80BASE[0xE1] = { "POP HL", 0 }
Z80BASE[0xE2] = { "JP PO,%s", 2 };     Z80BASE[0xE3] = { "EX (SP),HL", 0 }
Z80BASE[0xE4] = { "CALL PO,%s", 2 };   Z80BASE[0xE5] = { "PUSH HL", 0 }
Z80BASE[0xE6] = { "AND %s", 1 };       Z80BASE[0xE7] = { "RST 20H", 0 }
Z80BASE[0xE8] = { "RET PE", 0 };       Z80BASE[0xE9] = { "JP (HL)", 0 }
Z80BASE[0xEA] = { "JP PE,%s", 2 };     Z80BASE[0xEB] = { "EX DE,HL", 0 }
Z80BASE[0xEC] = { "CALL PE,%s", 2 };   Z80BASE[0xEE] = { "XOR %s", 1 }
Z80BASE[0xEF] = { "RST 28H", 0 }
Z80BASE[0xF0] = { "RET P", 0 };        Z80BASE[0xF1] = { "POP AF", 0 }
Z80BASE[0xF2] = { "JP P,%s", 2 };      Z80BASE[0xF3] = { "DI", 0 }
Z80BASE[0xF4] = { "CALL P,%s", 2 };    Z80BASE[0xF5] = { "PUSH AF", 0 }
Z80BASE[0xF6] = { "OR %s", 1 };        Z80BASE[0xF7] = { "RST 30H", 0 }
Z80BASE[0xF8] = { "RET M", 0 };        Z80BASE[0xF9] = { "LD SP,HL", 0 }
Z80BASE[0xFA] = { "JP M,%s", 2 };      Z80BASE[0xFB] = { "EI", 0 }
Z80BASE[0xFC] = { "CALL M,%s", 2 };    Z80BASE[0xFE] = { "CP %s", 1 }
Z80BASE[0xFF] = { "RST 38H", 0 }

-- ED 확장 → {fmt, t}. t: 0=없음, 2=즉시 워드. 없는 값은 .DB 처리.
local Z80ED = {
  [0x40]={"IN B,(C)",0},   [0x41]={"OUT (C),B",0}, [0x42]={"SBC HL,BC",0}, [0x43]={"LD (%s),BC",2},
  [0x44]={"NEG",0},        [0x45]={"RETN",0},      [0x46]={"IM 0",0},      [0x47]={"LD I,A",0},
  [0x48]={"IN C,(C)",0},   [0x49]={"OUT (C),C",0}, [0x4A]={"ADC HL,BC",0}, [0x4B]={"LD BC,(%s)",2},
  [0x4C]={"NEG",0},        [0x4D]={"RETI",0},      [0x4E]={"IM 0",0},      [0x4F]={"LD R,A",0},
  [0x50]={"IN D,(C)",0},   [0x51]={"OUT (C),D",0}, [0x52]={"SBC HL,DE",0}, [0x53]={"LD (%s),DE",2},
  [0x56]={"IM 1",0},       [0x57]={"LD A,I",0},
  [0x58]={"IN E,(C)",0},   [0x59]={"OUT (C),E",0}, [0x5A]={"ADC HL,DE",0}, [0x5B]={"LD DE,(%s)",2},
  [0x5E]={"IM 2",0},       [0x5F]={"LD A,R",0},
  [0x60]={"IN H,(C)",0},   [0x61]={"OUT (C),H",0}, [0x62]={"SBC HL,HL",0}, [0x63]={"LD (%s),HL",2},
  [0x67]={"RRD",0},
  [0x68]={"IN L,(C)",0},   [0x69]={"OUT (C),L",0}, [0x6A]={"ADC HL,HL",0}, [0x6B]={"LD HL,(%s)",2},
  [0x6F]={"RLD",0},
  [0x70]={"IN (C)",0},     [0x71]={"OUT (C),0",0}, [0x72]={"SBC HL,SP",0}, [0x73]={"LD (%s),SP",2},
  [0x78]={"IN A,(C)",0},   [0x79]={"OUT (C),A",0}, [0x7A]={"ADC HL,SP",0}, [0x7B]={"LD SP,(%s)",2},
  [0xA0]={"LDI",0},  [0xA1]={"CPI",0},  [0xA2]={"INI",0},  [0xA3]={"OUTI",0},
  [0xA8]={"LDD",0},  [0xA9]={"CPD",0},  [0xAA]={"IND",0},  [0xAB]={"OUTD",0},
  [0xB0]={"LDIR",0}, [0xB1]={"CPIR",0}, [0xB2]={"INIR",0}, [0xB3]={"OTIR",0},
  [0xB8]={"LDDR",0}, [0xB9]={"CPDR",0}, [0xBA]={"INDR",0}, [0xBB]={"OTDR",0},
}

-- t형 오퍼랜드를 fmt에 채워 최종 텍스트·소비 바이트 수를 낸다. base0=오퍼랜드 시작 오프셋.
local function z80_operand(fmt, t, read_byte, pc, base0)
  if t == 0 then return fmt, base0 end
  if t == 1 then
    local n = read_byte(pc + base0)
    return string.format(fmt, string.format("$%02X", n)), base0 + 1
  elseif t == 2 then
    local lo = read_byte(pc + base0)
    local hi = read_byte(pc + base0 + 1)
    return string.format(fmt, string.format("$%04X", lo + hi * 256)), base0 + 2
  else -- t == 3: 상대. 타깃 = 다음 명령 주소 + s8(disp).
    local d = read_byte(pc + base0)
    local target = (pc + base0 + 1 + z80_s8(d)) % 0x10000
    return string.format(fmt, string.format("$%04X", target)), base0 + 1
  end
end

-- CB 서브opcode를 니모닉으로. reg=피연산 표기(레지스터명 또는 (IX+d)).
local function z80_cb_text(sub, reg)
  local x = math.floor(sub / 0x40)      -- 0=롤/시프트, 1=BIT, 2=RES, 3=SET
  local y = math.floor(sub / 8) % 8     -- 비트 인덱스 또는 롤/시프트 그룹
  if x == 0 then return Z80CB[y] .. " " .. reg
  elseif x == 1 then return "BIT " .. y .. "," .. reg
  elseif x == 2 then return "RES " .. y .. "," .. reg
  else return "SET " .. y .. "," .. reg end
end

-- 한 명령 디코드 → text, len(바이트 수). read_byte(addr)로만 읽는다.
local function z80_decode(read_byte, pc)
  local op = read_byte(pc)
  if op == 0xCB then
    return z80_cb_text(read_byte(pc + 1), Z80R[read_byte(pc + 1) % 8]), 2
  elseif op == 0xED then
    local sub = read_byte(pc + 1)
    local e = Z80ED[sub]
    if not e then return string.format(".DB $ED $%02X", sub), 2 end
    local text, len = z80_operand(e[1], e[2], read_byte, pc, 2)
    return text, len
  elseif op == 0xDD or op == 0xFD then
    local ii = (op == 0xDD) and "IX" or "IY"
    local op2 = read_byte(pc + 1)
    if op2 == 0xCB then                 -- DD CB d sub : (ii+d) 비트 연산
      local d = read_byte(pc + 2)
      local sub = read_byte(pc + 3)
      return z80_cb_text(sub, "(" .. ii .. z80_disp(d) .. ")"), 4
    end
    if op2 == 0xDD or op2 == 0xFD or op2 == 0xED then
      return string.format(".DB $%02X", op), 1   -- 프리픽스 연쇄는 여기서 끊고 다음 반복이 재디코드
    end
    if op2 == 0xE9 then return "JP (" .. ii .. ")", 2 end  -- JP (HL)은 변위 없음
    local e = Z80BASE[op2]
    if not e then return string.format(".DB $%02X $%02X", op, op2), 2 end
    local fmt, t = e[1], e[2]
    local base0 = 2
    if fmt:find("%(HL%)") then          -- 메모리 간접 → (ii+d), 변위 바이트 소비
      local d = read_byte(pc + 2)
      fmt = fmt:gsub("%(HL%)", "(" .. ii .. z80_disp(d) .. ")")
      base0 = 3
    else
      fmt = fmt:gsub("HL", ii)          -- HL → IX/IY
    end
    local text, len = z80_operand(fmt, t, read_byte, pc, base0)
    return text, len
  else
    local e = Z80BASE[op]
    if not e then return string.format(".DB $%02X", op), 1 end
    local text, len = z80_operand(e[1], e[2], read_byte, pc, 1)
    return text, len
  end
end

-- disassemble(read_byte, start, count): start에서 count개 Z80 명령. 반환 [{addr,text,bytes}].
SYS.disassemble = function(read_byte, start, count)
  local out = {}
  local pc = start
  for _ = 1, count do
    local text, len = z80_decode(read_byte, pc)
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
assert(dir and dir ~= "", "emucap-sms: EMUCAP_ADAPTER_DIR 미설정 + 스크립트 경로 도출 실패 — launch로 띄우거나 파일에서 로드하라")
package.path = dir .. "/?.lua;" .. package.path
require("emucap-core")
