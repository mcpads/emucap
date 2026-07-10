-- NES / Famicom(Mesen) 엔트리. Mesen은 NES/Famicom을 하나의 nes 콘솔(cpuType "nes", 6502/2A03 CPU)로
-- 다룬다(GG가 sms 콘솔로 SMS/GG를 다루듯). 시스템별 SYS config 설정 후 제네릭 코어를 require.
-- memType·레지스터 키·CPU는 실측 확정(nes probe): cpu_type=nes, 버스=nesMemory, 6502 레지스터는 flat
-- dot-key(cpu.a/x/y/sp/pc/ps). ps는 raw 프로세서 상태 바이트(NV-BDIZC, 중첩 플래그 아님).
SYS = {
  system = "nes",
  system_label = "Nintendo Entertainment System",
  cpu_type = "nes",              -- emu.cpuType.nes (NES/Famicom 공통, 6502/2A03)
  default_memtype = "nesMemory", -- emu.memType.nesMemory (6502 CPU 버스 $0000-$FFFF)
  -- Mesen NesController setInput 키(소문자): a/b/start/select + 방향키. 표준 NES 패드는 X/Y/L/R 없음.
  buttons = {
    a = true, b = true, start = true, select = true,
    up = true, down = true, left = true, right = true,
  },
  aliases = {
    enter = "start", ["return"] = "start",
  },
  -- 6502 리셋 벡터: CPU가 $FFFC/$FFFD를 LE로 읽어 리셋 PC로 삼는다. 코어의 read16-포인터 모델
  -- (emu.read16(reset_vector, default_memtype) → 그 주소에 exec BP)에 맞는다 — SNES $00:FFFC와 동형.
  reset_vector = 0xFFFC,
  bank_mirror = false,   -- 6502엔 SNES식 $00/$80 뱅크 미러 없음
  dma_supported = false, -- SNES식 MDMAEN($420B) DMA 컨트롤러 없음 → dma kind BP는 미해당(NES OAM DMA $4014는 별개)
  -- read/write BP 주소 변환 맵: RAM offset을 6502 버스 base로 변환. 안 하면 addMemoryCallback이 버스
  -- $00xx(내부RAM/ROM)에 걸려 미발동한다. nesWorkRam/nesSaveRam(카트 PRG-RAM)은 버스 $6000-$7FFF.
  -- nesInternalRam은 offset 0 = 버스 $0000이라 identity(맵에 없음). 이미 버스인 nesMemory도 그대로.
  bp_bus_base = { nesWorkRam = 0x6000, nesSaveRam = 0x6000 },
  -- PPU측 메모리는 6502 CPU 버스에 없다 — CPU는 PPU 레지스터($2006/$2007 등)를 통해서만 접근하고, Mesen
  -- memory 콜백은 CPU 버스 접근만 잡으므로 이 memType들에 write/read BP를 걸면 절대 발화하지 않는다(조용한
  -- 미발동). bp_bus_base로 변환할 CPU-버스 주소가 없어 에러로 거부한다(SMS smsPaletteRam과 동형).
  -- Mesen Lua엔 PPU 콜백이 없어 PPU 포트 write 재구성을 지원하지 않는다.
  non_bus_write_memtypes = {
    nesPpuMemory          = "error",  -- PPU 주소공간 $0000-$3FFF 의사메모리(패턴/네임/팔레트 전체)
    nesNametableRam       = "error",  -- 네임테이블 VRAM
    nesPaletteRam         = "error",  -- 팔레트 RAM $3F00
    nesSpriteRam          = "error",  -- primary OAM($2004로만 접근)
    nesSecondarySpriteRam = "error",  -- secondary OAM(있으면)
    nesChrRam             = "error",  -- CHR RAM(PPU 패턴 테이블)
    nesChrRom             = "error",  -- CHR ROM(PPU 패턴 테이블)
  },
  -- 덤프 리전(emucap diff 입력). base는 버스/PPU주소(참조용), 실제 read는 memType offset 0부터.
  -- 없는 리전(PRG-RAM 미탑재 등)은 zero-fill로 안전(GB와 동일 처리).
  dump_regions = {
    { name = "iram",      mt = "nesInternalRam",  base = 0x0000, size = 0x800 },   -- 2KB CPU 내부RAM $0000-$07FF
    { name = "prgram",    mt = "nesSaveRam",      base = 0x6000, size = 0x2000 },  -- 8KB PRG-RAM/save $6000-$7FFF
    { name = "nametable", mt = "nesNametableRam", base = 0x2000, size = 0x800 },   -- 2KB PPU 네임테이블 RAM(VRAM)
    { name = "palette",   mt = "nesPaletteRam",   base = 0x3F00, size = 0x20 },    -- 32B 팔레트 RAM
    { name = "oam",       mt = "nesSpriteRam",    base = 0x0000, size = 0x100 },   -- 256B primary OAM($2004)
  },
  region_sizes = {
    nesInternalRam = 0x800, nesWorkRam = 0x2000, nesSaveRam = 0x2000,
    nesPrgRom = 0x80000, nesChrRom = 0x40000, nesChrRam = 0x2000,
    nesNametableRam = 0x800, nesPaletteRam = 0x20, nesSpriteRam = 0x100,
  },
}

-- ── 6502 디스어셈블러 + 콜/리턴 분류 (NES 전용 ISA) ──
-- Mesen2 Lua엔 디스어셈블 API가 없어 6502 디코더를 직접 구현한다. 코어가 nesMemory 버스에서 읽는
-- read_byte 클로저를 넘긴다. NES 2A03는 decimal을 안 쓰지만 opcode 해독 자체는 표준 6502. 미문서화
-- (illegal) opcode는 게임이 드물게 쓰지만 니모닉이 비표준이라 .byte $XX로 남긴다.

-- 호출: JSR(0x20). 리턴: RTS(0x60)·RTI(0x40). 코어의 SP기반 shadow 콜스택이 쓴다(6502 스택 $0100-$01FF).
SYS.op_is_call = function(op) return op == 0x20 end
SYS.op_is_return = function(op) return op == 0x60 or op == 0x40 end

-- record_hit이 히트 순간 잡는 레지스터(6502=a/x/y/sp/ps). pc는 코어가 full_pc(=cpu.pc)로 따로 싣는다.
-- ps는 raw 프로세서 상태 바이트(NV-BDIZC).
SYS.snapshot_regs = function(st)
  return {
    a = st["cpu.a"], x = st["cpu.x"], y = st["cpu.y"],
    sp = st["cpu.sp"], ps = st["cpu.ps"],
  }
end

-- NES PPU 포트($2006/$2007 등)는 SNES식 데이터-포트→VRAM 워드주소 매핑을 런타임 상태로 노출하지
-- 않는다 → nil(SNES 하드코딩 누수 방지).
SYS.port_semantics = nil

-- 어드레싱 모드 → 오퍼랜드 바이트 수.
local MODE_SIZE = {
  imp = 0, acc = 0, imm = 1,
  zp = 1, zpx = 1, zpy = 1, indx = 1, indy = 1, rel = 1,
  abs = 2, abx = 2, aby = 2, ind = 2,
}

-- 6502 문서화 opcode → {니모닉, 모드}. 여기 없는 값은 미문서화 → .byte 처리.
local OPCODES = {
  -- 0x0_
  [0x00]={"BRK","imp"},  [0x01]={"ORA","indx"}, [0x05]={"ORA","zp"},  [0x06]={"ASL","zp"},
  [0x08]={"PHP","imp"},  [0x09]={"ORA","imm"},  [0x0A]={"ASL","acc"}, [0x0D]={"ORA","abs"},
  [0x0E]={"ASL","abs"},
  -- 0x1_
  [0x10]={"BPL","rel"},  [0x11]={"ORA","indy"}, [0x15]={"ORA","zpx"}, [0x16]={"ASL","zpx"},
  [0x18]={"CLC","imp"},  [0x19]={"ORA","aby"},  [0x1D]={"ORA","abx"}, [0x1E]={"ASL","abx"},
  -- 0x2_
  [0x20]={"JSR","abs"},  [0x21]={"AND","indx"}, [0x24]={"BIT","zp"},  [0x25]={"AND","zp"},
  [0x26]={"ROL","zp"},   [0x28]={"PLP","imp"},  [0x29]={"AND","imm"}, [0x2A]={"ROL","acc"},
  [0x2C]={"BIT","abs"},  [0x2D]={"AND","abs"},  [0x2E]={"ROL","abs"},
  -- 0x3_
  [0x30]={"BMI","rel"},  [0x31]={"AND","indy"}, [0x35]={"AND","zpx"}, [0x36]={"ROL","zpx"},
  [0x38]={"SEC","imp"},  [0x39]={"AND","aby"},  [0x3D]={"AND","abx"}, [0x3E]={"ROL","abx"},
  -- 0x4_
  [0x40]={"RTI","imp"},  [0x41]={"EOR","indx"}, [0x45]={"EOR","zp"},  [0x46]={"LSR","zp"},
  [0x48]={"PHA","imp"},  [0x49]={"EOR","imm"},  [0x4A]={"LSR","acc"}, [0x4C]={"JMP","abs"},
  [0x4D]={"EOR","abs"},  [0x4E]={"LSR","abs"},
  -- 0x5_
  [0x50]={"BVC","rel"},  [0x51]={"EOR","indy"}, [0x55]={"EOR","zpx"}, [0x56]={"LSR","zpx"},
  [0x58]={"CLI","imp"},  [0x59]={"EOR","aby"},  [0x5D]={"EOR","abx"}, [0x5E]={"LSR","abx"},
  -- 0x6_
  [0x60]={"RTS","imp"},  [0x61]={"ADC","indx"}, [0x65]={"ADC","zp"},  [0x66]={"ROR","zp"},
  [0x68]={"PLA","imp"},  [0x69]={"ADC","imm"},  [0x6A]={"ROR","acc"}, [0x6C]={"JMP","ind"},
  [0x6D]={"ADC","abs"},  [0x6E]={"ROR","abs"},
  -- 0x7_
  [0x70]={"BVS","rel"},  [0x71]={"ADC","indy"}, [0x75]={"ADC","zpx"}, [0x76]={"ROR","zpx"},
  [0x78]={"SEI","imp"},  [0x79]={"ADC","aby"},  [0x7D]={"ADC","abx"}, [0x7E]={"ROR","abx"},
  -- 0x8_
  [0x81]={"STA","indx"}, [0x84]={"STY","zp"},   [0x85]={"STA","zp"},  [0x86]={"STX","zp"},
  [0x88]={"DEY","imp"},  [0x8A]={"TXA","imp"},  [0x8C]={"STY","abs"}, [0x8D]={"STA","abs"},
  [0x8E]={"STX","abs"},
  -- 0x9_
  [0x90]={"BCC","rel"},  [0x91]={"STA","indy"}, [0x94]={"STY","zpx"}, [0x95]={"STA","zpx"},
  [0x96]={"STX","zpy"},  [0x98]={"TYA","imp"},  [0x99]={"STA","aby"}, [0x9A]={"TXS","imp"},
  [0x9D]={"STA","abx"},
  -- 0xA_
  [0xA0]={"LDY","imm"},  [0xA1]={"LDA","indx"}, [0xA2]={"LDX","imm"}, [0xA4]={"LDY","zp"},
  [0xA5]={"LDA","zp"},   [0xA6]={"LDX","zp"},   [0xA8]={"TAY","imp"}, [0xA9]={"LDA","imm"},
  [0xAA]={"TAX","imp"},  [0xAC]={"LDY","abs"},  [0xAD]={"LDA","abs"}, [0xAE]={"LDX","abs"},
  -- 0xB_
  [0xB0]={"BCS","rel"},  [0xB1]={"LDA","indy"}, [0xB4]={"LDY","zpx"}, [0xB5]={"LDA","zpx"},
  [0xB6]={"LDX","zpy"},  [0xB8]={"CLV","imp"},  [0xB9]={"LDA","aby"}, [0xBA]={"TSX","imp"},
  [0xBC]={"LDY","abx"},  [0xBD]={"LDA","abx"},  [0xBE]={"LDX","aby"},
  -- 0xC_
  [0xC0]={"CPY","imm"},  [0xC1]={"CMP","indx"}, [0xC4]={"CPY","zp"},  [0xC5]={"CMP","zp"},
  [0xC6]={"DEC","zp"},   [0xC8]={"INY","imp"},  [0xC9]={"CMP","imm"}, [0xCA]={"DEX","imp"},
  [0xCC]={"CPY","abs"},  [0xCD]={"CMP","abs"},  [0xCE]={"DEC","abs"},
  -- 0xD_
  [0xD0]={"BNE","rel"},  [0xD1]={"CMP","indy"}, [0xD5]={"CMP","zpx"}, [0xD6]={"DEC","zpx"},
  [0xD8]={"CLD","imp"},  [0xD9]={"CMP","aby"},  [0xDD]={"CMP","abx"}, [0xDE]={"DEC","abx"},
  -- 0xE_
  [0xE0]={"CPX","imm"},  [0xE1]={"SBC","indx"}, [0xE4]={"CPX","zp"},  [0xE5]={"SBC","zp"},
  [0xE6]={"INC","zp"},   [0xE8]={"INX","imp"},  [0xE9]={"SBC","imm"}, [0xEA]={"NOP","imp"},
  [0xEC]={"CPX","abs"},  [0xED]={"SBC","abs"},  [0xEE]={"INC","abs"},
  -- 0xF_
  [0xF0]={"BEQ","rel"},  [0xF1]={"SBC","indy"}, [0xF5]={"SBC","zpx"}, [0xF6]={"INC","zpx"},
  [0xF8]={"SED","imp"},  [0xF9]={"SBC","aby"},  [0xFD]={"SBC","abx"}, [0xFE]={"INC","abx"},
}

local function s8(v) return (v >= 0x80) and (v - 0x100) or v end

-- 오퍼랜드 문자열. addr16=명령 시작 16비트 주소(상대 분기 타깃 계산용), b1/b2=오퍼랜드 바이트.
local function fmt_operand(mode, addr16, size, b1, b2)
  if mode == "imp" then return "" end
  if mode == "acc" then return "A" end
  -- 상대 분기: 타깃 = 다음 명령 주소(addr16+2) + s8(오프셋).
  if mode == "rel" then return string.format("$%04X", (addr16 + 2 + s8(b1)) % 0x10000) end
  local val = (size == 2) and (b1 + b2 * 256) or b1
  if mode == "imm" then return string.format("#$%02X", val)
  elseif mode == "zp" then return string.format("$%02X", val)
  elseif mode == "zpx" then return string.format("$%02X,X", val)
  elseif mode == "zpy" then return string.format("$%02X,Y", val)
  elseif mode == "indx" then return string.format("($%02X,X)", val)
  elseif mode == "indy" then return string.format("($%02X),Y", val)
  elseif mode == "abs" then return string.format("$%04X", val)
  elseif mode == "abx" then return string.format("$%04X,X", val)
  elseif mode == "aby" then return string.format("$%04X,Y", val)
  elseif mode == "ind" then return string.format("($%04X)", val)  -- JMP (지시자)
  end
  return ""
end

-- disassemble(read_byte, start, count): start에서 count개 6502 명령. 반환 [{addr,text,bytes}].
SYS.disassemble = function(read_byte, start, count)
  local out = {}
  local a = start
  for _ = 1, count do
    local addr16 = a % 0x10000
    local opcode = read_byte(a)
    local entry = OPCODES[opcode]
    if not entry then
      out[#out + 1] = { addr = string.format("0x%06X", a),
                        text = string.format(".byte $%02X", opcode),
                        bytes = string.format("%02X", opcode) }
      a = a + 1
    else
      local mnem, mode = entry[1], entry[2]
      local size = MODE_SIZE[mode]
      local b1 = (size >= 1) and read_byte(a + 1) or 0
      local b2 = (size >= 2) and read_byte(a + 2) or 0
      local operand = fmt_operand(mode, addr16, size, b1, b2)
      local text = (operand == "") and mnem or (mnem .. " " .. operand)
      local raw = string.format("%02X", opcode)
      if size >= 1 then raw = raw .. string.format(" %02X", b1) end
      if size >= 2 then raw = raw .. string.format(" %02X", b2) end
      out[#out + 1] = { addr = string.format("0x%06X", a), text = text, bytes = raw }
      a = a + 1 + size
    end
  end
  return out
end

local dir = os.getenv("EMUCAP_ADAPTER_DIR")
if not dir or dir == "" then
  -- 폴백: env가 없으면(수동 Script Window 로드 등) 이 스크립트 파일 경로에서 어댑터 디렉터리를 도출한다.
  local src = debug.getinfo(1, "S").source
  if src and src:sub(1, 1) == "@" then dir = src:sub(2):match("^(.*)[/\\][^/\\]+$") end
end
assert(dir and dir ~= "", "emucap-nes: EMUCAP_ADAPTER_DIR 미설정 + 스크립트 경로 도출 실패 — launch로 띄우거나 파일에서 로드하라")
package.path = dir .. "/?.lua;" .. package.path
require("emucap-core")
