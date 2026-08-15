-- Convert Mesen's bounded native debugger stack to the compact emucap response shape.
-- Mesen returns frames outermost-first; emucap puts the frozen PC first and walks outward.

local NativeCallstack = {}

local MAX_DEPTH = 64
local NATIVE_CAPACITY = 511

local function frame_kind(flags)
  if flags == 1 then return "nmi" end
  if flags == 2 then return "irq" end
  return "call"
end

function NativeCallstack.capture(current_pc, current_thumb, native_frames)
  local frames = {
    { pc = current_pc, kind = "pc", thumb = current_thumb and true or false },
  }
  local available = type(native_frames) == "table" and #native_frames or 0
  local first = math.max(1, available - (MAX_DEPTH - 2))
  for index = available, first, -1 do
    local source = native_frames[index]
    frames[#frames + 1] = {
      pc = source.source,
      kind = frame_kind(source.flags),
      target = source.target,
      return_address = source.returnAddress,
      return_stack_pointer = source.returnStackPointer,
      absolute_source = source.absoluteSource,
      source_memory_type = source.sourceMemoryType,
      absolute_target = source.absoluteTarget,
      target_memory_type = source.targetMemoryType,
      absolute_return = source.absoluteReturn,
      return_memory_type = source.returnMemoryType,
    }
  end
  return {
    frames = frames,
    depth = #frames,
    order = "innermost_to_outermost",
    method = "mesen-native-callstack",
    authority = "best_effort",
    cpu = "arm7tdmi",
    max_depth = MAX_DEPTH,
    native_depth = available,
    truncated = available > (MAX_DEPTH - 1) or available >= NATIVE_CAPACITY,
  }
end

return NativeCallstack
