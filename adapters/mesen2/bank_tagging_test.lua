-- Mesen ROM bank tagging unit test: the real GG/GB SYS.bank_of/read_banks + the bank_for_pc guard.
-- Run: EMUCAP_ADAPTER_DIR=. lua bank_tagging_test.lua   (from adapters/mesen2/)

local function ok(cond, msg) if not cond then error("FAIL " .. msg) end end

-- Load an entry file's SYS table without pulling in emucap-core (which needs the live emu environment).
local function load_entry(path)
  local real_require = require
  _G.require = function(name) if name == "emucap-core" then return end return real_require(name) end
  _G.SYS = nil
  dofile(path)
  _G.require = real_require
  return _G.SYS
end

-- GG / SMS (Sega mapper): three 16 KB slots; first 1 KB is fixed to bank 0.
local gg = load_entry("emucap-sms.lua")
local gb0 = gg.read_banks({ ["cart.prgBanks0"] = 0, ["cart.prgBanks1"] = 5, ["cart.prgBanks2"] = 9 })
ok(gg.bank_of(0x0100, gb0) == 0, "GG first-1KB fixed bank 0")
ok(gg.bank_of(0x0400, gb0) == 0, "GG slot0 at 0x0400 -> prgBanks0")
ok(gg.bank_of(0x3FFF, gb0) == 0, "GG slot0 top -> prgBanks0")
ok(gg.bank_of(0x4000, gb0) == 5, "GG slot1 base -> prgBanks1")
ok(gg.bank_of(0x7FFF, gb0) == 5, "GG slot1 top -> prgBanks1")
ok(gg.bank_of(0x8000, gb0) == 9, "GG slot2 base -> prgBanks2")
ok(gg.bank_of(0xBFFF, gb0) == 9, "GG slot2 top -> prgBanks2")
ok(gg.bank_of(0xC000, gb0) == nil, "GG RAM 0xC000 -> nil")
ok(gg.bank_of(0xFFFF, gb0) == nil, "GG RAM top -> nil")

-- GB / GBC: 0x0000-0x3FFF fixed bank 0, 0x4000-0x7FFF switchable.
local gb = load_entry("emucap-gb.lua")
local bk = gb.read_banks({ ["cart.prgBank"] = 7, ["cart.mode"] = false })
ok(gb.bank_of(0x0000, bk) == 0, "GB low base -> bank 0 (mode 0)")
ok(gb.bank_of(0x3FFF, bk) == 0, "GB low top -> bank 0 (mode 0)")
ok(gb.bank_of(0x4000, bk) == 7, "GB switchable base -> cart.prgBank")
ok(gb.bank_of(0x7FFF, bk) == 7, "GB switchable top -> cart.prgBank")
ok(gb.bank_of(0x8000, bk) == nil, "GB VRAM -> nil")

-- GB MBC1 mode-1 / MBC1M: low region is remapped and Mesen exposes no resolved low bank, so bank_of
-- returns nil (undetermined) for 0x0000-0x3FFF instead of a wrong 0. Switchable stays correct.
local bkm = gb.read_banks({ ["cart.prgBank"] = 7, ["cart.mode"] = true })
ok(gb.bank_of(0x1000, bkm) == nil, "GB MBC1 mode-1 low region -> nil (not a wrong 0)")
ok(gb.bank_of(0x4000, bkm) == 7, "GB MBC1 mode-1 switchable still -> cart.prgBank")
ok(gb.bank_of(0x1000, gb.read_banks({ ["cart.prgBank"] = 7 })) == 0, "GB no cart.mode -> low bank 0")

-- bank_tagging_active: true only when the mapper actually exposes the bank fields (so a mapper that
-- omits them is advertised false, not a false-trustworthy true).
ok(gg.bank_tagging_active({ ["cart.prgBanks0"] = 0 }) == true, "GG fields present -> active")
ok(gg.bank_tagging_active({}) == false, "GG fields absent -> inactive")
ok(gb.bank_tagging_active({ ["cart.prgBank"] = 2 }) == true, "GB field present -> active")
ok(gb.bank_tagging_active({}) == false, "GB field absent -> inactive")

-- Guard contract (mirrors bank_for_pc in emucap-core.lua): a system with no read_banks yields nil,
-- never a crash — SNES/NES/GBA rely on this.
local function bank_for_pc(SYS, st)
  if not SYS.read_banks then return nil end
  return SYS.bank_of(st["cpu.pc"], SYS.read_banks(st))
end
ok(bank_for_pc({}, { ["cpu.pc"] = 0x8000 }) == nil, "no read_banks -> nil (no crash)")
ok(bank_for_pc(gg, { ["cpu.pc"] = 0x4000,
  ["cart.prgBanks0"] = 0, ["cart.prgBanks1"] = 3, ["cart.prgBanks2"] = 0 }) == 3,
  "bank_for_pc tags from cpu.pc")

print("ALL BANK TAGGING TESTS PASSED")
