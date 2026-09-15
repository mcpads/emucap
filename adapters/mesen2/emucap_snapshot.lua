-- Atomic SNES instruction-halt capture. The caller owns the native safe-event scope.
local M = {}
M.max_bytes = 1024 * 1024

local function integer(value, maximum, name)
  if math.type(value) ~= "integer" or value < 0 or value % 1 ~= 0 or value > maximum then
    error("snapshot lacks exact " .. name)
  end
  return value
end

local function facts(host)
  local s = host.getState()
  local frame = integer(s["ppu.frameCount"], math.maxinteger, "PPU frame")
  local cycle = integer(host.getMasterClock(), math.maxinteger, "master clock")
  return {
    cpu = "main", kind = "main_cpu_instruction", boundary = "instruction_boundary",
    pc = integer(s["cpu.pc"], 65535, "PC"),
    program_bank = integer(s["cpu.k"], 255, "program bank"),
    frame = { value = string.format("%d", frame), domain = "snes_ppu_frame" },
    cycle = { value = string.format("%d", cycle), domain = "snes_master_clock" },
  }
end

function M.observe(host, safe, system)
  if not safe or system ~= "snes" then
    return false, "unsafe_halt", "snapshot observation requires a proven main-CPU instruction halt"
  end
  return true, { halt = facts(host) }
end

function M.capture(host, safe, system)
  if not safe or system ~= "snes" then
    return false, "unsafe_halt", "snapshot capture requires a proven main-CPU instruction halt"
  end
  local before = facts(host)
  local rom = host.getRomInfo()
  if type(rom.fileSha1Hash) ~= "string" or #rom.fileSha1Hash ~= 40
      or rom.fileSha1Hash:find("[^%x]") then
    error("snapshot lacks loaded artifact identity")
  end
  local data = host.createSavestate()
  if type(data) ~= "string" or #data == 0 or #data > M.max_bytes then
    error("snapshot exceeds the supported byte bound or is empty")
  end
  local after = facts(host)
  if before.pc ~= after.pc or before.program_bank ~= after.program_bank
      or before.frame.value ~= after.frame.value or before.cycle.value ~= after.cycle.value then
    error("serialization changed the instruction halt")
  end
  local encoded = data:gsub(".", function(c) return string.format("%02x", c:byte()) end)
  return true, { state = "frozen", boundary = "instruction_boundary", halt = before,
    format = "mesen-savestate", hex = encoded, loaded_artifact_sha1 = rom.fileSha1Hash:lower() }
end

return M
