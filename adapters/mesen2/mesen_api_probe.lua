-- Mesen2 Lua API 심층 프로브(가벼움) — 레지스터를 싸게 감시할 길이 있나. /tmp/mesen_api2.txt.
local function probe()
  local f = io.open("/tmp/mesen_api2.txt", "w")
  if not f then return end

  f:write("=== emu.memType (CPU 레지스터 의사메모리/레지스터 공간 있나) ===\n")
  local mt = {}
  for k, v in pairs(emu.memType or {}) do mt[#mt + 1] = k .. " = " .. tostring(v) end
  table.sort(mt)
  f:write(table.concat(mt, "\n") .. "\n")

  f:write("\n=== emu.callbackType ===\n")
  for k, v in pairs(emu.callbackType or {}) do f:write(k .. " = " .. tostring(v) .. "\n") end

  f:write("\n=== emu.counterType ===\n")
  for k, v in pairs(emu.counterType or {}) do f:write(k .. " = " .. tostring(v) .. "\n") end

  f:write("\n=== getState 반환 키 전체(레지스터 외 무엇이 있나) ===\n")
  local ks = {}
  for k in pairs(emu.getState()) do ks[#ks + 1] = k end
  table.sort(ks)
  f:write("필드수=" .. #ks .. "\n" .. table.concat(ks, " ") .. "\n")

  f:close()
end

local done = false
emu.addEventCallback(function()
  if not done then done = true; probe() end
end, emu.eventType.startFrame)
