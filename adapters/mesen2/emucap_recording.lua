local M = {}

local FRAME_BOUNDARY_SHA256 = "498fcd52f2fa2327e0af9e9730b4314f0854a6047f57dcde16961b8a4ecb80cd"
local FRAME_COMPLETED_SHA256 = "a335a785a0c109cc7edc6ecab27ff429e386c2ad2eb34769cac4f9cc47378b91"
local OBJ_EVALUATION_SHA256 = "0d32bfc67347b3169fd77f9d30beb9c325c64db30f0081c628ba85646ebd763b"
local OBJ_HANDOFF_SHA256 = "ad23c438ee6400f5f9cab84d877f490abe24670769e50efd2bf67d932d329bbc"
local CPU_INSTRUCTION_SHA256 = "f936fa1f0509851d3394edf3e3f7d6db0e40dd4310531f2ae73ac4ba81c55af0"
local CONTENT_READ_SHA256 = "233b0c2291c335a96afcdcb2b0caa6ca8e21c9c79458c8441531a3dccee4c883"
local TRANSFER_ENABLE_SHA256 = "90678005dc54a82c200e9aef01404ca10430ee3a5a32916777cc484650bcdcc7"
local TRANSFER_ACCESS_SHA256 = "735c6cdf3c8cbddde4b754dc6eea6700888bb03e9cd3c77eda750c5f8a372e60"
local DEVICE_PORT_WRITE_SHA256 = "36ffca829da2ceb7f4b76f2d38b12331eade10014dd07ecd61f21403be8e4ca5"
local INTERRUPT_DELIVERY_SHA256 = "c00494d891e76c380bd782d897c5f5ab4b59d918d49c3a64d78d9d4255c11e38"
local OBJ_CONSUMPTION_SHA256 = "570c5e6c3bf493c0b2e93a59d467fb7300c400993115a08789b8836ad536adc8"
local SNES_PPU_STATE_SHA256 = "21005a15437abd767cbeda5c7ede8741e2aeac4a006dafedede03a695377eaa2"
local BASE_CAPABILITY_REVISION = "dea5c89d917c0e645296117dc9b14dcf089a49794dbe72b0319a104016a449bf"
local SNES_STATE_CAPABILITY_REVISION = "7a63f4233406541101fdd078a4bd6ffbd1a9785efc24664355a6b491bd8f0efd"
local SNES_CAPABILITY_REVISION = "f303cc902eb1006eaab2dbd9c05a739a7184b4a4e2be7890e318f9b8c4b218a2"
local BASE_SNAPSHOT_CAPABILITY_REVISION = "3314d6344f03df096660917a6087b19a62aece996402ecdd1ee992c87131d0aa"
local SNES_STATE_SNAPSHOT_CAPABILITY_REVISION = "ea526265eb6a5d6b229d568d2bfe7df503adc54032959a9f73eb361b5e6ade3f"
local SNES_SNAPSHOT_CAPABILITY_REVISION = "3360ead44ccebf59a35aefcba6e5846d645188781682293b508a502343212782"
local SNES_DEEP_CAPABILITY_REVISION = "6f601a701d9a979cde0c118c9fcd4fd4a2d572f529728ca77db5f4ddd99b26b4"
local SNES_DEEP_SNAPSHOT_CAPABILITY_REVISION = "cf4f250d1319e642294bc69db241defe50d8a5ff1546f81aac99720b3209890b"
local SNES_REPEATABLE_CAPABILITY_REVISION = "81a8e91a0680bcf72549d25126c91a3fef1f4205725b7ea6b40f4f9f33d25157"
local capability_revision = BASE_CAPABILITY_REVISION
local semantic_advertised = false
local deep_advertised = false
local INPUT_MOVIE_FORMAT = "frame-full-state-1"
local MAX_FRAMES = 5000
local MAX_EVENTS = 100000
local MAX_BYTES = 64 * 1024 * 1024
local MAX_LINE_BYTES = 64 * 1024
local MAX_HOST_MS = 250000
local MAX_MOVIE_BYTES = 1024 * 1024
local MAX_BUTTONS_PER_FRAME = 32
local MAX_SNAPSHOT_MEMBERS = 8
local MAX_SNAPSHOT_MEMBER_BYTES = 128 * 1024
local MAX_SNAPSHOT_TOTAL_BYTES = 1024 * 1024
local MAX_INITIAL_SNAPSHOT_MEMBERS = 1
local MAX_INITIAL_SNAPSHOT_MEMBER_BYTES = 128 * 1024
local MAX_INITIAL_SNAPSHOT_TOTAL_BYTES = 128 * 1024
local MAX_INITIAL_SNAPSHOT_CALLBACK_MS = 100
local MAX_TERMINAL_STATE_BYTES = 128 * 1024
local PROGRESS_MS = 250

local OBJ_PAYLOAD_FIELDS = {
  { path = "cpu.pc", kind = "integer", min = 0, max = 0xffffff },
  { path = "cpu.a", kind = "integer", min = 0, max = 0xffff },
  { path = "cpu.x", kind = "integer", min = 0, max = 0xffff },
  { path = "cpu.y", kind = "integer", min = 0, max = 0xffff },
  { path = "cpu.sp", kind = "integer", min = 0, max = 0xffff },
  { path = "cpu.d", kind = "integer", min = 0, max = 0xffff },
  { path = "cpu.dbr", kind = "integer", min = 0, max = 0xff },
  { path = "cpu.k", kind = "integer", min = 0, max = 0xff },
  { path = "cpu.ps", kind = "integer", min = 0, max = 0xff },
  { path = "ppu.scanline", kind = "integer", min = 0, max = 0xffff },
  { path = "ppu.dot", kind = "integer", min = 0, max = 0xffff },
  { path = "ppu.hclock", kind = "integer", min = 0, max = 0xffffffff },
  { path = "forced_blank", kind = "boolean" },
}

local function int_field(path, max)
  return { path = path, kind = "integer", min = 0, max = max }
end

local CPU_INSTRUCTION_FIELDS = {
  int_field("pc", 0xffffff), int_field("opcode", 0xff), int_field("a", 0xffff),
  int_field("x", 0xffff), int_field("y", 0xffff), int_field("sp", 0xffff),
  int_field("d", 0xffff), int_field("dbr", 0xff), int_field("k", 0xff),
  int_field("ps", 0xff), { path = "emulation", kind = "boolean" },
  int_field("cpu_cycle", 0x1fffffffffffff),
}

local CONTENT_READ_FIELDS = {
  int_field("bus_address", 0xffffff), int_field("content_offset", 0xffffffff),
  int_field("value", 0xff), int_field("operation", 9), int_field("pc", 0xffffff),
  int_field("cpu_cycle", 0x1fffffffffffff),
}

local TRANSFER_ENABLE_FIELDS = {
  int_field("written_mask", 0xff), int_field("channel", 7),
  { path = "hdma", kind = "boolean" }, { path = "direction_to_bus_a", kind = "boolean" },
  int_field("source", 0xffffff), int_field("destination", 0xffff), int_field("size", 0x10000),
  int_field("mode", 7), { path = "fixed", kind = "boolean" },
  { path = "decrement", kind = "boolean" }, int_field("pc", 0xffffff),
  int_field("table_address", 0xffff), int_field("hdma_bank", 0xff),
  { path = "hdma_indirect", kind = "boolean" },
  int_field("cpu_cycle", 0x1fffffffffffff),
}

local TRANSFER_ACCESS_FIELDS = {
  int_field("bus_address", 0xffffff), int_field("value", 0xff), int_field("operation", 9),
  int_field("channel", 7), { path = "hdma", kind = "boolean" },
}

local DEVICE_PORT_WRITE_FIELDS = {
  int_field("port", 0xffffff), int_field("value", 0xff), int_field("operation", 9),
  { path = "transfer_active", kind = "boolean" }, int_field("channel", 7),
  { path = "hdma", kind = "boolean" }, int_field("pc", 0xffffff),
  int_field("cpu_cycle", 0x1fffffffffffff),
}

local INTERRUPT_DELIVERY_FIELDS = {
  { path = "nmi", kind = "boolean" }, int_field("interrupted_pc", 0xffffff),
  int_field("handler_pc", 0xffffff), int_field("pc", 0xffffff), int_field("a", 0xffff),
  int_field("x", 0xffff), int_field("y", 0xffff), int_field("sp", 0xffff),
  int_field("d", 0xffff), int_field("dbr", 0xff), int_field("k", 0xff),
  int_field("ps", 0xff), { path = "emulation", kind = "boolean" },
  int_field("cpu_cycle", 0x1fffffffffffff),
}

local OBJ_CONSUMPTION_FIELDS = {
  int_field("memory_kind", 1), int_field("address", 0xffff), int_field("value", 0xff),
  int_field("scanline", 0xffff), int_field("dot", 0xffff), int_field("hclock", 0xffff),
}

local EVENT_CONTRACTS = {
  frame_boundary = { digest = FRAME_BOUNDARY_SHA256, clock = "frame" },
  frame_completed = { digest = FRAME_COMPLETED_SHA256, clock = "frame" },
  snes_ppu_obj_evaluation_start = {
    digest = OBJ_EVALUATION_SHA256, clock = "snes_master", payload_fields = OBJ_PAYLOAD_FIELDS,
  },
  snes_ppu_obj_handoff = {
    digest = OBJ_HANDOFF_SHA256, clock = "snes_master", payload_fields = OBJ_PAYLOAD_FIELDS,
  },
  snes_cpu_instruction = {
    digest = CPU_INSTRUCTION_SHA256, clock = "snes_master", payload_fields = CPU_INSTRUCTION_FIELDS,
  },
  snes_content_read = {
    digest = CONTENT_READ_SHA256, clock = "snes_master", payload_fields = CONTENT_READ_FIELDS,
  },
  snes_transfer_enable = {
    digest = TRANSFER_ENABLE_SHA256, clock = "snes_master", payload_fields = TRANSFER_ENABLE_FIELDS,
  },
  snes_transfer_access = {
    digest = TRANSFER_ACCESS_SHA256, clock = "snes_master", payload_fields = TRANSFER_ACCESS_FIELDS,
  },
  snes_device_port_write = {
    digest = DEVICE_PORT_WRITE_SHA256, clock = "snes_master", payload_fields = DEVICE_PORT_WRITE_FIELDS,
  },
  snes_interrupt_delivery = {
    digest = INTERRUPT_DELIVERY_SHA256, clock = "snes_master", payload_fields = INTERRUPT_DELIVERY_FIELDS,
  },
  snes_ppu_obj_consumption_read = {
    digest = OBJ_CONSUMPTION_SHA256, clock = "snes_master", payload_fields = OBJ_CONSUMPTION_FIELDS,
  },
}

local function integer(value)
  return type(value) == "number" and value == math.floor(value)
end

local function digest(value)
  return type(value) == "string" and #value == 64 and not value:find("[^0-9a-fA-F]")
end

local function safe_id(value)
  return type(value) == "string" and #value >= 1 and #value <= 96
    and not value:find("[^A-Za-z0-9_-]")
end

local function copy_limits(limits)
  return {
    max_frames = limits.max_frames,
    max_events = limits.max_events,
    max_bytes = limits.max_bytes,
    max_line_bytes = limits.max_line_bytes,
    max_host_ms = limits.max_host_ms,
    progress_interval_ms = limits.progress_interval_ms,
  }
end

function M.capability(as_array, include_snes_semantic, include_terminal_snapshots, include_snes_deep,
    include_snes_state, repeatability_conditions)
  semantic_advertised = include_snes_semantic == true
  deep_advertised = include_snes_deep == true
  local state_advertised = include_snes_state == true
  if deep_advertised then
    capability_revision = include_terminal_snapshots and SNES_DEEP_SNAPSHOT_CAPABILITY_REVISION
      or SNES_DEEP_CAPABILITY_REVISION
  elseif semantic_advertised then
    capability_revision = include_terminal_snapshots and SNES_SNAPSHOT_CAPABILITY_REVISION
      or SNES_CAPABILITY_REVISION
  elseif state_advertised then
    capability_revision = include_terminal_snapshots and SNES_STATE_SNAPSHOT_CAPABILITY_REVISION
      or SNES_STATE_CAPABILITY_REVISION
  else
    capability_revision = include_terminal_snapshots and BASE_SNAPSHOT_CAPABILITY_REVISION
      or BASE_CAPABILITY_REVISION
  end
  if repeatability_conditions ~= nil then
    assert(deep_advertised and include_terminal_snapshots and state_advertised,
      "repeatable recording requires the complete canonical SNES observation surface")
    capability_revision = SNES_REPEATABLE_CAPABILITY_REVISION
  end
  local event_classes = {
    {
      id = "frame_boundary",
      contract_sha256 = FRAME_BOUNDARY_SHA256,
      clock_domains = as_array({ "frame" }),
      exact = true,
    },
    {
      id = "frame_completed",
      contract_sha256 = FRAME_COMPLETED_SHA256,
      clock_domains = as_array({ "frame" }),
      exact = true,
      stoppable = true,
    },
  }
  if semantic_advertised then
    event_classes[#event_classes + 1] = {
      id = "snes_ppu_obj_evaluation_start",
      contract_sha256 = OBJ_EVALUATION_SHA256,
      clock_domains = as_array({ "snes_master" }),
      exact = true,
    }
    event_classes[#event_classes + 1] = {
      id = "snes_ppu_obj_handoff",
      contract_sha256 = OBJ_HANDOFF_SHA256,
      clock_domains = as_array({ "snes_master" }),
      exact = true,
    }
  end
  if deep_advertised then
    for _, item in ipairs({
      { "snes_cpu_instruction", CPU_INSTRUCTION_SHA256 },
      { "snes_content_read", CONTENT_READ_SHA256 },
      { "snes_transfer_enable", TRANSFER_ENABLE_SHA256 },
      { "snes_transfer_access", TRANSFER_ACCESS_SHA256 },
      { "snes_device_port_write", DEVICE_PORT_WRITE_SHA256 },
      { "snes_interrupt_delivery", INTERRUPT_DELIVERY_SHA256 },
      { "snes_ppu_obj_consumption_read", OBJ_CONSUMPTION_SHA256 },
    }) do
      event_classes[#event_classes + 1] = {
        id = item[1], contract_sha256 = item[2],
        clock_domains = as_array({ "snes_master" }), exact = true,
        startable = item[1] == "snes_cpu_instruction" or nil,
      }
    end
  end
  local capability = {
    revision = capability_revision,
    origins = as_array({ "next_frame_boundary", "reset_release" }),
    units = as_array({ "frames" }),
    default_event_classes = as_array({ "frame_boundary" }),
    event_classes = as_array(event_classes),
    event_order = "guest_emission",
    class_accounting = true,
    warmup = {
      max_frames = MAX_FRAMES,
      transaction_event_classes = as_array({ "frame_boundary", "frame_completed" }),
    },
    input_movie = {
      format = INPUT_MOVIE_FORMAT,
      port = 0,
      max_frames = MAX_FRAMES,
      max_bytes = MAX_MOVIE_BYTES,
      max_buttons_per_frame = MAX_BUTTONS_PER_FRAME,
    },
    limits = {
      max_frames = MAX_FRAMES,
      max_events = MAX_EVENTS,
      max_bytes = MAX_BYTES,
      max_line_bytes = MAX_LINE_BYTES,
      max_host_ms = MAX_HOST_MS,
      progress_interval_ms = PROGRESS_MS,
    },
  }
  if include_terminal_snapshots then
    capability.terminal_snapshots = {
      max_members = MAX_SNAPSHOT_MEMBERS,
      max_member_bytes = MAX_SNAPSHOT_MEMBER_BYTES,
      max_total_bytes = MAX_SNAPSHOT_TOTAL_BYTES,
    }
  end
  if deep_advertised then
    capability.initial_snapshots = {
      memory_types = as_array({ "snesWorkRam" }),
      start_positions = as_array({ "event_anchor" }),
      max_members = MAX_INITIAL_SNAPSHOT_MEMBERS,
      max_member_bytes = MAX_INITIAL_SNAPSHOT_MEMBER_BYTES,
      max_total_bytes = MAX_INITIAL_SNAPSHOT_TOTAL_BYTES,
      max_callback_ms = MAX_INITIAL_SNAPSHOT_CALLBACK_MS,
    }
  end
  if state_advertised then
    capability.terminal_state = {
      max_bytes = MAX_TERMINAL_STATE_BYTES,
      profiles = as_array({ {
        id = "snes_ppu",
        contract_sha256 = SNES_PPU_STATE_SHA256,
        groups = as_array({ "ppu" }),
      } }),
    }
  end
  if repeatability_conditions ~= nil then
    capability.repeatability = {
      profile = "mesen_snes_repeatable",
      conditions_sha256 = repeatability_conditions,
      origins = as_array({ "reset_release" }),
      requires_input_movie = true,
    }
  end
  return capability
end

function M.revision()
  return capability_revision
end

local function validate_event_classes(classes)
  if type(classes) ~= "table" then return nil, "event_classes must be an array" end
  local selected, order = {}, {}
  for _, class in ipairs(classes) do
    if type(class) ~= "table" or type(class.id) ~= "string" then
      return nil, "event_classes contains an invalid entry"
    end
    local contract = EVENT_CONTRACTS[class.id]
    local semantic = class.id == "snes_ppu_obj_evaluation_start"
      or class.id == "snes_ppu_obj_handoff"
    local deep = class.id:match("^snes_") and not semantic
    if not contract or (semantic and not semantic_advertised) or (deep and not deep_advertised)
        or class.contract_sha256 ~= contract.digest or selected[class.id] then
      return nil, "event_classes do not match the advertised contracts"
    end
    selected[class.id] = true
    order[#order + 1] = class.id
  end
  if not selected.frame_boundary then
    return nil, "frame_boundary must be selected"
  end
  return selected, order
end

local function validate_limits(frames, event_count_per_frame, limits)
  if type(limits) ~= "table" then
    return nil, "bad_params", "limits must be an object"
  end
  local expected = {
    max_frames = frames,
    max_events = MAX_EVENTS,
    max_bytes = MAX_BYTES,
    max_line_bytes = MAX_LINE_BYTES,
    max_host_ms = MAX_HOST_MS,
    progress_interval_ms = PROGRESS_MS,
  }
  for name, ceiling in pairs(expected) do
    local value = limits[name]
    if not integer(value) or value < 1 or value > ceiling then
      return nil, "bad_params", name .. " is outside the advertised limit"
    end
  end
  if limits.max_frames ~= frames then
    return nil, "bad_params", "max_frames must equal the requested frame count"
  end
  if limits.max_events < frames * event_count_per_frame
      or limits.max_line_bytes > limits.max_bytes then
    return nil, "bad_params", "limits cannot contain the required frame stream"
  end
  if limits.progress_interval_ms < 10 or limits.progress_interval_ms >= limits.max_host_ms then
    return nil, "bad_params", "progress_interval_ms must be in 10..max_host_ms"
  end
  return copy_limits(limits)
end

local function read_movie(params, frames)
  if params == nil then return nil end
  if type(params) ~= "table" or params.format ~= INPUT_MOVIE_FORMAT or params.port ~= 0
      or params.frames ~= frames or not integer(params.bytes) or params.bytes < 1
      or params.bytes > MAX_MOVIE_BYTES or not digest(params.sha256)
      or type(params.path) ~= "string" or params.path == "" then
    return nil, "input_movie identity is invalid"
  end
  local file, open_error = io.open(params.path, "rb")
  if not file then return nil, "input_movie open failed: " .. tostring(open_error) end
  local text = file:read("*a")
  file:close()
  if type(text) ~= "string" or #text ~= params.bytes or text:sub(-1) ~= "\n" then
    return nil, "input_movie size or terminal newline mismatch"
  end

  local movie = {}
  local expected = 0
  for line in text:gmatch("([^\n]+)\n") do
    local offset, names = line:match("^(%d+):(.*)$")
    offset = tonumber(offset)
    if offset ~= expected then return nil, "input_movie offsets must be dense from zero" end
    local buttons = {}
    local previous = nil
    if names ~= "" then
      for button in (names .. ","):gmatch("([^,]*),") do
        if button == "" or button:find("[^a-z0-9_+%-]")
            or (previous and button <= previous) then
          return nil, "input_movie buttons are not canonical"
        end
        buttons[#buttons + 1] = button
        previous = button
      end
    end
    if #buttons > MAX_BUTTONS_PER_FRAME then
      return nil, "input_movie button count exceeds the advertised limit"
    end
    movie[#movie + 1] = buttons
    expected = expected + 1
  end
  if expected ~= frames then return nil, "input_movie frame count mismatch" end
  return movie
end

function M.validate(params, expected_launch_id, start_frame, now_ms)
  if type(params) ~= "table" then return nil, "bad_params", "params must be an object" end
  if not safe_id(params.capture_id) then
    return nil, "bad_params", "capture_id is invalid"
  end
  if type(expected_launch_id) ~= "string" or expected_launch_id == ""
      or params.launch_id ~= expected_launch_id then
    return nil, "bad_state", "launch_id does not match the active generation"
  end
  if not digest(params.request_digest_sha256) then
    return nil, "bad_params", "request_digest_sha256 must be a SHA-256"
  end
  if params.capability_revision ~= capability_revision then
    return nil, "unsupported", "capability revision mismatch"
  end
  if params.origin ~= "next_frame_boundary" and params.origin ~= "reset_release" then
    return nil, "unsupported", "origin is not advertised by this runtime"
  end
  if not integer(params.frames) or params.frames < 1 or params.frames > MAX_FRAMES then
    return nil, "bad_params", "frames must be an integer in 1.." .. MAX_FRAMES
  end
  local warmup_frames = params.warmup_frames or 0
  if not integer(warmup_frames) or warmup_frames < 0
      or warmup_frames + params.frames > MAX_FRAMES then
    return nil, "bad_params", "warmup_frames + frames must be in 1.." .. MAX_FRAMES
  end
  local total_frames = warmup_frames + params.frames
  if not integer(start_frame) or start_frame < 0 then
    return nil, "bad_state", "start frame must be a non-negative integer"
  end
  if type(now_ms) ~= "number" then return nil, "bad_state", "host time is unavailable" end

  local selected, selected_order = validate_event_classes(params.event_classes)
  local event_error = selected_order
  if not selected then return nil, "unsupported", event_error end
  local event_count_per_frame = selected.frame_completed and 2 or 1
  local limits, kind, message = validate_limits(total_frames, event_count_per_frame, params.limits)
  if not limits then return nil, kind, message end

  local stop_on = params.stop_on
  if stop_on ~= nil then
    if type(stop_on) ~= "table" or stop_on.event_class ~= "frame_completed"
        or not selected.frame_completed or not integer(stop_on.occurrence)
        or stop_on.occurrence < 1 or stop_on.occurrence > params.frames then
      return nil, "bad_params", "stop_on must select an advertised frame_completed occurrence"
    end
  end
  local start_on = params.start_on
  if start_on ~= nil then
    if type(start_on) ~= "table" or start_on.event_class ~= "snes_cpu_instruction"
        or not selected.snes_cpu_instruction then
      return nil, "bad_params",
        "start_on must select the advertised startable snes_cpu_instruction class"
    end
  end
  local initial_snapshots = params.initial_snapshots or {}
  if type(initial_snapshots) ~= "table" then
    return nil, "bad_params", "initial_snapshots must be an array"
  end
  if #initial_snapshots > 0 and start_on == nil then
    return nil, "bad_params", "initial_snapshots require start_on"
  end
  if #initial_snapshots > MAX_INITIAL_SNAPSHOT_MEMBERS then
    return nil, "bad_params", "initial snapshot count exceeds the advertised limit"
  end
  local initial_total, initial_labels = 0, {}
  for _, snapshot in ipairs(initial_snapshots) do
    if type(snapshot) ~= "table" or not safe_id(snapshot.label)
        or initial_labels[snapshot.label] or snapshot.memory_type ~= "snesWorkRam"
        or not integer(snapshot.address) or not integer(snapshot.length)
        or snapshot.address < 0 or snapshot.length < 1
        or snapshot.length > MAX_INITIAL_SNAPSHOT_MEMBER_BYTES
        or snapshot.address > 0x20000 or snapshot.length > 0x20000 - snapshot.address then
      return nil, "bad_params", "initial snapshot identity or WRAM range is invalid"
    end
    initial_labels[snapshot.label] = true
    initial_total = initial_total + snapshot.length
  end
  if initial_total > MAX_INITIAL_SNAPSHOT_TOTAL_BYTES then
    return nil, "bad_params", "initial snapshot bytes exceed the advertised limit"
  end
  local movie, movie_error = read_movie(params.input_movie, total_frames)
  if movie_error then return nil, "bad_params", movie_error end

  return {
    capture_id = params.capture_id,
    launch_id = params.launch_id,
    request_digest_sha256 = params.request_digest_sha256,
    limits = limits,
    selected = selected,
    selected_order = selected_order,
    stop_on = stop_on,
    start_on = start_on,
    initial_snapshots = initial_snapshots,
    movie = movie,
    warmup_frames = warmup_frames,
    total_frames = total_frames,
  }
end

local function close_sink(state)
  if state.sink_closed then return true end
  if state.sink_close_attempted then return nil, state.sink_close_error or "sink close failed" end
  state.sink_close_attempted = true
  local ok, result, err = pcall(state.sink.close)
  if not ok then
    state.sink_close_error = tostring(result)
    return nil, state.sink_close_error
  end
  if result == nil or result == false then
    state.sink_close_error = tostring(err or "sink close failed")
    return nil, state.sink_close_error
  end
  state.sink_closed = true
  return true
end

local function close_member_sink(state)
  if not state.member_sink then return true end
  if state.member_sink_closed then return true end
  if state.member_sink_close_attempted then
    return nil, state.member_sink_close_error or "member sink close failed"
  end
  state.member_sink_close_attempted = true
  local ok, result, err = pcall(state.member_sink.close)
  if not ok then
    state.member_sink_close_error = tostring(result)
    return nil, state.member_sink_close_error
  end
  if result == nil or result == false then
    state.member_sink_close_error = tostring(err or "member sink close failed")
    return nil, state.member_sink_close_error
  end
  state.member_sink_closed = true
  return true
end

local function mark_terminal(state, operation, execution, integrity, boundary, reason, status)
  state.active = false
  state.operation_outcome = operation
  state.execution_outcome = execution
  state.integrity = integrity
  state.final_frame = boundary
  state.actual_end = math.max(state.origin_frame, math.min(boundary, state.maximum_end))
  state.advanced_frames = math.max(state.advanced_frames, state.actual_end - state.origin_frame)
  state.completed_frames = math.max(0, state.actual_end - state.start_frame)
  state.reason = reason
  state.status = status or (operation == "completed" and "completed" or "failed")

  if state.hooks_owned and not state.hooks_released then
    local release_ok, released, release_error = pcall(state.release_hooks)
    if release_ok and released ~= nil and released ~= false then
      state.hooks_owned = false
      state.hooks_released = true
    else
      state.status = "failed"
      state.operation_outcome = "failed"
      state.execution_outcome = "adapter_error"
      state.integrity = "unverifiable"
      local detail = release_ok and release_error or released
      state.reason = "hook_release_failed: " .. tostring(detail or "release failed")
    end
  end

  if state.input_owned and not state.input_released then
    local release_ok, released, release_error = pcall(state.release_input)
    if release_ok and released ~= nil and released ~= false then
      state.input_owned = false
      state.input_released = true
    else
      state.status = "failed"
      state.operation_outcome = "failed"
      state.execution_outcome = "adapter_error"
      state.integrity = "unverifiable"
      local detail = release_ok and release_error or released
      state.reason = "input_release_failed: " .. tostring(detail or "release failed")
    end
  end

  local ok, err = close_sink(state)
  if not ok then
    state.status = "failed"
    state.operation_outcome = "failed"
    state.execution_outcome = "adapter_error"
    state.integrity = "unverifiable"
    state.reason = "sink_close_failed: " .. tostring(err)
  end
  local member_ok, member_err = close_member_sink(state)
  if not member_ok then
    state.status = "failed"
    state.operation_outcome = "failed"
    state.execution_outcome = "adapter_error"
    state.integrity = "unverifiable"
    state.reason = "member_sink_close_failed: " .. tostring(member_err)
  end
  return { kind = "terminal" }
end

local function json_escape(value)
  return (value:gsub('[%z\1-\31\\"]', function(char)
    local escapes = { ['\\'] = '\\\\', ['"'] = '\\"', ['\b'] = '\\b', ['\f'] = '\\f',
      ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t' }
    return escapes[char] or string.format('\\u%04x', char:byte())
  end))
end

local function json_value(value)
  local kind = type(value)
  if kind == "nil" then return "null" end
  if kind == "boolean" then return value and "true" or "false" end
  if kind == "number" then
    if value ~= math.floor(value) or value < 0 then return nil, "payload numbers must be non-negative integers" end
    return string.format("%.0f", value)
  end
  if kind == "string" then return '"' .. json_escape(value) .. '"' end
  if kind ~= "table" then return nil, "unsupported payload value" end
  local keys = {}
  for key in pairs(value) do
    if type(key) ~= "string" then return nil, "payload objects require string keys" end
    keys[#keys + 1] = key
  end
  table.sort(keys)
  local fields = {}
  for _, key in ipairs(keys) do
    local encoded, err = json_value(value[key])
    if not encoded then return nil, err end
    fields[#fields + 1] = '"' .. json_escape(key) .. '":' .. encoded
  end
  return "{" .. table.concat(fields, ",") .. "}"
end

local function payload_value(payload, path)
  local value = payload
  for part in path:gmatch("[^.]+") do
    if type(value) ~= "table" then return nil end
    value = value[part]
  end
  return value
end

local function payload_leaf_paths(value, prefix, paths)
  if type(value) ~= "table" then return nil end
  for key, child in pairs(value) do
    if type(key) ~= "string" then return nil end
    local path = prefix == "" and key or (prefix .. "." .. key)
    if type(child) == "table" then
      if not payload_leaf_paths(child, path, paths) then return nil end
    else
      paths[path] = true
    end
  end
  return true
end

local function validate_payload(contract, payload)
  if not contract.payload_fields then
    return type(payload) == "table" and next(payload) == nil
  end
  local actual = {}
  if not payload_leaf_paths(payload, "", actual) then return nil end
  local expected_count = 0
  for _, field in ipairs(contract.payload_fields) do
    expected_count = expected_count + 1
    if not actual[field.path] then return nil end
    actual[field.path] = nil
    local value = payload_value(payload, field.path)
    if field.kind == "boolean" then
      if type(value) ~= "boolean" then return nil end
    elseif not integer(value) or value < field.min or value > field.max then
      return nil
    end
  end
  return next(actual) == nil and expected_count == #contract.payload_fields
end

local function event_line(sequence, class, contract, frame, clock_domain, tick, payload)
  local encoded, err = json_value(payload or {})
  if not encoded then return nil, err end
  return string.format(
    '{"sequence":%d,"class":"%s","contract_sha256":"%s","clock":{"domain":"%s","tick":%d},"frame":%d,"payload":%s}\n',
    sequence, class, contract, clock_domain, tick, frame, encoded)
end

local function write_event(state, class, frame, tick, payload)
  local contract = EVENT_CONTRACTS[class]
  if not contract or not state.selected[class] then
    return mark_terminal(state, "failed", "adapter_error", "unverifiable", frame,
      "unselected_event_class")
  end
  payload = payload or {}
  if not validate_payload(contract, payload) then
    return mark_terminal(state, "failed", "adapter_error", "unverifiable", tick,
      "event_payload_contract_failed")
  end
  if class == "frame_boundary" then
    if frame ~= state.origin_frame + state.boundary_records then
      return mark_terminal(state, "failed", "adapter_error", "unverifiable", frame,
        "frame_boundary_gap_or_regression")
    end
  elseif class == "frame_completed" and frame ~= state.origin_frame + state.completed_records then
    return mark_terminal(state, "failed", "adapter_error", "unverifiable", tick,
      "frame_completed_gap_or_regression")
  end

  local sequence = state.events
  local line, encode_error = event_line(sequence, class, contract.digest, frame, contract.clock, tick, payload)
  if not line then
    return mark_terminal(state, "failed", "adapter_error", "unverifiable", tick,
      "event_payload_encode_failed: " .. tostring(encode_error))
  end
  if #line > state.limits.max_line_bytes then
    return mark_terminal(state, "failed", "adapter_error", "unverifiable", tick,
      "event_line_limit_exceeded")
  end
  if state.events + 1 > state.limits.max_events then
    state.dropped = state.dropped + 1
    state.class_dropped[class] = (state.class_dropped[class] or 0) + 1
    return mark_terminal(state, "failed", "loss_detected", "lossy", tick,
      "event_limit_exceeded")
  end
  if state.physical_bytes + #line > state.limits.max_bytes then
    state.dropped = state.dropped + 1
    state.class_dropped[class] = (state.class_dropped[class] or 0) + 1
    return mark_terminal(state, "failed", "loss_detected", "lossy", tick,
      "byte_limit_exceeded")
  end

  local ok, result, err, partial = pcall(state.sink.write, line)
  if not ok then
    return mark_terminal(state, "failed", "adapter_error", "unverifiable", tick,
      "sink_write_failed: " .. tostring(result))
  end
  if result == true then result = #line end
  if type(result) == "number" and result == #line then
    state.events = state.events + 1
    state.class_counts[class] = (state.class_counts[class] or 0) + 1
    state.complete_bytes = state.complete_bytes + #line
    state.physical_bytes = state.physical_bytes + #line
    if class == "frame_boundary" then
      state.boundary_records = state.boundary_records + 1
      state.last_frame = frame
    elseif class == "frame_completed" then
      state.completed_records = state.completed_records + 1
    end
    return nil, {
      sequence = sequence,
      event_class = class,
      clock_domain = contract.clock,
      clock_tick = tick,
      frame = frame,
      occurrence = state.class_counts[class],
      contract_sha256 = contract.digest,
    }
  end

  local sent = tonumber(partial)
  if sent == nil and type(result) == "number" then sent = result end
  sent = math.max(0, math.min(sent or 0, #line))
  state.physical_bytes = state.physical_bytes + sent
  if sent > 0 then state.truncated = true end
  return mark_terminal(state, "failed", "adapter_error", "unverifiable", tick,
    "sink_write_failed: " .. tostring(err or "partial write"))
end

function M.semantic_event(state, class, frame, tick, payload)
  if not state or not state.active or not state.observation_armed
      or frame < state.start_frame or not state.selected[class] then return state, nil end
  if state.start_on and not state.observation_started then
    if class ~= state.start_on.event_class then return state, nil end
    if #state.initial_snapshots > 0 then
      local started = state.wall_ms()
      local ok, captured, capture_error = pcall(state.capture_initial, state)
      local elapsed = state.wall_ms() - started
      if not ok or captured == nil or captured == false
          or elapsed < 0 or elapsed > MAX_INITIAL_SNAPSHOT_CALLBACK_MS then
        return state, mark_terminal(state, "failed", "adapter_error", "unverifiable", frame,
          "initial_snapshot_failed: " .. tostring(ok and capture_error or captured))
      end
    end
    local closed, close_error = close_member_sink(state)
    if not closed then
      return state, mark_terminal(state, "failed", "adapter_error", "unverifiable", frame,
        "initial_snapshot_sink_close_failed: " .. tostring(close_error))
    end
    state.observation_started = true
    for _, id in ipairs(state.selected_order) do
      if id ~= "frame_boundary" and id ~= "frame_completed" then
        state.class_armed[id] = true
        state.class_start[id] = state.start_frame
      end
    end
  elseif not state.observation_started then
    return state, nil
  end
  local effect, facts = write_event(state, class, frame, tick, payload)
  if not effect and state.start_on and not state.observation_start then
    -- Event occurrence is local stop-condition bookkeeping. The public observation anchor has a
    -- closed shape and identifies the exact stream record directly by sequence and clock.
    state.observation_start = {
      sequence = facts.sequence,
      event_class = facts.event_class,
      contract_sha256 = facts.contract_sha256,
      frame = facts.frame,
      clock_domain = facts.clock_domain,
      clock_tick = facts.clock_tick,
    }
  end
  return state, effect
end

local function decode_movie(movie, decode_buttons)
  if not movie then return nil end
  if type(decode_buttons) ~= "function" then return nil, "input decoder is unavailable" end
  local decoded = {}
  for index, buttons in ipairs(movie) do
    local state, err = decode_buttons(buttons)
    if not state then return nil, "input_movie frame " .. (index - 1) .. ": " .. tostring(err) end
    decoded[index] = state
  end
  return decoded
end

-- Validate and decode every caller-controlled movie token before a reset is queued. The prepared
-- value owns no input, hook, sink, or guest time and can therefore be held until the native reset
-- callback establishes the first post-reset boundary.
function M.prepare(params, expected_launch_id, start_frame, now_ms, decode_buttons, prevalidated)
  local validated, kind, message = prevalidated, nil, nil
  if not validated then
    validated, kind, message = M.validate(params, expected_launch_id, start_frame, now_ms)
  end
  if not validated then return nil, kind, message end
  local movie, movie_error = decode_movie(validated.movie, decode_buttons)
  if movie_error then return nil, "bad_params", movie_error end
  return { validated = validated, movie = movie }
end

local function install_movie_input(state, index, boundary)
  if not state.movie then return nil end
  local ok, installed, err = pcall(state.install_input, state.movie[index], 0)
  if not ok or installed == nil or installed == false then
    return mark_terminal(state, "failed", "adapter_error", "unverifiable", boundary,
      "input_apply_failed: " .. tostring(ok and err or installed))
  end
  state.input_owned = true
  state.input_released = false
  return nil
end

function M.start(params, request_id, expected_launch_id, start_frame, now_ms, sink,
    decode_buttons, install_input, release_input, prepared, release_hooks,
    member_sink, capture_initial, wall_ms)
  local kind, message
  if not prepared then
    prepared, kind, message = M.prepare(
      params, expected_launch_id, start_frame, now_ms, decode_buttons)
  end
  if not prepared then return nil, nil, kind, message end
  local validated = prepared.validated
  if type(sink) ~= "table" or type(sink.write) ~= "function" or type(sink.close) ~= "function" then
    return nil, nil, "bad_sink", "sink must provide write and close"
  end
  if #validated.initial_snapshots > 0
      and (type(member_sink) ~= "table" or type(member_sink.write) ~= "function"
        or type(member_sink.close) ~= "function" or type(capture_initial) ~= "function"
        or type(wall_ms) ~= "function") then
    return nil, nil, "bad_sink", "initial snapshot sink callbacks are unavailable"
  end
  local movie = prepared.movie
  if movie and (type(install_input) ~= "function" or type(release_input) ~= "function") then
    return nil, nil, "bad_state", "input movie callbacks are unavailable"
  end

  local state = {
    active = true,
    request_id = request_id,
    capture_id = validated.capture_id,
    launch_id = validated.launch_id,
    request_digest_sha256 = validated.request_digest_sha256,
    origin_frame = start_frame,
    start_frame = start_frame + validated.warmup_frames,
    maximum_end = start_frame + validated.total_frames,
    actual_end = start_frame,
    final_frame = nil,
    last_frame = start_frame - 1,
    frames_requested = params.frames,
    warmup_frames = validated.warmup_frames,
    total_frames = validated.total_frames,
    advanced_frames = 0,
    completed_frames = 0,
    boundary_records = 0,
    completed_records = 0,
    events = 0,
    complete_bytes = 0,
    physical_bytes = 0,
    dropped = 0,
    truncated = false,
    first_sequence_gap = nil,
    limits = validated.limits,
    selected = validated.selected,
    selected_order = validated.selected_order,
    class_armed = {
      frame_boundary = true,
      frame_completed = validated.selected.frame_completed == true,
    },
    class_start = {
      frame_boundary = start_frame,
      frame_completed = validated.selected.frame_completed and start_frame or nil,
    },
    class_counts = {},
    class_dropped = {},
    stop_on = validated.stop_on,
    start_on = validated.start_on,
    initial_snapshots = validated.initial_snapshots,
    observation_start = nil,
    stop_event = nil,
    movie = movie,
    install_input = install_input,
    release_input = release_input,
    input_owned = false,
    input_released = false,
    hooks_owned = false,
    hooks_released = false,
    observation_armed = false,
    observation_started = validated.start_on == nil,
    release_hooks = release_hooks,
    started_ms = now_ms,
    last_progress_ms = now_ms,
    progress_sequence = 0,
    sink = sink,
    sink_closed = false,
    sink_close_attempted = false,
    sink_close_error = nil,
    member_sink = member_sink,
    member_sink_closed = member_sink == nil,
    member_sink_close_attempted = false,
    member_sink_close_error = nil,
    capture_initial = capture_initial,
    wall_ms = wall_ms,
  }
  local effect = install_movie_input(state, 1, start_frame)
  if effect then return state, effect end
  effect = select(1, write_event(state, "frame_boundary", start_frame, start_frame))
  if not effect then
    effect = { kind = validated.warmup_frames == 0 and "arm_observation" or "working" }
  end
  return state, effect
end

function M.attach_hooks(state, hooks_owned)
  if hooks_owned == nil then hooks_owned = true end
  if not state or not state.active
      or (hooks_owned and type(state.release_hooks) ~= "function") then
    return nil, "hook cleanup callback is unavailable"
  end
  state.hooks_owned = hooks_owned == true
  state.observation_armed = true
  for _, id in ipairs(state.selected_order) do
    if id ~= "frame_boundary" and id ~= "frame_completed" then
      state.class_armed[id] = state.start_on == nil
      state.class_start[id] = state.start_on == nil and state.start_frame or nil
    end
  end
  return true
end

function M.tick(state, boundary, now_ms)
  if not state or not state.active then return state, nil end
  if now_ms < state.started_ms or now_ms - state.started_ms > state.limits.max_host_ms then
    return state, mark_terminal(state, "aborted", "adapter_error", "unverifiable", boundary,
      "host_deadline_exceeded", "interrupted")
  end
  local expected = state.origin_frame + state.advanced_frames + 1
  if boundary ~= expected or boundary > state.maximum_end then
    return state, mark_terminal(state, "failed", "adapter_error", "unverifiable", boundary,
      "end_boundary_missed_or_regressed")
  end

  local completion
  if state.selected.frame_completed then
    local effect
    effect, completion = write_event(state, "frame_completed", boundary - 1, boundary)
    if effect then return state, effect end
  end
  state.advanced_frames = state.advanced_frames + 1
  state.actual_end = boundary
  state.completed_frames = math.max(0, boundary - state.start_frame)

  if boundary > state.start_frame and completion then
    state.observation_completed_records = (state.observation_completed_records or 0) + 1
    completion.occurrence = state.observation_completed_records
  end
  if state.stop_on and completion and boundary > state.start_frame
      and completion.occurrence == state.stop_on.occurrence then
    state.stop_event = completion
    return state, mark_terminal(state, "completed", "event_stop", "complete", boundary, nil)
  end
  if boundary == state.maximum_end then
    if state.start_on and not state.observation_started then
      return state, mark_terminal(state, "failed", "adapter_error", "unverifiable", boundary,
        "event_aligned_start_not_observed")
    end
    return state, mark_terminal(state, "completed", "target_reached", "complete", boundary, nil)
  end

  local effect = install_movie_input(state, state.advanced_frames + 1, boundary)
  if effect then return state, effect end
  effect = select(1, write_event(state, "frame_boundary", boundary, boundary))
  if effect then return state, effect end
  if boundary == state.start_frame and not state.observation_armed then
    return state, { kind = "arm_observation" }
  end
  effect = { kind = "none" }
  if now_ms - state.last_progress_ms >= state.limits.progress_interval_ms then
    state.last_progress_ms = now_ms
    effect.kind = "working"
  end
  return state, effect
end

function M.cancel(state, boundary, reason)
  if not state or not state.active then return state, nil end
  return state, mark_terminal(state, "aborted", "adapter_error", "unverifiable", boundary,
    reason or "cancelled", "interrupted")
end

function M.fail_input(state, boundary, reason)
  if not state or not state.active then return state, nil end
  return state, mark_terminal(state, "failed", "adapter_error", "unverifiable", boundary,
    "input_apply_failed: " .. tostring(reason))
end

function M.progress(state)
  local progress = {
    status = "working",
    capture_id = state.capture_id,
    sequence = state.progress_sequence,
    frame = state.actual_end,
    frames = state.advanced_frames,
    events = state.events,
    bytes = state.complete_bytes,
    phase = state.warmup_frames > state.advanced_frames and "warming"
      or (state.start_on and not state.observation_started and "aligning" or "recording"),
  }
  state.progress_sequence = state.progress_sequence + 1
  return progress
end

function M.result(state, now_ms)
  local input_cleanup = "not_acquired"
  if state.movie then input_cleanup = state.input_released and "released" or "unverifiable" end
  local class_facts = {}
  for _, id in ipairs(state.selected_order) do
    local interval = nil
    if (state.warmup_frames > 0 or state.start_on ~= nil) and state.class_armed[id] then
      interval = { f_start = state.class_start[id], f_end = state.actual_end }
    end
    class_facts[#class_facts + 1] = {
      id = id,
      armed = state.class_armed[id] == true,
      armed_interval = interval,
      observed = state.class_counts[id] or 0,
      dropped = state.class_dropped[id] or 0,
    }
  end
  return {
    status = state.status,
    capture_id = state.capture_id,
    operation_outcome = state.operation_outcome,
    execution_outcome = state.execution_outcome,
    integrity = state.integrity,
    reason = state.reason,
    f_origin = (state.warmup_frames > 0 or state.start_on ~= nil) and state.origin_frame or nil,
    f_start = state.start_frame,
    f_end = state.actual_end,
    final_frame = state.final_frame,
    frames = state.completed_frames,
    events = state.events,
    bytes = state.complete_bytes,
    physical_bytes = state.physical_bytes,
    dropped = state.dropped,
    truncated = state.truncated,
    first_sequence_gap = state.first_sequence_gap,
    stop_event = state.stop_event,
    observation_start = state.observation_start,
    event_classes = class_facts,
    wall_ms = math.floor(math.max(0, now_ms - state.started_ms)),
    final_execution_state = "frozen",
    cleanup = {
      hooks = state.hooks_released and "released" or (state.hooks_owned and "unverifiable" or "not_acquired"),
      transient_input = input_cleanup,
      sink = state.sink_closed and state.member_sink_closed and "released" or "unverifiable",
    },
  }
end

return M
