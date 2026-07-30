local Dump = {}

local function write_file(open_file, path, data)
  local file, open_error = open_file(path, "wb")
  if not file then
    return false, "cannot open " .. path .. ": " .. tostring(open_error)
  end

  local wrote, write_error = file:write(data)
  if not wrote then
    file:close()
    return false, "cannot write " .. path .. ": " .. tostring(write_error)
  end

  local closed, close_error = file:close()
  if not closed then
    return false, "cannot close " .. path .. ": " .. tostring(close_error)
  end
  return true
end

local function json_escape(value)
  return (value:gsub('[%c"\\]', function(char)
    if char == '"' then return '\\"' end
    if char == '\\' then return '\\\\' end
    return string.format('\\u%04x', string.byte(char))
  end))
end

function Dump.write(params, regions, api, open_file)
  if type(params) ~= "table" or type(params.path) ~= "string" or params.path == "" then
    return false, "bad_params", "path is required"
  end
  if type(regions) ~= "table" then
    return false, "internal_error", "dump region list is missing"
  end

  api = api or emu
  open_file = open_file or io.open
  local metas = {}
  for _, region in ipairs(regions) do
    local memory_type = api.memType[region.mt]
    if memory_type == nil then
      return false, "internal_error", "unknown dump memory type: " .. tostring(region.mt)
    end

    local buffer = {}
    for offset = 0, region.size - 1 do
      buffer[offset + 1] = string.char(api.read(offset, memory_type, false))
    end

    local path = params.path .. "/" .. region.name .. ".bin"
    local ok, err = write_file(open_file, path, table.concat(buffer))
    if not ok then return false, "io_error", err end
    metas[#metas + 1] = string.format(
      '{"name":"%s","memory_type":"%s","base_address":%d,"size":%d}',
      json_escape(region.name), json_escape(region.mt), region.base, region.size)
  end

  local ok, err = write_file(
    open_file, params.path .. "/regions.json", "[" .. table.concat(metas, ",") .. "]")
  if not ok then return false, "io_error", err end
  return true, { path = params.path, regions = #regions }
end

return Dump
