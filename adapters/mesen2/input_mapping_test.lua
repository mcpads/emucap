-- emucap-live.lua input button normalization unit test. `lua input_mapping_test.lua`.

local VALID_BUTTONS = {
  a = true, b = true, x = true, y = true, l = true, r = true,
  start = true, select = true, up = true, down = true, left = true, right = true,
}
local BUTTON_ALIASES = {
  enter = "start", ["return"] = "start",
  l1 = "l", r1 = "r", lb = "l", rb = "r",
}
local function buttons_to_table(buttons)
  local t = {}
  local unknown = {}
  for _, b in ipairs(buttons or {}) do
    local raw = tostring(b)
    local lb = raw:lower()
    lb = BUTTON_ALIASES[lb] or lb
    if VALID_BUTTONS[lb] then
      t[lb] = true
    else
      unknown[#unknown + 1] = raw
    end
  end
  if #unknown > 0 then
    return nil, "unknown SNES button(s): " .. table.concat(unknown, ", ")
  end
  return t
end

local function ok(cond, msg)
  if not cond then error("FAIL " .. msg) end
end

local mapped = assert(buttons_to_table({ "A", "enter", "L1", "rb" }))
ok(mapped.a == true, "A maps to a")
ok(mapped.start == true, "enter maps to start")
ok(mapped.l == true, "L1 maps to l")
ok(mapped.r == true, "rb maps to r")

local t, err = buttons_to_table({ "coin" })
ok(t == nil, "unknown button is rejected")
ok(err:match("coin"), "unknown button name appears in error")

print("ALL INPUT MAPPING TESTS PASSED")
