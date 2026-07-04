-- SNES(Mesen) 엔트리. 시스템별 SYS config를 설정하고 제네릭 코어(emucap-core.lua)를 dofile 한다.
-- launch가 이 파일을 Mesen --args LUA로 넘기고 EMUCAP_ADAPTER_DIR로 코어 위치를 준다.
SYS = {
  system = "snes",
  system_label = "SNES",
  cpu_type = "snes",              -- emu.cpuType.snes
  default_memtype = "snesMemory", -- emu.memType.snesMemory
  -- Mesen emu.setInput은 소문자 키만 인식한다.
  buttons = {
    a = true, b = true, x = true, y = true, l = true, r = true,
    start = true, select = true, up = true, down = true, left = true, right = true,
  },
  aliases = {
    enter = "start", ["return"] = "start",
    l1 = "l", r1 = "r", lb = "l", rb = "r",
  },
  reset_vector = 0xFFFC,   -- $00:FFFC(뱅크0)
  bank_mirror = true,      -- snesMemory $2000-$7FFF의 $00/$80 뱅크 미러 BP 자동등록
  dma_supported = true,    -- SNES MDMAEN($420B) DMA 컨트롤러 → dma kind BP 지원
  bp_bus_base = {},        -- read/write BP 주소 변환 맵: SNES는 snesWorkRam low-RAM==버스 미러라 identity(빈 맵=변환 없음)
  dump_regions = {
    { name = "wram", mt = "snesWorkRam",   base = 8257536, size = 0x20000 },  -- 128KB @ $7E0000
    { name = "vram", mt = "snesVideoRam",  base = 0,       size = 0x10000 },  -- 64KB
    { name = "cram", mt = "snesCgRam",     base = 0,       size = 0x200 },    -- 512B(팔레트)
    { name = "oam",  mt = "snesSpriteRam", base = 0,       size = 0x220 },    -- 544B(스프라이트)
  },
  region_sizes = {
    snesWorkRam = 0x20000, snesVideoRam = 0x10000, snesCgRam = 0x200,
    snesSpriteRam = 0x220, snesSaveRam = 0x8000,
  },
}

-- ── 65816 디스어셈블러 + 콜/리턴 분류 (SNES 전용 ISA — 코어에서 이관) ──
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

-- JSR/JSL(호출)·RTS/RTL/RTI(리턴) 분류 — 코어의 콜스택 shadow-track이 쓴다.
SYS.op_is_call = function(op) return op == 0x20 or op == 0x22 end
SYS.op_is_return = function(op) return op == 0x60 or op == 0x6b or op == 0x40 end

-- record_hit이 히트 순간 잡는 레지스터 세트(ISA별). SNES=65816: a/x/y/sp/d/dbr/k/ps. pc는 코어가 full_pc로 따로 실는다.
SYS.snapshot_regs = function(st)
  return { a = st["cpu.a"], x = st["cpu.x"], y = st["cpu.y"], sp = st["cpu.sp"],
           d = st["cpu.d"], dbr = st["cpu.dbr"], k = st["cpu.k"], ps = st["cpu.ps"] }
end

-- write BP가 PPU 데이터 포트에 걸렸으면 목적지 주소를 이벤트에 라벨링(런타임 타일맵 추적). CPU의 소량 직접
-- 포트 쓰기(STA $2118 등 타일맵 1엔트리)가 "VRAM 어느 워드주소로 갔나"를 PC·값과 함께 답하게. addr은
-- 뱅크미러로 $80xxxx일 수 있어 하위 16비트로 판별. ($2118/9 VRAM·$2122 CGRAM·$2104 OAM.)
SYS.port_semantics = function(ev, addr, st)
  local low = addr % 0x10000
  if low == 0x2118 or low == 0x2119 then ev.vram_addr = st["ppu.vramAddress"]
  elseif low == 0x2122 then ev.cgram_addr = st["ppu.cgramAddress"]
  elseif low == 0x2104 then ev.oam_addr = st["ppu.oamRamAddress"] end
end

-- disassemble(read_byte, start, count): 24비트 실행주소(뱅크 포함)에서 count개 명령.
-- M/X 시작값은 현재 cpu.ps(없으면 8bit 가정). 반환 [{addr,text,bytes}].
SYS.disassemble = function(read_byte, start, count)
  local st = emu.getState()
  local ps = st["cpu.ps"] or st["cpu.p"] or st["cpu.status"] or 0x30
  local m8 = (math.floor(ps / 0x20) % 2) == 1   -- bit5=M: set→8bit A
  local x8 = (math.floor(ps / 0x10) % 2) == 1   -- bit4=X: set→8bit X/Y
  local out = {}
  local a = start
  for _ = 1, count do
    local addr16 = a % 0x10000
    local opcode = read_byte(a)
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
      local b1 = (size >= 1) and read_byte(a + 1) or 0
      local b2 = (size >= 2) and read_byte(a + 2) or 0
      local b3 = (size >= 3) and read_byte(a + 3) or 0
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
  return out
end

-- 코어는 require로 로드한다(Mesen 샌드박스에서 require는 검증됨 — socket.core). package.path에
-- 어댑터 디렉터리를 얹어 emucap-core.lua를 찾게 한다.
local dir = os.getenv("EMUCAP_ADAPTER_DIR")
if not dir or dir == "" then
  -- 폴백: env가 없으면(수동 Script Window 로드 등) 이 스크립트 파일 경로에서 어댑터 디렉터리를 도출한다.
  local src = debug.getinfo(1, "S").source
  if src and src:sub(1, 1) == "@" then dir = src:sub(2):match("^(.*)[/\\][^/\\]+$") end
end
assert(dir and dir ~= "", "emucap-snes: EMUCAP_ADAPTER_DIR 미설정 + 스크립트 경로 도출 실패 — launch로 띄우거나 파일에서 로드하라")
package.path = dir .. "/?.lua;" .. package.path
require("emucap-core")
