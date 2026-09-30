-- Agent-selected host pacing over Mesen's native speed settings.
--
-- Mesen's governor is EmulationSpeed (integer percent, 0 = unlimited) unless the maximum-speed
-- toggle or a held turbo/rewind key overrides it. The patched host exposes those settings to Lua
-- (emu.getPacing/emu.setPacing) and a pacingIdle event during long frame-limiter waits. This
-- module maps that native state to the common policy and keeps an observed-change revision;
-- it performs no I/O.
local M = {}

local MIN_PERCENT = 1
local MAX_PERCENT = 10000

-- Host audio output: at exactly 100 percent Mesen adapts its sample rate to the audio device, and
-- after a speed change the device queue can hold throughput below the target for a while.
function M.host_constraints(audio, as_array)
  return as_array(audio and { "audio_output" } or {})
end

function M.capability(as_array, audio)
  return {
    modes = as_array({ "limited", "unlimited" }),
    percent = { min = MIN_PERCENT, max = MAX_PERCENT, quantum = 1 },
    states = as_array({ "running", "frozen" }),
    scope = "host_pacing",
    source = "native",
    control_service_ms = 20,
    host_constraints = M.host_constraints(audio, as_array),
  }
end

function M.new_state()
  return { revision = 0, key = nil }
end

local function key_of(p)
  return string.format("%d|%s|%s|%s", p.emulationSpeed, tostring(p.maximumSpeed),
    tostring(p.turbo), tostring(p.rewind))
end

-- Record one native observation; a changed tuple is a new revision.
function M.observe(state, pacing)
  local key = key_of(pacing)
  if key ~= state.key then
    return { revision = state.revision + 1, key = key }
  end
  return state
end

-- Held turbo or rewind replaces the target, so it is custom; maximum speed or speed 0 is unlimited.
function M.public(state, pacing, null, as_array, audio)
  local mode, percent = "custom", null
  if not (pacing.turbo or pacing.rewind) then
    if pacing.maximumSpeed or pacing.emulationSpeed == 0 then
      mode = "unlimited"
    elseif pacing.emulationSpeed >= MIN_PERCENT and pacing.emulationSpeed <= MAX_PERCENT then
      mode, percent = "limited", pacing.emulationSpeed
    end
  end
  return {
    mode = mode,
    percent = percent,
    source = "native",
    policy_revision = tostring(state.revision),
    host_constraints = M.host_constraints(audio, as_array),
    diagnostics = {
      emulation_speed = pacing.emulationSpeed,
      maximum_speed = pacing.maximumSpeed,
      turbo = pacing.turbo,
      rewind = pacing.rewind,
      effective_speed = pacing.effectiveSpeed,
    },
  }
end

-- nil request means query. Percent must be an integer inside the domain; no rounding.
function M.parse_request(params)
  local mode, percent = params.mode, params.percent
  if mode == nil and percent == nil then return "query" end
  if mode == "unlimited" and percent == nil then return { speed = 0 } end
  if mode == "limited" and type(percent) == "number" then
    if percent ~= math.floor(percent) or percent < MIN_PERCENT or percent > MAX_PERCENT then
      return nil, string.format("percent must be an integer in %d..%d", MIN_PERCENT, MAX_PERCENT)
    end
    return { speed = percent }
  end
  return nil, "use limited with percent, unlimited without percent, or omit both to query"
end

-- A set is confirmed only when the native base speed is the request and nothing overrides it.
function M.confirms(request, pacing)
  return pacing.emulationSpeed == request.speed and not pacing.maximumSpeed
    and not pacing.turbo and not pacing.rewind
end

return M
