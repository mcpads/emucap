local repo_root = arg[1] or "."
local plugin = dofile(repo_root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua")

-- MAME emits %X for all three point kinds. Check the first alphabetic ID and
-- an all-digit hex ID that decimal parsing silently attributes to another point.
for _, point in ipairs({
  { "Breakpoint", "breakpoint", "bp" },
  { "Watchpoint", "watchpoint", "wp" },
  { "Registerpoint", "registerpoint", "rp" },
}) do
  for _, case in ipairs({ { "9", 9 }, { "A", 10 }, { "F", 15 }, { "10", 16 }, { "1A", 26 } }) do
    assert(plugin.debugger_point_index(point[1] .. " " .. case[1] .. " set", point[1]) == case[2])
    assert(plugin.debugger_point_index("Stopped at " .. point[2] .. " " .. case[1],
      "Stopped at " .. point[2]) == case[2])
    assert(plugin.debugger_point_clear_command(point[3], case[2]) == point[3] .. "clear " .. case[1])
  end
end
assert(plugin.debugger_point_index("Breakpoint unavailable", "Breakpoint") == nil)
assert(plugin.debugger_point_index("Breakpoint 1G set", "Breakpoint") == nil)
assert(plugin.debugger_point_index("Stopped at watchpoint A reading 00 from 0010", "Stopped at watchpoint") == 10)
print("PC-98 hexadecimal debugger point IDs passed")
