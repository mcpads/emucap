local adapter_dir = os.getenv("EMUCAP_ADAPTER_DIR") or "adapters/mesen2"
local Recording = dofile(adapter_dir .. "/emucap_recording.lua")

local CONTRACT = "498fcd52f2fa2327e0af9e9730b4314f0854a6047f57dcde16961b8a4ecb80cd"
local COMPLETED_CONTRACT = "a335a785a0c109cc7edc6ecab27ff429e386c2ad2eb34769cac4f9cc47378b91"
local OBJ_EVALUATION_CONTRACT = "0d32bfc67347b3169fd77f9d30beb9c325c64db30f0081c628ba85646ebd763b"
local OBJ_HANDOFF_CONTRACT = "ad23c438ee6400f5f9cab84d877f490abe24670769e50efd2bf67d932d329bbc"
local CPU_INSTRUCTION_CONTRACT = "f936fa1f0509851d3394edf3e3f7d6db0e40dd4310531f2ae73ac4ba81c55af0"
local OBJ_CONSUMPTION_CONTRACT = "8969bf826c9b56b41a52266e8ba8453868e48b5ac3486f8b0cf499eb90cf0e2d"
local CGRAM_LOOKUP_CONTRACT = "f9f507926817ef3d14de8ca4cfbfd05364afd78842cca7b04aaeffe094960795"
local BG_CHR_FETCH_CONTRACT = "ee9aecc8a9aa130ee871e08785ea9f32de14172260c8993d0baa33f4f07fc68c"
local REVISION = "c7bc749b13517456b049a73a868bc662c54cc98e580cc306b873328fd842dc22"
local LAUNCH = "launch-01test"

local function equal(actual, expected, label)
  if actual ~= expected then
    error(string.format("%s: expected %s, got %s", label, tostring(expected), tostring(actual)), 2)
  end
end

local function contains(value, pattern, label)
  if not tostring(value):find(pattern, 1, true) then
    error(string.format("%s: %q does not contain %q", label, tostring(value), pattern), 2)
  end
end

local function fake_sink(options)
  options = options or {}
  local sink = { chunks = {}, close_count = 0, writes = 0 }
  sink.write = function(line)
    sink.writes = sink.writes + 1
    if options.error_at == sink.writes then return nil, "injected write error", 0 end
    if options.partial_at == sink.writes then
      local sent = math.min(options.partial_bytes or 5, #line - 1)
      sink.chunks[#sink.chunks + 1] = line:sub(1, sent)
      return nil, "timeout", sent
    end
    sink.chunks[#sink.chunks + 1] = line
    if options.order then
      options.order[#options.order + 1] = line:find('"class":"frame_completed"', 1, true)
        and "event:completed" or "event:boundary"
    end
    return #line
  end
  sink.close = function()
    sink.close_count = sink.close_count + 1
    if options.close_error then return nil, "injected close error" end
    return true
  end
  return sink
end

local function params(frames, overrides)
  local p = {
    capture_id = "capture-test",
    launch_id = LAUNCH,
    request_digest_sha256 = string.rep("a", 64),
    capability_revision = REVISION,
    origin = "next_frame_boundary",
    frames = frames,
    warmup_frames = 0,
    event_classes = { { id = "frame_boundary", contract_sha256 = CONTRACT } },
    limits = {
      max_frames = frames,
      max_events = 100,
      max_bytes = 1024 * 1024,
      max_line_bytes = 4096,
      max_host_ms = 2000,
      progress_interval_ms = 100,
    },
  }
  for key, value in pairs(overrides or {}) do p[key] = value end
  if (p.warmup_frames > 0 or p.start_on ~= nil) and p.event_arming == nil then
    p.event_arming = {}
    for _, class in ipairs(p.event_classes) do
      p.event_arming[#p.event_arming + 1] = {
        id = class.id,
        scope = (class.id == "frame_boundary" or class.id == "frame_completed")
          and "transaction" or "observation",
      }
    end
  end
  return p
end

do
  Recording.capability(function(value) return value end, true, false, false, true)
  local valid = params(1, { capture_id = "capture-test" })
  local state, code, message = Recording.validate(valid, LAUNCH, 0, 0)
  assert(state, tostring(code) .. ": " .. tostring(message))

  for _, capture_id in ipairs({ "capture_test", "-capture", "capture-", "capture--test" }) do
    local invalid = params(1, { capture_id = capture_id })
    local rejected, rejected_code, rejected_message = Recording.validate(invalid, LAUNCH, 0, 0)
    equal(rejected, nil, "unsafe capture ID rejection")
    equal(rejected_code, "bad_params", "unsafe capture ID code")
    equal(rejected_message, "capture_id is invalid", "unsafe capture ID message")
  end
end

local function decode_buttons(buttons)
  local decoded = {}
  for _, button in ipairs(buttons) do
    if button == "unknown" then return nil, "unknown test button" end
    decoded[button] = true
  end
  return decoded
end

local function movie_file(text)
  local path = os.tmpname()
  local file = assert(io.open(path, "wb"))
  assert(file:write(text))
  assert(file:close())
  return path
end

local function obj_consumption_payload(overrides)
  local value = {
    memory_kind = 1,
    address = 0x2000,
    value = 0x34,
    scanline = 16,
    dot = 128,
    hclock = 512,
  }
  for key, item in pairs(overrides or {}) do value[key] = item end
  return value
end

local function cgram_lookup_payload(overrides)
  local value = {
    address = 0x80,
    value = 0x1234,
    layer = 4,
    target = 1,
    pixel_x = 72,
    scanline = 40,
    dot = 94,
    hclock = 376,
  }
  for key, item in pairs(overrides or {}) do value[key] = item end
  return value
end

local function bg_chr_fetch_payload(overrides)
  local value = {
    address = 0x2400,
    value = 0x1234,
    layer = 1,
    scanline = 40,
    dot = 94,
    hclock = 376,
  }
  for key, item in pairs(overrides or {}) do value[key] = item end
  return value
end

do
  local capability = Recording.capability(function(value) return value end, true, false, true, true)
  equal(capability.revision,
    "9cb6540758c6f4a690371afc92c52803597183466ab0fc3486cbabc9e4287840",
    "deep capability revision")
  equal(capability.event_order, "guest_emission", "cross-class event order")
  equal(#capability.event_classes, 13, "deep event class count")
  equal(capability.event_classes[5].id, "snes_cpu_instruction", "first deep class")
  equal(capability.event_classes[5].contract_sha256, CPU_INSTRUCTION_CONTRACT,
    "instruction contract")
  equal(capability.event_classes[5].startable, true, "instruction startability")
  equal(capability.initial_snapshots.memory_types[1], "snesWorkRam",
    "callback-safe initial snapshot memory")
  equal(capability.initial_snapshots.max_members, 1, "initial snapshot member bound")
  equal(capability.initial_snapshots.max_callback_ms, 100, "initial snapshot callback bound")
  equal(capability.event_classes[11].id, "snes_ppu_obj_consumption_read",
    "consumption-read class")
  equal(capability.event_classes[11].stoppable, true,
    "consumption-read stoppability")
  equal(capability.event_classes[11].filterable_fields[1].path, "memory_kind",
    "consumption memory-kind filter")
  equal(capability.event_classes[11].filterable_fields[2].path, "address",
    "consumption address filter")
  equal(capability.event_classes[12].id, "snes_ppu_cgram_lookup", "CGRAM lookup class")
  equal(capability.event_classes[12].contract_sha256, CGRAM_LOOKUP_CONTRACT,
    "CGRAM lookup contract")
  equal(capability.event_classes[12].stoppable, true, "CGRAM lookup stoppability")
  equal(capability.event_classes[12].filterable_fields[1].path, "address",
    "CGRAM address filter")
  equal(capability.event_classes[12].filterable_fields[2].path, "layer",
    "CGRAM layer filter")
  equal(capability.event_classes[12].filterable_fields[3].path, "target",
    "CGRAM renderer-target filter")
  equal(capability.event_classes[13].id, "snes_ppu_bg_chr_fetch", "BG CHR fetch class")
  equal(capability.event_classes[13].contract_sha256, BG_CHR_FETCH_CONTRACT,
    "BG CHR fetch contract")
  equal(capability.event_classes[13].stoppable, true, "BG CHR fetch stoppability")
  equal(capability.event_classes[13].filterable_fields[1].path, "address",
    "BG CHR VRAM-word filter")
  equal(capability.event_classes[13].filterable_fields[2].path, "layer",
    "BG CHR layer filter")
  equal(capability.event_classes[13].filterable_fields[3].path, "scanline",
    "BG CHR scanline filter")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local conditions = "b9f4760915a13576fe4fa5c55a75dffd0e79987ac6259cea1bff5a1701826d6b"
  local capability = Recording.capability(
    function(value) return value end, true, true, true, true, conditions)
  equal(capability.revision,
    "4436231189dd28b27f252d8c1241ccdfc04ead72aca5b1f675c53ad5a6377511",
    "repeatable capability revision")
  equal(capability.repeatability.profile, "mesen_snes_repeatable",
    "repeatable profile identity")
  equal(capability.repeatability.conditions_sha256, conditions,
    "repeatable condition identity")
  equal(#capability.repeatability.origins, 1, "repeatable origin count")
  equal(capability.repeatability.origins[1], "reset_release", "repeatable origin")
  equal(capability.repeatability.requires_input_movie, true,
    "repeatable input movie requirement")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local deep = Recording.capability(function(value) return value end, true, false, true, true)
  local p = params(2, {
    capability_revision = deep.revision,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_ppu_cgram_lookup", contract_sha256 = CGRAM_LOOKUP_CONTRACT },
    },
    event_filters = { {
      event_class = "snes_ppu_cgram_lookup",
      terms = {
        { kind = "u64_range", path = "address", start = 0x80, length = 1 },
        { kind = "u64_range", path = "layer", start = 4, length = 1 },
        { kind = "u64_range", path = "target", start = 1, length = 1 },
      },
    } },
    stop_on = { event_class = "snes_ppu_cgram_lookup", occurrence = 2 },
  })
  local sink = fake_sink()
  local state = assert(Recording.start(
    p, 42, LAUNCH, 100, 0, sink, nil, nil, nil, nil, function() return true end))
  assert(Recording.attach_hooks(state))
  state = select(1, Recording.semantic_event(
    state, "snes_ppu_cgram_lookup", 100, 2000, cgram_lookup_payload({ address = 0x7f })))
  local effect
  state, effect = Recording.semantic_event(
    state, "snes_ppu_cgram_lookup", 100, 2001, cgram_lookup_payload({ target = 2 }))
  equal(effect, nil, "sub-screen CGRAM lookup is outside the main-screen filter")
  state, effect = Recording.semantic_event(
    state, "snes_ppu_cgram_lookup", 100, 2002, cgram_lookup_payload())
  equal(effect, nil, "first filtered CGRAM lookup continues")
  state, effect = Recording.semantic_event(
    state, "snes_ppu_cgram_lookup", 100, 2003, cgram_lookup_payload({ pixel_x = 73 }))
  equal(effect.kind, "terminal", "second filtered CGRAM lookup stops")
  local result = Recording.result(state, 1)
  equal(result.execution_outcome, "event_stop", "CGRAM event-stop outcome")
  equal(result.final_frame, 100, "CGRAM partial-frame terminal coordinate")
  equal(result.f_end, 101, "CGRAM partial-frame scope")
  equal(result.stop_event.event_class, "snes_ppu_cgram_lookup", "CGRAM stop class")
  equal(result.stop_event.occurrence, 2, "CGRAM filtered occurrence")
  equal(result.event_classes[2].observed, 2, "CGRAM class accounting")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local deep = Recording.capability(function(value) return value end, true, false, true, true)
  local p = params(2, {
    capability_revision = deep.revision,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_ppu_bg_chr_fetch", contract_sha256 = BG_CHR_FETCH_CONTRACT },
    },
    event_filters = { {
      event_class = "snes_ppu_bg_chr_fetch",
      terms = {
        { kind = "u64_range", path = "address", start = 0x2400, length = 0x20 },
        { kind = "u64_range", path = "layer", start = 1, length = 1 },
        { kind = "u64_range", path = "scanline", start = 40, length = 1 },
      },
    } },
    stop_on = { event_class = "snes_ppu_bg_chr_fetch", occurrence = 2 },
  })
  local sink = fake_sink()
  local state = assert(Recording.start(
    p, 43, LAUNCH, 100, 0, sink, nil, nil, nil, nil, function() return true end))
  assert(Recording.attach_hooks(state))
  state = select(1, Recording.semantic_event(
    state, "snes_ppu_bg_chr_fetch", 100, 3000,
    bg_chr_fetch_payload({ address = 0x23ff })))
  local effect
  state, effect = Recording.semantic_event(
    state, "snes_ppu_bg_chr_fetch", 100, 3001, bg_chr_fetch_payload({ layer = 0 }))
  equal(effect, nil, "other BG layer is outside the filter")
  state, effect = Recording.semantic_event(
    state, "snes_ppu_bg_chr_fetch", 100, 3002, bg_chr_fetch_payload())
  equal(effect, nil, "first filtered BG fetch continues")
  state, effect = Recording.semantic_event(
    state, "snes_ppu_bg_chr_fetch", 100, 3003,
    bg_chr_fetch_payload({ address = 0x241f }))
  equal(effect.kind, "terminal", "second filtered BG fetch stops")
  local result = Recording.result(state, 1)
  equal(result.execution_outcome, "event_stop", "BG fetch event-stop outcome")
  equal(result.integrity, "complete", "BG fetch event-stop integrity")
  equal(result.stop_event.event_class, "snes_ppu_bg_chr_fetch", "BG fetch stop class")
  equal(result.stop_event.occurrence, 2, "BG fetch filtered occurrence")
  equal(result.event_classes[2].observed, 2, "BG fetch class accounting")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local deep = Recording.capability(function(value) return value end, true, false, true, true)
  local p = params(1, {
    capability_revision = deep.revision,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_cpu_instruction", contract_sha256 = CPU_INSTRUCTION_CONTRACT },
    },
    start_on = { event_class = "snes_cpu_instruction" },
    initial_snapshots = {
      { label = "wram", memory_type = "snesWorkRam", address = 0, length = 4 },
    },
  })
  local member = fake_sink()
  local now = 0
  local capture_calls = 0
  local function capture(state)
    capture_calls = capture_calls + 1
    assert(state.member_sink.write('{"label":"wram","bytes":4}\n'))
    assert(state.member_sink.write("ABCD"))
    now = now + 10
    return true
  end
  local event_sink = fake_sink()
  local state, effect = Recording.start(
    p, 101, LAUNCH, 90, 0, event_sink, nil, nil, nil, nil,
    function() return true end, member, capture, function() return now end)
  equal(effect.kind, "arm_observation", "event-aligned hook arm")
  equal(Recording.attach_hooks(state), true, "event-aligned hooks attached")
  equal(Recording.progress(state).phase, "aligning", "alignment progress phase")
  local cpu = { pc = 0x808000, opcode = 0xea, a = 1, x = 2, y = 3, sp = 0x1ff,
    d = 0, dbr = 0x7e, k = 0x80, ps = 0x34, emulation = false, cpu_cycle = 1234 }
  state, effect = Recording.semantic_event(state, "snes_cpu_instruction", 90, 4567, cpu)
  equal(effect, nil, "anchor event accepted")
  equal(capture_calls, 1, "initial snapshot captured once")
  equal(member.close_count, 1, "initial snapshot sink closes at anchor")
  equal(state.observation_start.sequence, 1, "anchor sequence follows transaction boundary")
  equal(state.observation_start.contract_sha256, CPU_INSTRUCTION_CONTRACT,
    "anchor contract identity")
  equal(Recording.progress(state).phase, "recording", "post-anchor progress phase")
  state, effect = Recording.tick(state, 91, 20)
  equal(effect.kind, "terminal", "event-aligned recording terminal")
  local result = Recording.result(state, 20)
  equal(result.observation_start.clock_tick, 4567, "terminal anchor clock")
  equal(result.observation_start.occurrence, nil, "terminal anchor keeps the closed wire shape")
  equal(#result.event_classes, 2, "terminal accounting covers every selected class")
  equal(result.event_classes[1].observed, 1, "aligned transaction-class accounting")
  equal(result.event_classes[2].observed, 1, "aligned observation-class accounting")
  equal(result.cleanup.sink, "released", "both sinks released")

  local failed_member = fake_sink()
  local failed_event_sink = fake_sink()
  local failed, failed_effect = Recording.start(
    p, 102, LAUNCH, 100, 0, failed_event_sink, nil, nil, nil, nil,
    function() return true end, failed_member,
    function() return nil, "injected member write failure" end,
    function() return 0 end)
  equal(failed_effect.kind, "arm_observation", "failure case hook arm")
  assert(Recording.attach_hooks(failed))
  failed, failed_effect = Recording.semantic_event(
    failed, "snes_cpu_instruction", 100, 5000, cpu)
  equal(failed_effect.kind, "terminal", "initial snapshot failure fail-stops")
  local failed_result = Recording.result(failed, 0)
  equal(failed_result.operation_outcome, "failed", "initial snapshot failure outcome")
  equal(failed_result.integrity, "unverifiable", "initial snapshot failure integrity")
  equal(failed_result.observation_start, nil, "failed anchor is not admitted")
  equal(failed_result.cleanup.hooks, "released", "failure removes hooks")
  equal(failed_result.cleanup.sink, "released", "failure closes both sinks")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local deep = Recording.capability(function(value) return value end, true, false, true, true)
  local p = params(2, {
    capability_revision = deep.revision,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "frame_completed", contract_sha256 = COMPLETED_CONTRACT },
      { id = "snes_ppu_obj_consumption_read", contract_sha256 = OBJ_CONSUMPTION_CONTRACT },
    },
    event_filters = { {
      event_class = "snes_ppu_obj_consumption_read",
      terms = {
        { kind = "u64_range", path = "address", start = 0x2000, length = 0x100 },
      },
    } },
    stop_on = { event_class = "snes_ppu_obj_consumption_read", occurrence = 2 },
  })
  local sink = fake_sink()
  local state = assert(Recording.start(
    p, 40, LAUNCH, 100, 0, sink, nil, nil, nil, nil, function() return true end))
  assert(Recording.attach_hooks(state))
  state = select(1, Recording.semantic_event(
    state, "snes_ppu_obj_consumption_read", 100, 1000,
    obj_consumption_payload({ address = 0x1fff })))
  local effect
  state, effect = Recording.semantic_event(
    state, "snes_ppu_obj_consumption_read", 100, 1001, obj_consumption_payload())
  equal(effect, nil, "first filtered occurrence continues")
  state, effect = Recording.semantic_event(
    state, "snes_ppu_obj_consumption_read", 100, 1002,
    obj_consumption_payload({ address = 0x20ff }))
  equal(effect.kind, "terminal", "second filtered occurrence stops")
  local result = Recording.result(state, 1)
  equal(result.execution_outcome, "event_stop", "filtered event-stop outcome")
  equal(result.integrity, "complete", "filtered event-stop integrity")
  equal(result.final_frame, 100, "partial-frame terminal coordinate")
  equal(result.f_end, 101, "partial-frame closed scope")
  equal(result.frames, 1, "partial-frame scope count")
  equal(result.events, 3, "only persisted filtered events count")
  equal(result.stop_event.sequence, 2, "stop event is final stream record")
  equal(result.stop_event.clock_domain, "snes_master", "stop clock domain is preserved")
  equal(result.stop_event.clock_tick, 1002, "stop clock tick is preserved")
  equal(result.stop_event.occurrence, 2, "filtered occurrence is preserved")
  equal(result.stop_event.contract_sha256, nil, "terminal stop facts use the public wire shape")
  equal(result.event_classes[2].observed, 0, "partial frame has no completion")
  equal(#sink.chunks, 3, "no record follows the stop event")

  local rejected, rejected_effect, kind = Recording.start(params(1, {
    capability_revision = deep.revision,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_cpu_instruction", contract_sha256 = CPU_INSTRUCTION_CONTRACT },
    },
    stop_on = { event_class = "snes_cpu_instruction", occurrence = 1 },
  }), 41, LAUNCH, 100, 0, fake_sink())
  equal(rejected, nil, "non-stoppable class rejects before arm")
  equal(rejected_effect, nil, "non-stoppable class has no guest effect")
  equal(kind, "bad_params", "non-stoppable class rejection kind")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local capability = Recording.capability(function(value) return value end, true, false, false, true)
  equal(capability.revision, REVISION, "capability revision")
  equal(capability.event_classes[1].contract_sha256, CONTRACT, "capability contract")
  equal(capability.event_classes[2].contract_sha256, COMPLETED_CONTRACT, "completion contract")
  equal(capability.event_classes[2].stoppable, true, "completion stoppability")
  equal(capability.event_classes[3].contract_sha256, OBJ_EVALUATION_CONTRACT,
    "OBJ evaluation contract")
  equal(capability.event_classes[4].contract_sha256, OBJ_HANDOFF_CONTRACT,
    "OBJ handoff contract")
  equal(capability.class_accounting, true, "per-class terminal accounting")
  equal(capability.warmup.selectable_event_scopes[1].id, "frame_boundary",
    "boundary scope selection")
  equal(capability.warmup.selectable_event_scopes[1].scopes[2], "observation",
    "observation scope selection")
  equal(capability.input_movie.format, "frame-full-state-1", "movie format")
  equal(capability.origins[1], "next_frame_boundary", "Mesen origin")
  equal(capability.origins[2], "reset_release", "Mesen reset origin")
  equal(capability.origins[3], "state_load", "Mesen state origin")
  equal(capability.state_load.format, "mesen-savestate", "state format")
  equal(capability.state_load.alignment, "restored_frame_boundary", "state alignment")
  equal(capability.state_load.requires_input_movie, true, "state movie requirement")
  equal(capability.limits.max_frames, 5000, "capability frame bound")
  equal(capability.input_movie.max_frames, 5000, "movie frame bound")
  equal(capability.limits.max_host_ms, 250000, "capability host deadline")
  equal(capability.terminal_state.max_bytes, 128 * 1024, "terminal state byte bound")
  equal(capability.terminal_state.profiles[1].id, "snes_ppu", "terminal state profile")
  equal(capability.terminal_state.profiles[1].contract_sha256,
    "21005a15437abd767cbeda5c7ede8741e2aeac4a006dafedede03a695377eaa2",
    "terminal state contract")
  equal(capability.terminal_state.profiles[1].groups[1], "ppu", "terminal state group")
end

do
  local capability = Recording.capability(function(value) return value end, false, false, false, false)
  equal(#capability.event_classes, 2, "non-SNES runtime omits SNES classes")
  equal(capability.revision,
    "20520b327e06f8ed30387f20f8609b861ceb4306ac1d955f6fb7a38b7489e885",
    "base capability revision")
  equal(capability.terminal_state, nil, "non-SNES runtime omits terminal state profiles")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local capability = Recording.capability(function(value) return value end, false, false, false, true)
  equal(capability.revision,
    "89000c4b91750e4ad8317eb234ccc3e75a6c304978feb74daaedda8f2aa3ba7e",
    "SNES state-only capability revision")
  equal(capability.terminal_state.profiles[1].id, "snes_ppu",
    "terminal state is independent of semantic hooks")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local text = "0:\n1:\n"
  local path = movie_file(text)
  local p = params(2, {
    origin = "state_load",
    initial_state = {
      path = "/managed/initial.state",
      format = "mesen-savestate",
      bytes = 11,
      sha256 = string.rep("d", 64),
      frame = 120,
      boundary = "frame_boundary",
    },
    input_movie = {
      path = path,
      format = "frame-full-state-1",
      port = 0,
      frames = 2,
      bytes = #text,
      sha256 = string.rep("e", 64),
    },
  })
  local installed, released = 0, false
  local sink = fake_sink()
  local state, effect = Recording.start(
    p, 45, LAUNCH, 120, 0, sink, decode_buttons,
    function() installed = installed + 1; return true end,
    function() released = true; return true end)
  equal(effect.kind, "arm_observation", "restored boundary arms atomically")
  equal(state.start_frame, 120, "restored frame coordinate")
  equal(sink.writes, 1, "restored boundary is the first event")
  equal(installed, 1, "movie offset zero is owned before resume")
  assert(Recording.attach_hooks(state, false))
  state = select(1, Recording.tick(state, 121, 1))
  equal(installed, 2, "second frame input is installed at its boundary")
  state, effect = Recording.tick(state, 122, 2)
  equal(effect.kind, "terminal", "state-backed window reaches exact end")
  local result = Recording.result(state, 2)
  equal(result.f_start, 120, "state-backed start")
  equal(result.f_end, 122, "state-backed end")
  equal(result.frames, 2, "state-backed frame count")
  equal(result.events, 2, "state-backed boundary count")
  equal(released, true, "state-backed input is released at terminal")

  local second_sink = fake_sink()
  local second = assert(Recording.start(
    p, 46, LAUNCH, 120, 0, second_sink, decode_buttons,
    function() return true end, function() return true end))
  assert(Recording.attach_hooks(second, false))
  second = select(1, Recording.tick(second, 121, 1))
  second = select(1, Recording.tick(second, 122, 2))
  equal(table.concat(second_sink.chunks), table.concat(sink.chunks),
    "one producer receipt yields the same bounded boundary stream twice")

  local failed_input_state, failed_input_effect = Recording.start(
    p, 47, LAUNCH, 120, 0, fake_sink(), decode_buttons,
    function() return false, "injected install failure" end,
    function() return true end)
  equal(failed_input_effect.kind, "terminal", "state input failure is terminal")
  local failed_input_result = Recording.result(failed_input_state, 0)
  equal(failed_input_result.status, "failed", "state input failure status")
  equal(failed_input_result.integrity, "unverifiable", "state input failure integrity")
  equal(failed_input_result.final_execution_state, "frozen", "state input failure freezes")
  equal(failed_input_result.cleanup.transient_input, "not_acquired",
    "state input failure reports no transient ownership")
  os.remove(path)
end

local function obj_payload(overrides)
  local value = {
    cpu = { pc = 0x808000, a = 1, x = 2, y = 3, sp = 0x1ff,
      d = 0, dbr = 0x7e, k = 0x80, ps = 0x34 },
    ppu = { scanline = 12, dot = 44, hclock = 176 },
    forced_blank = false,
  }
  for key, item in pairs(overrides or {}) do value[key] = item end
  return value
end

do
  local deep = Recording.capability(function(value) return value end, true, false, true, true)
  local p = params(1, {
    capability_revision = deep.revision,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_ppu_obj_consumption_read", contract_sha256 = OBJ_CONSUMPTION_CONTRACT },
    },
    event_filters = { {
      event_class = "snes_ppu_obj_consumption_read",
      terms = {
        { kind = "u64_range", path = "address", start = 0x2000, length = 0x100 },
        { kind = "u64_range", path = "memory_kind", start = 1, length = 1 },
      },
    } },
  })
  local sink = fake_sink()
  local state = assert(Recording.start(
    p, 37, LAUNCH, 100, 0, sink, nil, nil, nil, nil, function() return true end))
  assert(Recording.attach_hooks(state))
  local effect
  state, effect = Recording.semantic_event(
    state, "snes_ppu_obj_consumption_read", 100, 1000,
    obj_consumption_payload({ address = 0x1fff }))
  equal(effect, nil, "out-of-scope event is ignored")
  state, effect = Recording.semantic_event(
    state, "snes_ppu_obj_consumption_read", 100, 1001,
    obj_consumption_payload({ address = 0x20ff }))
  equal(effect, nil, "in-scope event is persisted")
  state, effect = Recording.tick(state, 101, 1)
  equal(effect.kind, "terminal", "filtered recording terminal")
  local result = Recording.result(state, 1)
  equal(result.events, 2, "filter excludes events before sequence accounting")
  equal(result.event_classes[2].observed, 1, "filter counts only matching events")
  equal(result.event_classes[2].dropped, 0, "filter exclusion is not producer loss")

  p.event_filters[1].terms[1].start = 0xffff
  p.event_filters[1].terms[1].length = 2
  local rejected, rejected_effect, kind = Recording.start(p, 38, LAUNCH, 100, 0, fake_sink())
  equal(rejected, nil, "out-of-domain filter rejected before mutation")
  equal(rejected_effect, nil, "out-of-domain filter has no effect")
  equal(kind, "bad_params", "out-of-domain filter rejection kind")
  p.event_filters = { named = p.event_filters[1] }
  rejected, rejected_effect, kind = Recording.start(p, 39, LAUNCH, 100, 0, fake_sink())
  equal(rejected, nil, "object-shaped filter collection rejected before mutation")
  equal(rejected_effect, nil, "object-shaped filter collection has no effect")
  equal(kind, "bad_params", "object-shaped filter collection rejection kind")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local p = params(1, {
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "frame_completed", contract_sha256 = COMPLETED_CONTRACT },
      { id = "snes_ppu_obj_handoff", contract_sha256 = OBJ_HANDOFF_CONTRACT },
    },
  })
  local state = assert(Recording.start(
    p, 35, LAUNCH, 80, 0, fake_sink(), nil, nil, nil, nil, function() return true end))
  assert(Recording.attach_hooks(state))
  state = Recording.semantic_event(state, "snes_ppu_obj_handoff", 80, 3000, obj_payload())
  local effect
  state, effect = Recording.tick(state, 81, 100)
  equal(effect.kind, "terminal", "semantic events do not consume frame-completed occurrences")
  local result = Recording.result(state, 100)
  equal(result.operation_outcome, "completed", "mixed event stream completes")
  equal(result.event_classes[2].observed, 1, "frame-completed accounting is independent")
  equal(result.event_classes[3].observed, 1, "semantic accounting is independent")
end

do
  local p = params(1, {
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_ppu_obj_handoff", contract_sha256 = OBJ_HANDOFF_CONTRACT },
    },
    limits = {
      max_frames = 1,
      max_events = 1000,
      max_bytes = 1024 * 1024,
      max_line_bytes = 4096,
      max_host_ms = 2000,
      progress_interval_ms = 100,
    },
  })
  local hooks_released = 0
  local sink = fake_sink()
  local state, effect = Recording.start(
    p, 31, LAUNCH, 40, 0, sink, nil, nil, nil, nil,
    function() hooks_released = hooks_released + 1; return true end)
  equal(effect.kind, "arm_observation", "semantic recording arm")
  equal(Recording.attach_hooks(state), true, "semantic hooks attached")
  for index = 1, 300 do
    state, effect = Recording.semantic_event(
      state, "snes_ppu_obj_handoff", 40, 1000 + index, obj_payload())
    equal(effect, nil, "semantic event accepted " .. index)
  end
  state, effect = Recording.tick(state, 41, 4000)
  equal(effect.kind, "terminal", "semantic recording terminal")
  local result = Recording.result(state, 4000)
  equal(result.events, 301, "semantic records bypass live queue ceiling")
  equal(result.event_classes[1].observed, 1, "frame class accounting")
  equal(result.event_classes[2].observed, 300, "semantic class accounting")
  equal(result.event_classes[2].armed, true, "semantic class armed accounting")
  equal(result.event_classes[2].dropped, 0, "semantic class loss accounting")
  equal(result.cleanup.hooks, "released", "semantic hook cleanup")
  equal(hooks_released, 1, "semantic hooks released once")
end


do
  local p = params(1, {
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_ppu_obj_handoff", contract_sha256 = OBJ_HANDOFF_CONTRACT },
    },
  })
  local state = assert(Recording.start(p, 34, LAUNCH, 70, 0, fake_sink()))
  local result = Recording.result(state, 0)
  equal(result.event_classes[1].armed, true, "frame class arms at sink boundary")
  equal(result.event_classes[2].armed, false, "semantic class is not armed before hook install")
end

do
  local p = params(1, {
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_ppu_obj_evaluation_start", contract_sha256 = OBJ_EVALUATION_CONTRACT },
    },
  })
  local hooks_released = 0
  local sink = fake_sink({ error_at = 2 })
  local state = assert(Recording.start(
    p, 32, LAUNCH, 50, 0, sink, nil, nil, nil, nil,
    function() hooks_released = hooks_released + 1; return true end))
  assert(Recording.attach_hooks(state))
  local effect
  state, effect = Recording.semantic_event(
    state, "snes_ppu_obj_evaluation_start", 50, 2000, obj_payload())
  equal(effect.kind, "terminal", "semantic sink failure is fail-stop")
  equal(Recording.result(state, 1).integrity, "unverifiable", "semantic sink failure integrity")
  equal(hooks_released, 1, "sink failure releases semantic hooks")
end

do
  local p = params(1, {
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_ppu_obj_handoff", contract_sha256 = OBJ_HANDOFF_CONTRACT },
    },
  })
  local state = assert(Recording.start(p, 33, LAUNCH, 60, 0, fake_sink()))
  assert(Recording.attach_hooks(state, false))
  local invalid = obj_payload()
  invalid.cpu.pc = 0x1000000
  local effect
  state, effect = Recording.semantic_event(state, "snes_ppu_obj_handoff", 60, 3000, invalid)
  equal(effect.kind, "terminal", "invalid semantic payload fails before write")
  contains(Recording.result(state, 1).reason, "event_payload_contract_failed",
    "invalid semantic payload reason")
end

do
  local text = "0:a\n1:b\n"
  local path = movie_file(text)
  local p = params(2, {
    origin = "reset_release",
    input_movie = {
      path = path,
      format = "frame-full-state-1",
      port = 0,
      frames = 2,
      bytes = #text,
      sha256 = string.rep("7", 64),
    },
  })
  local prepared, kind, message = Recording.prepare(p, LAUNCH, 90, 0, decode_buttons)
  equal(prepared ~= nil, true, "reset request prepares before mutation")
  equal(kind, nil, "reset prepare kind")
  equal(message, nil, "reset prepare message")

  local sink = fake_sink()
  local installed = 0
  equal(sink.writes, 0, "prepare writes no event")
  local state, effect = Recording.start(
    p, 30, LAUNCH, 91, 0, sink, decode_buttons,
    function(input) installed = installed + 1; return input.a ~= nil end,
    function() return true end, prepared)
  equal(effect.kind, "arm_observation", "reset release starts at native boundary")
  equal(installed, 1, "reset offset zero input is armed")
  equal(state.start_frame, 91, "reset release frame")
  equal(sink.writes, 1, "reset release emits first boundary")
  os.remove(path)
end

do
  local long = params(1400, {
    limits = {
      max_frames = 1400,
      max_events = 1400,
      max_bytes = 1024 * 1024,
      max_line_bytes = 4096,
      max_host_ms = 70000,
      progress_interval_ms = 250,
    },
  })
  local validated, kind, message = Recording.validate(long, LAUNCH, 0, 0)
  equal(validated ~= nil, true, "1400-frame admission")
  equal(kind, nil, "1400-frame admission kind")
  equal(message, nil, "1400-frame admission message")

  local over = params(5001)
  local state, _, over_kind = Recording.start(over, 99, LAUNCH, 0, 0, fake_sink())
  equal(state, nil, "over-ceiling recording rejects")
  equal(over_kind, "bad_params", "over-ceiling rejection kind")
end

do
  local p = params(2, {
    warmup_frames = 3,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "frame_completed", contract_sha256 = COMPLETED_CONTRACT },
      { id = "snes_ppu_obj_handoff", contract_sha256 = OBJ_HANDOFF_CONTRACT },
    },
    limits = {
      max_frames = 5,
      max_events = 1000,
      max_bytes = 1024 * 1024,
      max_line_bytes = 4096,
      max_host_ms = 2000,
      progress_interval_ms = 100,
    },
  })
  local state, effect = Recording.start(
    p, 97, LAUNCH, 100, 0, fake_sink(), nil, nil, nil, nil, function() return true end)
  equal(effect.kind, "working", "warmup begins without observation hooks")
  local ignored
  state, ignored = Recording.semantic_event(
    state, "snes_ppu_obj_handoff", 100, 1, obj_payload())
  equal(ignored, nil, "observation event is ignored before its armed interval")
  state = select(1, Recording.tick(state, 101, 1))
  state = select(1, Recording.tick(state, 102, 2))
  state, effect = Recording.tick(state, 103, 3)
  equal(effect.kind, "arm_observation", "observation arms at the declared guest boundary")
  equal(Recording.attach_hooks(state), true, "warmup observation hooks attached")
  state, effect = Recording.semantic_event(
    state, "snes_ppu_obj_handoff", 103, 3000, obj_payload())
  equal(effect, nil, "tail observation event accepted")
  state = select(1, Recording.tick(state, 104, 4))
  state, effect = Recording.tick(state, 105, 5)
  equal(effect.kind, "terminal", "warmup transaction terminal")
  local result = Recording.result(state, 5)
  equal(result.f_origin, 100, "warmup origin")
  equal(result.f_start, 103, "observation start")
  equal(result.f_end, 105, "observation end")
  equal(result.frames, 2, "observation frame count")
  equal(result.event_classes[1].observed, 5, "transaction frame accounting")
  equal(result.event_classes[3].observed, 1, "tail event accounting")
  equal(result.event_classes[1].armed_interval.f_start, 100, "frame class interval start")
  equal(result.event_classes[1].armed_interval.f_end, 105, "frame class interval end")
  equal(result.event_classes[3].armed_interval.f_start, 103, "deep class interval start")
  equal(result.event_classes[3].armed_interval.f_end, 105, "deep class interval end")
end

do
  local text = "0:a\n1:b\n2:a\n3:b\n4:a\n"
  local path = movie_file(text)
  local p = params(2, {
    warmup_frames = 3,
    event_arming = {
      { id = "frame_boundary", scope = "observation" },
    },
    input_movie = {
      path = path,
      format = "frame-full-state-1",
      port = 0,
      frames = 5,
      bytes = #text,
      sha256 = string.rep("6", 64),
    },
    limits = {
      max_frames = 5,
      max_events = 2,
      max_bytes = 1024 * 1024,
      max_line_bytes = 4096,
      max_host_ms = 2000,
      progress_interval_ms = 100,
    },
  })
  local installed, released = 0, false
  local sink = fake_sink()
  local state, effect = Recording.start(
    p, 95, LAUNCH, 300, 0, sink, decode_buttons,
    function() installed = installed + 1; return true end,
    function() released = true; return true end)
  equal(effect.kind, "working", "observation-only boundary warmup begins")
  equal(sink.writes, 0, "warmup emits no observation-only boundary")
  state = select(1, Recording.tick(state, 301, 1))
  state = select(1, Recording.tick(state, 302, 2))
  state, effect = Recording.tick(state, 303, 3)
  equal(effect.kind, "arm_observation", "observation-only boundary begins at f_start")
  equal(sink.writes, 1, "f_start boundary is the first emitted record")
  assert(Recording.attach_hooks(state, false))
  state = select(1, Recording.tick(state, 304, 4))
  state, effect = Recording.tick(state, 305, 5)
  equal(effect.kind, "terminal", "observation-only boundary reaches the target")
  local result = Recording.result(state, 5)
  equal(result.events, 2, "warmup boundaries do not consume event accounting")
  equal(result.dropped, 0, "excluded warmup callbacks are not drops")
  equal(result.event_classes[1].observed, 2, "observation boundary count")
  equal(result.event_classes[1].armed_interval.f_start, 303,
    "observation boundary interval start")
  equal(result.event_classes[1].armed_interval.f_end, 305,
    "observation boundary interval end")
  equal(installed, 5, "input movie continues through warmup and observation")
  equal(released, true, "input movie is released at the frozen terminal")
  os.remove(path)

  local rejected, rejected_effect, rejected_kind = Recording.start(params(1, {
    warmup_frames = 1,
    event_arming = {},
    limits = {
      max_frames = 2,
      max_events = 2,
      max_bytes = 1024 * 1024,
      max_line_bytes = 4096,
      max_host_ms = 2000,
      progress_interval_ms = 100,
    },
  }), 94, LAUNCH, 400, 0, fake_sink())
  equal(rejected, nil, "incomplete event arming rejects before recording")
  equal(rejected_effect, nil, "incomplete event arming has no guest effect")
  equal(rejected_kind, "bad_params", "incomplete event arming rejection kind")
end

do
  local p = params(2, {
    warmup_frames = 2,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "frame_completed", contract_sha256 = COMPLETED_CONTRACT },
    },
    stop_on = { event_class = "frame_completed", occurrence = 1 },
    limits = {
      max_frames = 4,
      max_events = 100,
      max_bytes = 1024 * 1024,
      max_line_bytes = 4096,
      max_host_ms = 2000,
      progress_interval_ms = 100,
    },
  })
  local state, effect = Recording.start(p, 96, LAUNCH, 200, 0, fake_sink())
  state = select(1, Recording.tick(state, 201, 1))
  state, effect = Recording.tick(state, 202, 2)
  equal(effect.kind, "arm_observation", "stop counter waits through warmup")
  assert(Recording.attach_hooks(state, false))
  state, effect = Recording.tick(state, 203, 3)
  equal(effect.kind, "terminal", "first observation completion stops")
  local result = Recording.result(state, 3)
  equal(result.frames, 1, "stop reports observation frames only")
  equal(result.stop_event.occurrence, 1, "stop occurrence is observation-relative")
  equal(result.stop_event.frame, 202, "stop event belongs to the first observation frame")
end

do
  local rows = {}
  for offset = 0, 1399 do rows[#rows + 1] = offset .. ":a\n" end
  local text = table.concat(rows)
  local path = movie_file(text)
  local p = params(1400, {
    input_movie = {
      path = path,
      format = "frame-full-state-1",
      port = 0,
      frames = 1400,
      bytes = #text,
      sha256 = string.rep("6", 64),
    },
    limits = {
      max_frames = 1400,
      max_events = 5000,
      max_bytes = 1024 * 1024,
      max_line_bytes = 4096,
      max_host_ms = 70000,
      progress_interval_ms = 250,
    },
  })
  local installed = 0
  local released = false
  local state, effect = Recording.start(
    p, 98, LAUNCH, 100, 0, fake_sink(), decode_buttons,
    function(input)
      if input.a then installed = installed + 1 end
      return true
    end,
    function() released = true; return true end)
  for boundary = 101, 1500 do
    state, effect = Recording.tick(state, boundary, boundary - 100)
  end
  equal(effect.kind, "terminal", "1400-frame terminal")
  equal(Recording.result(state, 1400).frames, 1400, "1400-frame result")
  equal(installed, 1400, "1400-frame movie applied whole")
  equal(released, true, "1400-frame movie releases input")
  os.remove(path)
end

do
  local text = "0:a\n1:b\n2:\n"
  local path = movie_file(text)
  local p = params(3, {
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "frame_completed", contract_sha256 = COMPLETED_CONTRACT },
    },
    input_movie = {
      path = path,
      format = "frame-full-state-1",
      port = 0,
      frames = 3,
      bytes = #text,
      sha256 = string.rep("c", 64),
    },
    stop_on = { event_class = "frame_completed", occurrence = 2 },
  })
  local order = {}
  local installed = {}
  local sink = fake_sink({ order = order })
  local function install_input(state)
    installed[#installed + 1] = state
    order[#order + 1] = state.a and "input:a" or (state.b and "input:b" or "input:none")
    return true
  end
  local function release_input()
    order[#order + 1] = "input:release"
    return true
  end
  local state, effect = Recording.start(
    p, 14, LAUNCH, 40, 0, sink, decode_buttons, install_input, release_input)
  equal(effect.kind, "arm_observation", "movie arm progress")
  equal(installed[1].a, true, "offset zero input")
  state, effect = Recording.tick(state, 41, 1)
  equal(installed[2].b, true, "offset one input")
  state, effect = Recording.tick(state, 42, 2)
  equal(effect.kind, "terminal", "event stop terminal")
  equal(#installed, 2, "unused movie suffix is not applied")
  local result = Recording.result(state, 2)
  equal(result.execution_outcome, "event_stop", "event stop outcome")
  equal(result.frames, 2, "event stop actual frames")
  equal(result.events, 4, "interleaved event count")
  equal(result.f_end, 42, "event stop scope")
  equal(result.stop_event.sequence, 3, "terminal event sequence")
  equal(result.stop_event.clock_tick, 42, "terminal event clock")
  equal(result.cleanup.transient_input, "released", "movie input cleanup")
  contains(sink.chunks[2], '"class":"frame_completed"', "first completion event")
  contains(sink.chunks[3], '"class":"frame_boundary"', "next boundary event")
  contains(sink.chunks[4], '"class":"frame_completed"', "stop completion event")
  local expected_order = {
    "input:a", "event:boundary", "event:completed", "input:b",
    "event:boundary", "event:completed", "input:release",
  }
  for index, expected in ipairs(expected_order) do
    equal(order[index], expected, "movie/event ordering " .. index)
  end
  equal(#order, #expected_order, "movie/event ordering length")
  os.remove(path)
end

do
  local function schedule(step_ms)
    local text = "0:a\n1:b\n2:\n"
    local path = movie_file(text)
    local order = {}
    local override = nil
    local p = params(3, {
      event_classes = {
        { id = "frame_boundary", contract_sha256 = CONTRACT },
        { id = "frame_completed", contract_sha256 = COMPLETED_CONTRACT },
      },
      input_movie = {
        path = path,
        format = "frame-full-state-1",
        port = 0,
        frames = 3,
        bytes = #text,
        sha256 = string.rep("f", 64),
      },
    })
    local state, effect = Recording.start(
      p, 16, LAUNCH, 80, 0, fake_sink({ order = order }), decode_buttons,
      function(input)
        override = input
        order[#order + 1] = input.a and "input:a" or (input.b and "input:b" or "input:none")
        return true
      end,
      function()
        override = nil
        order[#order + 1] = "input:release"
        return true
      end)
    equal(effect.kind, "arm_observation", "delayed schedule arm")
    for offset = 1, 3 do
      state, effect = Recording.tick(state, 80 + offset, step_ms * offset)
    end
    equal(effect.kind, "terminal", "delayed schedule terminal")
    local result = Recording.result(state, step_ms * 3)
    equal(result.frames, 3, "delayed schedule frames")
    equal(result.cleanup.transient_input, "released", "delayed schedule cleanup")
    equal(override, nil, "native input ownership after recording")
    local native = { keyboard = true }
    if override then native = override end
    equal(native.keyboard, true, "native input remains visible after recording")
    os.remove(path)
    return table.concat(order, "|")
  end

  equal(schedule(1), schedule(500), "host delay does not move movie input")
end

do
  local text = "0:unknown\n"
  local path = movie_file(text)
  local p = params(1, {
    input_movie = {
      path = path,
      format = "frame-full-state-1",
      port = 0,
      frames = 1,
      bytes = #text,
      sha256 = string.rep("d", 64),
    },
  })
  local state, _, kind = Recording.start(p, 15, LAUNCH, 10, 0, fake_sink(), decode_buttons)
  equal(state, nil, "unknown movie button rejects before arm")
  equal(kind, "bad_params", "unknown movie button kind")
  os.remove(path)
end

do
  for _, occurrence in ipairs({ 1, 3 }) do
    local p = params(3, {
      event_classes = {
        { id = "frame_boundary", contract_sha256 = CONTRACT },
        { id = "frame_completed", contract_sha256 = COMPLETED_CONTRACT },
      },
      stop_on = { event_class = "frame_completed", occurrence = occurrence },
    })
    local state, effect = Recording.start(p, 20 + occurrence, LAUNCH, 50, 0, fake_sink())
    local boundary = 50
    while effect.kind ~= "terminal" do
      boundary = boundary + 1
      state, effect = Recording.tick(state, boundary, boundary - 50)
    end
    local result = Recording.result(state, boundary - 50)
    equal(result.execution_outcome, "event_stop", "first/final stop outcome")
    equal(result.frames, occurrence, "first/final stop frames")
    equal(result.events, occurrence * 2, "first/final stop records")
  end
end

do
  local text = "0:a\n"
  local path = movie_file(text)
  local p = params(1, {
    input_movie = {
      path = path,
      format = "frame-full-state-1",
      port = 0,
      frames = 1,
      bytes = #text,
      sha256 = string.rep("e", 64),
    },
  })
  local released = false
  local state, effect = Recording.start(
    p, 24, LAUNCH, 60, 0, fake_sink({ error_at = 1 }), decode_buttons,
    function() return true end,
    function() released = true; return true end)
  equal(effect.kind, "terminal", "movie sink failure terminal")
  equal(released, true, "movie sink failure releases input")
  equal(Recording.result(state, 0).cleanup.transient_input, "released",
    "movie sink failure cleanup")
  os.remove(path)
end

do
  local sink = fake_sink()
  local state, effect = Recording.start(params(3), 7, LAUNCH, 40, 1000, sink)
  equal(effect.kind, "arm_observation", "arm progress")
  local progress = Recording.progress(state)
  equal(progress.capture_id, "capture-test", "progress capture identity")
  equal(progress.sequence, 0, "first progress sequence")
  equal(progress.events, 1, "first progress event count")
  contains(sink.chunks[1], '"contract_sha256":"' .. CONTRACT .. '"', "event contract")
  state = select(1, Recording.tick(state, 41, 1010))
  state = select(1, Recording.tick(state, 42, 1020))
  state, effect = Recording.tick(state, 43, 1030)
  equal(effect.kind, "terminal", "exact terminal")
  local result = Recording.result(state, 1030.75)
  equal(result.status, "completed", "terminal status")
  equal(result.operation_outcome, "completed", "terminal operation")
  equal(result.integrity, "complete", "terminal integrity")
  equal(result.f_start, 40, "scope start")
  equal(result.f_end, 43, "scope end")
  equal(result.final_frame, 43, "freeze boundary")
  equal(result.events, 3, "exact event count")
  equal(result.wall_ms, 30, "fractional host time is normalized to wire integer")
  equal(result.cleanup.hooks, "not_acquired", "hook cleanup")
  equal(result.cleanup.sink, "released", "sink cleanup")
  equal(sink.close_count, 1, "sink close count")
end

do
  local sink = fake_sink({ partial_at = 2, partial_bytes = 7 })
  local state = assert(Recording.start(params(3), 8, LAUNCH, 70, 0, sink))
  local effect
  state, effect = Recording.tick(state, 71, 1)
  equal(effect.kind, "terminal", "partial terminal")
  local result = Recording.result(state, 1)
  equal(result.integrity, "unverifiable", "partial integrity")
  equal(result.events, 1, "partial complete records")
  equal(result.physical_bytes, result.bytes + 7, "partial physical bytes")
end

do
  local sink = fake_sink()
  local p = params(3)
  local state = assert(Recording.start(p, 9, LAUNCH, 10, 0, sink))
  state.limits.max_events = 1 -- internal fault injection after valid admission
  local effect
  state, effect = Recording.tick(state, 11, 1)
  equal(effect.kind, "terminal", "event limit terminal")
  equal(Recording.result(state, 1).integrity, "lossy", "event limit integrity")
end

do
  local deep = Recording.capability(function(value) return value end, true, false, true, true)
  local p = params(1, {
    capability_revision = deep.revision,
    event_classes = {
      { id = "frame_boundary", contract_sha256 = CONTRACT },
      { id = "snes_ppu_obj_consumption_read", contract_sha256 = OBJ_CONSUMPTION_CONTRACT },
    },
  })
  local state = assert(Recording.start(
    p, 36, LAUNCH, 4210, 0, fake_sink(), nil, nil, nil, nil, function() return true end))
  assert(Recording.attach_hooks(state))
  state.limits.max_events = 1 -- overflow on the first dense semantic event
  local effect
  state, effect = Recording.semantic_event(
    state, "snes_ppu_obj_consumption_read", 4210, 1504075748, obj_consumption_payload())
  equal(effect.kind, "terminal", "semantic event limit terminal")
  local result = Recording.result(state, 1)
  equal(result.reason, "event_limit_exceeded", "semantic event limit reason")
  equal(result.final_frame, 4210, "semantic terminal stays in the frame domain")
  equal(result.f_end, 4211, "semantic terminal includes its partial frame in scope")
  equal(result.frames, 1, "semantic terminal counts one scoped partial frame")
  Recording.capability(function(value) return value end, true, false, false, true)
end

do
  local sink = fake_sink()
  local state = assert(Recording.start(params(5), 10, LAUNCH, 20, 0, sink))
  local effect
  state, effect = Recording.cancel(state, 21, "request_cancelled")
  equal(effect.kind, "terminal", "cancel terminal")
  local result = Recording.result(state, 1)
  equal(result.status, "interrupted", "cancel wire status")
  equal(result.operation_outcome, "aborted", "cancel operation")
  equal(result.integrity, "unverifiable", "cancel integrity")
  equal(result.dropped, 0, "cancel is not invented producer loss")
  equal(sink.close_count, 1, "cancel closes sink")
end

do
  local bad = params(1)
  bad.launch_id = "launch-other"
  local state, effect, kind = Recording.start(bad, 11, LAUNCH, 0, 0, fake_sink())
  equal(state, nil, "mismatched launch state")
  equal(effect, nil, "mismatched launch effect")
  equal(kind, "bad_state", "mismatched launch kind")

  bad = params(1)
  bad.event_classes[1].contract_sha256 = string.rep("b", 64)
  state, effect, kind = Recording.start(bad, 12, LAUNCH, 0, 0, fake_sink())
  equal(state, nil, "mismatched contract state")
  equal(kind, "unsupported", "mismatched contract kind")

  bad = params(1)
  bad.limits.max_events = 100001
  state, effect, kind = Recording.start(bad, 13, LAUNCH, 0, 0, fake_sink())
  equal(state, nil, "expanded limit state")
  equal(kind, "bad_params", "expanded limit kind")

  bad = params(1)
  bad.origin = "unadvertised_origin"
  state, effect, kind = Recording.start(bad, 14, LAUNCH, 0, 0, fake_sink())
  equal(state, nil, "unadvertised origin state")
  equal(kind, "unsupported", "unadvertised origin kind")
end

print("recording_test: ok")
