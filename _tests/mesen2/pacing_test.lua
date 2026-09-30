local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Pacing = require("emucap_pacing")

local NULL = {}
local function as_array(t) return t end
local function eq(actual, expected, message)
  if actual ~= expected then
    error(("FAIL %s: %s ~= %s"):format(message, tostring(actual), tostring(expected)))
  end
end

local function native(speed, overrides)
  local p = { emulationSpeed = speed, maximumSpeed = false, turbo = false, rewind = false,
    turboSpeed = 300, rewindSpeed = 100, effectiveSpeed = speed }
  for k, v in pairs(overrides or {}) do p[k] = v end
  return p
end

-- Native settings map to one common policy; overrides are never reported as the base target.
do
  local state = Pacing.observe(Pacing.new_state(), native(50))
  local policy = Pacing.public(state, native(50), NULL, as_array)
  eq(policy.mode, "limited", "limited mode")
  eq(policy.percent, 50, "limited percent")
  eq(Pacing.public(state, native(0), NULL, as_array).mode, "unlimited", "speed 0")
  eq(Pacing.public(state, native(0), NULL, as_array).percent, NULL, "unlimited percent is null")
  eq(Pacing.public(state, native(100, { maximumSpeed = true }), NULL, as_array).mode,
    "unlimited", "maximum speed toggle")
  for _, override in ipairs({ { turbo = true }, { rewind = true } }) do
    local p = Pacing.public(state, native(100, override), NULL, as_array)
    eq(p.mode, "custom", "held override")
    eq(p.percent, NULL, "custom percent is null")
  end
end

-- Only an observed change advances the revision.
do
  local s1 = Pacing.observe(Pacing.new_state(), native(100))
  local s2 = Pacing.observe(s1, native(100))
  eq(s2.revision, s1.revision, "unchanged tuple keeps revision")
  local s3 = Pacing.observe(s2, native(100, { turbo = true }))
  eq(s3.revision, s1.revision + 1, "human turbo is a new revision")
end

-- Requests are explicit and integer-only; confirmation requires no active override.
do
  eq(Pacing.parse_request({}), "query", "query")
  eq(Pacing.parse_request({ mode = "unlimited" }).speed, 0, "unlimited")
  eq(Pacing.parse_request({ mode = "limited", percent = 400 }).speed, 400, "limited")
  for _, bad in ipairs({ { mode = "limited" }, { mode = "limited", percent = 33.5 },
      { mode = "limited", percent = 0 }, { mode = "limited", percent = 10001 },
      { mode = "unlimited", percent = 100 }, { percent = 50 }, { mode = "turbo" } }) do
    local request = Pacing.parse_request(bad)
    eq(request, nil, "rejected request")
  end
  eq(Pacing.confirms({ speed = 50 }, native(50)), true, "confirmed")
  eq(Pacing.confirms({ speed = 50 }, native(40)), false, "clamped")
  eq(Pacing.confirms({ speed = 50 }, native(50, { rewind = true })), false, "overridden")
end

print("ALL MESEN PACING TESTS PASSED")
