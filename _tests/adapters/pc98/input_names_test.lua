local repo_root = arg[1] or "."
local plugin = dofile(repo_root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua")

local cases = {
  { "0", "0" },
  { "0 (pad)", "kp0" },
  { "kp0", "kp0" },
  { "kp_5", "kp5" },
  { "numpad9", "kp9" },
}

for _, case in ipairs(cases) do
  local actual = plugin.canonical_input_name(case[1])
  assert(actual == case[2], string.format("%s normalized to %s, expected %s", case[1], actual, case[2]))
end

print("PC-98 input name checks passed")
