local Memory = {}

local function is_non_negative_integer(value)
  return type(value) == "number" and value >= 0 and value == math.floor(value)
end

local function configured_size(sys, name)
  if name == sys.default_memtype then return sys.address_space_size end
  return sys.region_sizes and sys.region_sizes[name] or nil
end

local function runtime_size(api, sys, name)
  local configured = configured_size(sys, name)
  if configured == nil then return nil end
  local memory_type = api.memType[name]
  if memory_type == nil then return nil end
  if type(api.getMemorySize) ~= "function" then return configured end
  local ok, size = pcall(api.getMemorySize, memory_type)
  if not ok or not is_non_negative_integer(size) or size == 0 then return nil end
  return size
end

function Memory.catalog(api, sys)
  local names = {}
  local function add(name)
    if runtime_size(api, sys, name) then names[#names + 1] = name end
  end
  add(sys.default_memtype)
  for name, _ in pairs(sys.region_sizes or {}) do add(name) end
  table.sort(names)
  return names
end

function Memory.resolve(api, sys, name)
  if type(name) ~= "string" or name == "" then
    return nil, "bad_params", "memory_type must be a non-empty string"
  end
  local size = runtime_size(api, sys, name)
  local memory_type = api.memType[name]
  if size == nil or memory_type == nil then
    return nil, "bad_params", "unsupported or unavailable memory_type: " .. name
  end
  return { name = name, memory_type = memory_type, size = size }
end

function Memory.range(api, sys, name, address, length)
  local region, kind, message = Memory.resolve(api, sys, name)
  if not region then return nil, kind, message end
  if not is_non_negative_integer(address) then
    return nil, "bad_params", "address must be a non-negative integer"
  end
  if not is_non_negative_integer(length) then
    return nil, "bad_params", "length must be a non-negative integer"
  end
  if address > region.size or length > region.size - address then
    return nil, "bad_params", string.format(
      "memory range %s@0x%X+0x%X exceeds region size 0x%X",
      name, address, length, region.size)
  end
  region.address = address
  region.length = length
  return region
end

return Memory
