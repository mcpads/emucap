-- Game Boy Advance(Mesen) 엔트리. Mesen cpuType "gba"(ARM7TDMI). 시스템별 SYS config 설정 후 제네릭
-- 코어를 require. memType·레지스터 키·CPU는 실측 확정(gba probe): cpu_type=gba, PC=cpu.r15, CPSR는 플랫
-- 정수가 아니라 중첩 불리언(cpu.cpsr.negative 등), 버스=gbaMemory, RAM=gbaIntWorkRam/gbaExtWorkRam/
-- gbaVideoRam/gbaPaletteRam/gbaSpriteRam/gbaSaveRam/gbaPrgRom. 콘솔 밖 read는 zero-fill(실측).
--
-- disassemble는 ARM7TDMI Lua 디코더로 제공한다(ARM 32비트 + Thumb 16비트, CPSR T비트로 모드 판정).
-- op_is_call/op_is_return은 미제공 — ARM 콜스택은 LR기반이라 코어의 SP모델과 안 맞는다(이번 범위 밖).
-- 따라서 disassemble는 광고되고 call_stack은 미지원으로 광고·거부된다.
-- memory/state/screenshot/input/BP/save·load·probe·watch_register·set_trace는 상속.
SYS = {
  system = "gba",
  system_label = "Game Boy Advance",
  cpu_type = "gba",             -- emu.cpuType.gba (ARM7TDMI)
  default_memtype = "gbaMemory", -- emu.memType.gbaMemory (ARM CPU 버스, 32비트 주소공간)
  -- Mesen GbaController setInput 키(소문자): a/b/l/r/start/select + 방향키(X/Y 없음).
  buttons = {
    a = true, b = true, l = true, r = true,
    start = true, select = true,
    up = true, down = true, left = true, right = true,
  },
  aliases = {
    enter = "start", ["return"] = "start",
    l1 = "l", r1 = "r", lb = "l", rb = "r",
  },
  -- ARM 예외벡터($0=리셋)는 SNES식 포인터 테이블이 아니고 GBA 카트는 $08000000에서 시작한다 —
  -- break_on_reset의 read16-포인터 모델이 맞지 않아 지원하지 않는다.
  reset_vector = nil,
  bank_mirror = false,
  dma_supported = false,  -- GBA DMA는 SNES MDMAEN($420B) 컨트롤러가 아님 → dma kind BP 미지원
  -- full-range exec 콜백(save/load/probe·watch_register·set_trace) 상한을 32비트로 올린다. GBA는 카트ROM
  -- $08000000·EWRAM $02000000·IWRAM $03000000에서 실행하므로 코어 기본 24비트(0xFFFFFF)면 콜백이 절대
  -- 발화하지 않는다(save_state가 영영 미완). $0FFFFFFF이 전 실행영역을 덮는다.
  exec_range_max = 0x0FFFFFFF,
  -- read/write BP 주소 변환 맵: RAM memType offset을 GBA 버스 base로 변환한다. 안 하면
  -- addMemoryCallback이 버스 저주소(BIOS)에 걸려 미발동. 이미 버스인 gbaMemory는 맵에 없어 그대로.
  bp_bus_base = {
    gbaIntWorkRam = 0x03000000, gbaExtWorkRam = 0x02000000,
    gbaVideoRam   = 0x06000000, gbaPaletteRam = 0x05000000,
    gbaSpriteRam  = 0x07000000, gbaSaveRam    = 0x0E000000,
    gbaPrgRom     = 0x08000000,
  },
  -- 덤프 리전(emucap diff 입력). base는 버스주소(참조용), 실제 read는 memType offset 0부터. 표준 GBA 크기:
  -- IWRAM 32KB·EWRAM 256KB·VRAM 96KB·PAL 1KB·OAM 1KB.
  dump_regions = {
    { name = "iwram", mt = "gbaIntWorkRam", base = 0x03000000, size = 0x8000 },
    { name = "ewram", mt = "gbaExtWorkRam", base = 0x02000000, size = 0x40000 },
    { name = "vram",  mt = "gbaVideoRam",   base = 0x06000000, size = 0x18000 },
    { name = "pram",  mt = "gbaPaletteRam", base = 0x05000000, size = 0x400 },
    { name = "oam",   mt = "gbaSpriteRam",  base = 0x07000000, size = 0x400 },
  },
  region_sizes = {
    gbaIntWorkRam = 0x8000, gbaExtWorkRam = 0x40000, gbaVideoRam = 0x18000,
    gbaPaletteRam = 0x400, gbaSpriteRam = 0x400, gbaSaveRam = 0x20000,
    gbaPrgRom = 0x2000000,
  },
}

-- record_hit이 히트 순간 잡는 레지스터(ARM7=r0..r15 + CPSR 플래그). pc는 코어가 full_pc(=cpu.r15 폴백)로
-- 따로 싣는다. CPSR은 Mesen이 플랫 정수가 아니라 중첩 불리언으로 노출하므로(실측) 플래그를 서브테이블로 싣는다.
SYS.snapshot_regs = function(st)
  local r = {}
  for i = 0, 15 do r["r" .. i] = st["cpu.r" .. i] end
  r.cpsr = {
    negative = st["cpu.cpsr.negative"], zero = st["cpu.cpsr.zero"],
    carry = st["cpu.cpsr.carry"], overflow = st["cpu.cpsr.overflow"],
    irqDisable = st["cpu.cpsr.irqDisable"], fiqDisable = st["cpu.cpsr.fiqDisable"],
    thumb = st["cpu.cpsr.thumb"],
  }
  return r
end

-- GBA VDP는 SNES식 데이터-포트→VRAM 워드주소 매핑을 런타임 상태로 노출하지 않는다 → nil(누수 방지).
SYS.port_semantics = nil

-- ── ARM7TDMI 디스어셈블러 ─────────────────────────────────────
-- Mesen2 Lua엔 디스어셈블 API가 없어 ARM7(ARMv4T)을 직접 디코드한다. 모드(ARM 32비트/Thumb 16비트)는
-- SYS.disassemble이 emu.getState()의 cpu.cpsr.thumb로 판정한다(임의 주소가 반대 모드일 가능성은 한계).
-- 비트 추출은 나눗셈·나머지로 한다(다른 엔트리와 동일 — 임베디드 Lua 비트연산 비의존).

local function pow2(n) return math.floor(2 ^ n + 0.5) end
local function bits(w, hi, lo) return math.floor(w / pow2(lo)) % pow2(hi - lo + 1) end

local function signext(v, n)
  local half = pow2(n - 1)
  if v >= half then return v - pow2(n) end
  return v
end

local function mask32(v) return v % pow2(32) end

local function ror32(v, amount)
  amount = amount % 32
  if amount == 0 then return v end
  local low = math.floor(v / pow2(amount))
  local high = (v % pow2(amount)) * pow2(32 - amount)
  return (low + high) % pow2(32)
end

local function unum(v)
  if v < 10 then return tostring(v) end
  return string.format("0x%X", v)
end
local function imm_str(v) return "#" .. unum(v) end
local function hexaddr(v) return string.format("0x%08X", mask32(v)) end

local ARMREG = { [13] = "SP", [14] = "LR", [15] = "PC" }
local function reg(n) return ARMREG[n] or ("R" .. n) end

-- 조건코드 접미사(AL=1110은 생략, NV=1111은 arm_text에서 .word로 처리).
local COND = {
  [0] = "EQ", [1] = "NE", [2] = "CS", [3] = "CC", [4] = "MI", [5] = "PL", [6] = "VS", [7] = "VC",
  [8] = "HI", [9] = "LS", [10] = "GE", [11] = "LT", [12] = "GT", [13] = "LE", [14] = "", [15] = "NV",
}
local DP = {
  [0] = "AND", [1] = "EOR", [2] = "SUB", [3] = "RSB", [4] = "ADD", [5] = "ADC", [6] = "SBC", [7] = "RSC",
  [8] = "TST", [9] = "TEQ", [10] = "CMP", [11] = "CMN", [12] = "ORR", [13] = "MOV", [14] = "BIC", [15] = "MVN",
}
local SHIFT = { [0] = "LSL", [1] = "LSR", [2] = "ASR", [3] = "ROR" }

-- 레지스터 리스트(LDM/STM/PUSH/POP): 3개 이상 연속이면 R0-R3로 묶고, 1~2개는 개별 나열.
local function reg_list(mask)
  local parts, i = {}, 0
  while i <= 15 do
    if bits(mask, i, i) == 1 then
      local j = i
      while j < 15 and bits(mask, j + 1, j + 1) == 1 do j = j + 1 end
      if (j - i) >= 2 then
        parts[#parts + 1] = reg(i) .. "-" .. reg(j)
      else
        for k = i, j do parts[#parts + 1] = reg(k) end
      end
      i = j + 1
    else
      i = i + 1
    end
  end
  return "{" .. table.concat(parts, ", ") .. "}"
end

-- 즉치 이동 시프트가 없는 스케일드 레지스터(LDR/STR 레지스터 오프셋 + data-processing 시프트 공통).
local function scaled_reg(w)
  local rm, styp, amt = bits(w, 3, 0), bits(w, 6, 5), bits(w, 11, 7)
  if styp == 0 and amt == 0 then return reg(rm) end
  if styp == 3 and amt == 0 then return reg(rm) .. ", RRX" end
  if (styp == 1 or styp == 2) and amt == 0 then amt = 32 end
  return reg(rm) .. ", " .. SHIFT[styp] .. " #" .. amt
end

-- data-processing operand2(레지스터 형): bit4=1이면 Rs 지정 시프트, 아니면 즉치 시프트.
local function dp_shifter(w)
  if bits(w, 4, 4) == 1 then
    return reg(bits(w, 3, 0)) .. ", " .. SHIFT[bits(w, 6, 5)] .. " " .. reg(bits(w, 11, 8))
  end
  return scaled_reg(w)
end

local function dp_imm(w)
  return imm_str(ror32(bits(w, 7, 0), bits(w, 11, 8) * 2))
end

local function psr_fields(mask)
  local f = ""
  if bits(mask, 3, 3) == 1 then f = f .. "f" end
  if bits(mask, 2, 2) == 1 then f = f .. "s" end
  if bits(mask, 1, 1) == 1 then f = f .. "x" end
  if bits(mask, 0, 0) == 1 then f = f .. "c" end
  if f == "" then return "" end
  return "_" .. f
end

local function dp_build(w, cond, opcode, s, op2)
  local name = DP[opcode]
  local sfx = cond .. ((s == 1) and "S" or "")
  if opcode >= 8 and opcode <= 11 then       -- TST/TEQ/CMP/CMN: Rn, op2 (S 암묵)
    return name .. cond .. " " .. reg(bits(w, 19, 16)) .. ", " .. op2
  elseif opcode == 13 or opcode == 15 then   -- MOV/MVN: Rd, op2
    return name .. sfx .. " " .. reg(bits(w, 15, 12)) .. ", " .. op2
  end
  return name .. sfx .. " " .. reg(bits(w, 15, 12)) .. ", " .. reg(bits(w, 19, 16)) .. ", " .. op2
end

-- LDR/STR·halfword 공통 어드레싱: pre/post·writeback·부호로 [Rn, off]{!} 또는 [Rn], off 조립.
local function xfer_addr(name, rd, rn, p, wb, offstr, zero)
  if p == 1 then
    if zero then
      return name .. " " .. reg(rd) .. ", [" .. reg(rn) .. "]" .. ((wb == 1) and "!" or "")
    end
    return name .. " " .. reg(rd) .. ", [" .. reg(rn) .. ", " .. offstr .. "]" .. ((wb == 1) and "!" or "")
  end
  return name .. " " .. reg(rd) .. ", [" .. reg(rn) .. "], " .. offstr
end

-- 한 ARM 워드(LE)를 니모닉으로. 못 알아보면 .word.
local function arm_text(w, addr)
  local condn = bits(w, 31, 28)
  if condn == 15 then return string.format(".word 0x%08X", w) end   -- NV/확장(ARM7 미실행)
  local cond = COND[condn]

  if bits(w, 27, 4) == 0x12FFF1 then                                -- BX
    return "BX" .. cond .. " " .. reg(bits(w, 3, 0))
  end
  if bits(w, 27, 24) == 0xF then                                    -- SWI
    return string.format("SWI%s #0x%X", cond, bits(w, 23, 0))
  end

  local op1 = bits(w, 27, 25)
  if op1 == 5 then                                                  -- B / BL
    local l = bits(w, 24, 24)
    local off = signext(bits(w, 23, 0), 24) * 4
    return (l == 1 and "BL" or "B") .. cond .. " " .. hexaddr(addr + 8 + off)
  end
  if op1 == 4 then                                                  -- LDM / STM
    local p, u, sbit, wb, l = bits(w, 24, 24), bits(w, 23, 23), bits(w, 22, 22), bits(w, 21, 21), bits(w, 20, 20)
    local rn = bits(w, 19, 16)
    local list = reg_list(bits(w, 15, 0))
    if l == 0 and p == 1 and u == 0 and wb == 1 and rn == 13 then return "PUSH" .. cond .. " " .. list end
    if l == 1 and p == 0 and u == 1 and wb == 1 and rn == 13 then return "POP" .. cond .. " " .. list end
    local mode
    if p == 1 and u == 1 then mode = "IB" elseif p == 0 and u == 1 then mode = "IA"
    elseif p == 1 and u == 0 then mode = "DB" else mode = "DA" end
    return (l == 1 and "LDM" or "STM") .. mode .. cond .. " " .. reg(rn)
      .. ((wb == 1) and "!" or "") .. ", " .. list .. ((sbit == 1) and "^" or "")
  end
  if op1 == 2 or op1 == 3 then                                      -- LDR / STR
    local i, p, u, b, wb, l = bits(w, 25, 25), bits(w, 24, 24), bits(w, 23, 23), bits(w, 22, 22), bits(w, 21, 21), bits(w, 20, 20)
    local rn, rd = bits(w, 19, 16), bits(w, 15, 12)
    local name = (l == 1 and "LDR" or "STR") .. cond .. ((b == 1) and "B" or "")
    local sign = (u == 1) and "" or "-"
    if i == 0 then
      local imm = bits(w, 11, 0)
      return xfer_addr(name, rd, rn, p, wb, "#" .. sign .. unum(imm), imm == 0)
    end
    return xfer_addr(name, rd, rn, p, wb, sign .. scaled_reg(w), false)
  end
  if op1 == 6 or op1 == 7 then                                      -- 코프로세서(ARM7 emucap 범위 밖)
    return string.format(".word 0x%08X", w)
  end

  -- op1 ∈ {0,1}: data-processing 및 그 특수형(bit25=I).
  local i = bits(w, 25, 25)
  if i == 0 then
    if bits(w, 7, 4) == 9 then
      local sel = bits(w, 24, 23)
      if sel == 0 then                                              -- MUL / MLA
        local a, s = bits(w, 21, 21), bits(w, 20, 20)
        local rd, rn, rs, rm = bits(w, 19, 16), bits(w, 15, 12), bits(w, 11, 8), bits(w, 3, 0)
        local sfx = cond .. ((s == 1) and "S" or "")
        if a == 0 then return "MUL" .. sfx .. " " .. reg(rd) .. ", " .. reg(rm) .. ", " .. reg(rs) end
        return "MLA" .. sfx .. " " .. reg(rd) .. ", " .. reg(rm) .. ", " .. reg(rs) .. ", " .. reg(rn)
      elseif sel == 1 then                                          -- UMULL/UMLAL/SMULL/SMLAL
        local uns, a, s = bits(w, 22, 22), bits(w, 21, 21), bits(w, 20, 20)
        local rdhi, rdlo, rs, rm = bits(w, 19, 16), bits(w, 15, 12), bits(w, 11, 8), bits(w, 3, 0)
        local nm = (uns == 0) and (a == 0 and "UMULL" or "UMLAL") or (a == 0 and "SMULL" or "SMLAL")
        return nm .. cond .. ((s == 1) and "S" or "") .. " " .. reg(rdlo) .. ", " .. reg(rdhi) .. ", " .. reg(rm) .. ", " .. reg(rs)
      elseif sel == 2 then                                          -- SWP / SWPB
        local rn, rd, rm = bits(w, 19, 16), bits(w, 15, 12), bits(w, 3, 0)
        return "SWP" .. cond .. ((bits(w, 22, 22) == 1) and "B" or "") .. " " .. reg(rd) .. ", " .. reg(rm) .. ", [" .. reg(rn) .. "]"
      end
      return string.format(".word 0x%08X", w)
    elseif bits(w, 7, 7) == 1 and bits(w, 4, 4) == 1 then           -- LDRH/STRH/LDRSB/LDRSH
      local p, u, immf, wb, l = bits(w, 24, 24), bits(w, 23, 23), bits(w, 22, 22), bits(w, 21, 21), bits(w, 20, 20)
      local rn, rd, sh = bits(w, 19, 16), bits(w, 15, 12), bits(w, 6, 5)
      local ty = (sh == 1 and "H") or (sh == 2 and "SB") or "SH"
      local name
      if l == 1 then
        name = "LDR" .. cond .. ty
      elseif sh == 1 then
        name = "STR" .. cond .. "H"
      else
        return string.format(".word 0x%08X", w)                    -- LDRD/STRD(ARMv5+)는 미대상
      end
      local sign = (u == 1) and "" or "-"
      if immf == 1 then
        local imm = bits(w, 11, 8) * 16 + bits(w, 3, 0)
        return xfer_addr(name, rd, rn, p, wb, "#" .. sign .. unum(imm), imm == 0)
      end
      return xfer_addr(name, rd, rn, p, wb, sign .. reg(bits(w, 3, 0)), false)
    else
      local opcode, s = bits(w, 24, 21), bits(w, 20, 20)
      if s == 0 and opcode >= 8 and opcode <= 11 then               -- MRS / MSR(레지스터)
        local psr = (bits(w, 22, 22) == 1) and "SPSR" or "CPSR"
        if bits(w, 21, 21) == 0 then
          return "MRS" .. cond .. " " .. reg(bits(w, 15, 12)) .. ", " .. psr
        end
        return "MSR" .. cond .. " " .. psr .. psr_fields(bits(w, 19, 16)) .. ", " .. reg(bits(w, 3, 0))
      end
      return dp_build(w, cond, opcode, s, dp_shifter(w))
    end
  else
    local opcode, s = bits(w, 24, 21), bits(w, 20, 20)
    if s == 0 and opcode >= 8 and opcode <= 11 then                 -- MSR(즉치)
      local psr = (bits(w, 22, 22) == 1) and "SPSR" or "CPSR"
      return "MSR" .. cond .. " " .. psr .. psr_fields(bits(w, 19, 16)) .. ", " .. dp_imm(w)
    end
    return dp_build(w, cond, opcode, s, dp_imm(w))
  end
end

local function arm_decode(read_byte, pc)
  local w = read_byte(pc) + read_byte(pc + 1) * 256 + read_byte(pc + 2) * 65536 + read_byte(pc + 3) * 16777216
  return arm_text(w, pc), 4
end

local THUMB_ALU = {
  [0] = "AND", [1] = "EOR", [2] = "LSL", [3] = "LSR", [4] = "ASR", [5] = "ADC", [6] = "SBC", [7] = "ROR",
  [8] = "TST", [9] = "NEG", [10] = "CMP", [11] = "CMN", [12] = "ORR", [13] = "MUL", [14] = "BIC", [15] = "MVN",
}
local THUMB_IMM = { [0] = "MOV", [1] = "CMP", [2] = "ADD", [3] = "SUB" }
local THUMB_HI = { [0] = "ADD", [1] = "CMP", [2] = "MOV" }

-- 한 Thumb 반워드(LE)를 니모닉으로. BL은 2반워드라 4바이트 소비. 못 알아보면 .hword.
local function thumb_decode(read_byte, pc)
  local hw = read_byte(pc) + read_byte(pc + 1) * 256
  local h3 = bits(hw, 15, 13)

  if h3 == 0 then
    local op = bits(hw, 12, 11)
    if op == 3 then                                                -- ADD/SUB (레지스터/즉치3)
      local sub = bits(hw, 9, 9)
      local rno, rs, rd = bits(hw, 8, 6), bits(hw, 5, 3), bits(hw, 2, 0)
      local name = (sub == 1) and "SUB" or "ADD"
      if bits(hw, 10, 10) == 1 then
        return name .. " " .. reg(rd) .. ", " .. reg(rs) .. ", #" .. rno, 2
      end
      return name .. " " .. reg(rd) .. ", " .. reg(rs) .. ", " .. reg(rno), 2
    end
    local amt = bits(hw, 10, 6)                                     -- LSL/LSR/ASR 즉치 시프트
    if (op == 1 or op == 2) and amt == 0 then amt = 32 end
    return SHIFT[op] .. " " .. reg(bits(hw, 2, 0)) .. ", " .. reg(bits(hw, 5, 3)) .. ", #" .. amt, 2

  elseif h3 == 1 then                                              -- MOV/CMP/ADD/SUB 즉치8
    return THUMB_IMM[bits(hw, 12, 11)] .. " " .. reg(bits(hw, 10, 8)) .. ", " .. imm_str(bits(hw, 7, 0)), 2

  elseif h3 == 2 then
    if bits(hw, 12, 12) == 0 then
      if bits(hw, 11, 11) == 0 then
        if bits(hw, 10, 10) == 0 then                              -- ALU
          return THUMB_ALU[bits(hw, 9, 6)] .. " " .. reg(bits(hw, 2, 0)) .. ", " .. reg(bits(hw, 5, 3)), 2
        end
        local op = bits(hw, 9, 8)                                  -- Hi 레지스터 / BX
        local rs = bits(hw, 5, 3) + bits(hw, 6, 6) * 8
        local rd = bits(hw, 2, 0) + bits(hw, 7, 7) * 8
        if op == 3 then return "BX " .. reg(rs), 2 end
        return THUMB_HI[op] .. " " .. reg(rd) .. ", " .. reg(rs), 2
      end
      local off = bits(hw, 7, 0) * 4                               -- PC-relative LDR
      local pool = mask32((pc + 4) - ((pc + 4) % 4) + off)
      return "LDR " .. reg(bits(hw, 10, 8)) .. ", [PC, #" .. unum(off) .. "]  ; " .. string.format("0x%08X", pool), 2
    end
    if bits(hw, 9, 9) == 0 then                                    -- 레지스터 오프셋 load/store
      local name = (bits(hw, 11, 11) == 1 and "LDR" or "STR") .. ((bits(hw, 10, 10) == 1) and "B" or "")
      return name .. " " .. reg(bits(hw, 2, 0)) .. ", [" .. reg(bits(hw, 5, 3)) .. ", " .. reg(bits(hw, 8, 6)) .. "]", 2
    end
    local h, s = bits(hw, 11, 11), bits(hw, 10, 10)               -- 부호확장 byte/halfword
    local name = (s == 0 and h == 0 and "STRH") or (s == 0 and h == 1 and "LDRH")
      or (s == 1 and h == 0 and "LDRSB") or "LDRSH"
    return name .. " " .. reg(bits(hw, 2, 0)) .. ", [" .. reg(bits(hw, 5, 3)) .. ", " .. reg(bits(hw, 8, 6)) .. "]", 2

  elseif h3 == 3 then                                              -- 즉치 오프셋 load/store
    local b, l = bits(hw, 12, 12), bits(hw, 11, 11)
    local rb, rd = bits(hw, 5, 3), bits(hw, 2, 0)
    local name = (l == 1 and "LDR" or "STR") .. ((b == 1) and "B" or "")
    local off = (b == 1) and bits(hw, 10, 6) or bits(hw, 10, 6) * 4
    if off == 0 then return name .. " " .. reg(rd) .. ", [" .. reg(rb) .. "]", 2 end
    return name .. " " .. reg(rd) .. ", [" .. reg(rb) .. ", #" .. unum(off) .. "]", 2

  elseif h3 == 4 then
    if bits(hw, 12, 12) == 0 then                                  -- halfword load/store
      local name = (bits(hw, 11, 11) == 1) and "LDRH" or "STRH"
      local rb, rd, off = bits(hw, 5, 3), bits(hw, 2, 0), bits(hw, 10, 6) * 2
      if off == 0 then return name .. " " .. reg(rd) .. ", [" .. reg(rb) .. "]", 2 end
      return name .. " " .. reg(rd) .. ", [" .. reg(rb) .. ", #" .. unum(off) .. "]", 2
    end
    local name = (bits(hw, 11, 11) == 1) and "LDR" or "STR"       -- SP-relative load/store
    return name .. " " .. reg(bits(hw, 10, 8)) .. ", [SP, #" .. unum(bits(hw, 7, 0) * 4) .. "]", 2

  elseif h3 == 5 then
    if bits(hw, 12, 12) == 0 then                                  -- ADD Rd, PC/SP, #imm
      local base = (bits(hw, 11, 11) == 1) and "SP" or "PC"
      return "ADD " .. reg(bits(hw, 10, 8)) .. ", " .. base .. ", #" .. unum(bits(hw, 7, 0) * 4), 2
    elseif bits(hw, 15, 8) == 0xB0 then                            -- ADD/SUB SP, #imm
      return ((bits(hw, 7, 7) == 1) and "SUB" or "ADD") .. " SP, #" .. unum(bits(hw, 6, 0) * 4), 2
    elseif bits(hw, 15, 9) == 0x5A then                            -- PUSH (R=1 → LR)
      return "PUSH " .. reg_list(bits(hw, 7, 0) + ((bits(hw, 8, 8) == 1) and 16384 or 0)), 2
    elseif bits(hw, 15, 9) == 0x5E then                            -- POP (R=1 → PC)
      return "POP " .. reg_list(bits(hw, 7, 0) + ((bits(hw, 8, 8) == 1) and 32768 or 0)), 2
    end
    return string.format(".hword 0x%04X", hw), 2

  elseif h3 == 6 then
    if bits(hw, 12, 12) == 0 then                                  -- LDMIA / STMIA
      local name = (bits(hw, 11, 11) == 1) and "LDMIA" or "STMIA"
      return name .. " " .. reg(bits(hw, 10, 8)) .. "!, " .. reg_list(bits(hw, 7, 0)), 2
    end
    local c = bits(hw, 11, 8)
    if c == 15 then return string.format("SWI #0x%X", bits(hw, 7, 0)), 2 end
    if c == 14 then return string.format(".hword 0x%04X", hw), 2 end
    local off = signext(bits(hw, 7, 0), 8) * 2                     -- 조건 분기
    return "B" .. COND[c] .. " " .. hexaddr(pc + 4 + off), 2

  else                                                             -- h3 == 7
    local top5 = bits(hw, 15, 11)
    if top5 == 0x1C then                                           -- 무조건 분기
      return "B " .. hexaddr(pc + 4 + signext(bits(hw, 10, 0), 11) * 2), 2
    elseif top5 == 0x1E then                                       -- BL 상위 반워드
      local hw2 = read_byte(pc + 2) + read_byte(pc + 3) * 256
      if bits(hw2, 15, 11) == 0x1F then
        local off = signext(bits(hw, 10, 0), 11) * 4096 + bits(hw2, 10, 0) * 2
        return "BL " .. hexaddr(pc + 4 + off), 4
      end
      return string.format(".hword 0x%04X", hw), 2                 -- 짝 없는 상위 반워드
    end
    return string.format(".hword 0x%04X", hw), 2                   -- BL 하위 고아 / BLX(ARMv5)
  end
end

-- disassemble(read_byte, start, count): start에서 count개 명령. 반환 [{addr,text,bytes}].
-- 모드는 CPSR T비트로 판정한다(ARM=4바이트/Thumb=2바이트씩 전진). BL은 4바이트를 소비한다.
SYS.disassemble = function(read_byte, start, count)
  local thumb = false
  if emu and emu.getState then
    local ok, st = pcall(emu.getState)
    if ok and st then thumb = st["cpu.cpsr.thumb"] and true or false end
  end
  local out, pc = {}, start
  for _ = 1, count do
    local text, len
    if thumb then text, len = thumb_decode(read_byte, pc) else text, len = arm_decode(read_byte, pc) end
    local raw = {}
    for i = 0, len - 1 do raw[#raw + 1] = string.format("%02X", read_byte(pc + i)) end
    out[#out + 1] = { addr = string.format("0x%08X", pc), text = text, bytes = table.concat(raw, " ") }
    pc = pc + len
  end
  return out
end

-- op_is_call/op_is_return 미제공(위 헤더 참조) — call_stack은 이번 범위 밖(LR기반)이라 코어가 미구현 광고.

local dir = os.getenv("EMUCAP_ADAPTER_DIR")
if not dir or dir == "" then
  -- 폴백: env가 없으면(수동 Script Window 로드 등) 이 스크립트 파일 경로에서 어댑터 디렉터리를 도출한다.
  local src = debug.getinfo(1, "S").source
  if src and src:sub(1, 1) == "@" then dir = src:sub(2):match("^(.*)[/\\][^/\\]+$") end
end
assert(dir and dir ~= "", "emucap-gba: EMUCAP_ADAPTER_DIR 미설정 + 스크립트 경로 도출 실패 — launch로 띄우거나 파일에서 로드하라")
package.path = dir .. "/?.lua;" .. package.path
require("emucap-core")
