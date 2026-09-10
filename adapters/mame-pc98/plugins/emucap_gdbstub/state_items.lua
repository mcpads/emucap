-- license:BSD-3-Clause
-- MAME save-manager item serialization; guest execution is owned by the bridge.

local function save_item_supported(size)
  return size == 1 or size == 2 or size == 4 or size == 8
end

local function save_items_to_dir(path)
  -- Refresh device-owned serialization buffers at this frozen guest boundary.
  -- A host without the native presave hook must fail rather than save stale buffers.
  manager.machine:prepare_state_save()
  local manifest_path = path .. "/manifest.txt"
  local manifest, manifest_err = io.open(manifest_path, "wb")
  if not manifest then
    return nil, "manifest open failed: " .. tostring(manifest_err)
  end

  local saved = 0
  local skipped = 0
  local idx = 0
  while true do
    local item = emu.item(idx)
    if not item or item.size == 0 then
      break
    end

    if item.count > 0 then
      local filename = string.format("item_%06d.bin", idx)
      local bytes_len = item.size * item.count
      local ok, data_or_err = pcall(function() return item:read_block(0, bytes_len) end)
      if not ok then
        manifest:close()
        return nil, "item read failed at " .. tostring(idx) .. ": " .. tostring(data_or_err)
      end
      if type(data_or_err) ~= "string" or #data_or_err ~= bytes_len then
        manifest:close()
        return nil, "item read length mismatch at " .. tostring(idx)
      end
      local f, file_err = io.open(path .. "/" .. filename, "wb")
      if not f then
        manifest:close()
        return nil, "item file open failed: " .. tostring(file_err)
      end
      local written, write_err = f:write(data_or_err)
      local closed, close_err = f:close()
      if not written or not closed then
        manifest:close()
        return nil, "item write failed: " .. tostring(write_err or close_err)
      end
      local recorded, record_err = manifest:write(string.format("%d|%d|%d|%d|%s\n", idx, item.size, item.count, bytes_len, filename))
      if not recorded then
        manifest:close()
        return nil, "manifest write failed: " .. tostring(record_err)
      end
      saved = saved + 1
      if not save_item_supported(item.size) then
        skipped = skipped + 1
      end
    end
    idx = idx + 1
  end

  local closed, close_err = manifest:close()
  if not closed then
    return nil, "manifest close failed: " .. tostring(close_err)
  end
  return saved, skipped
end

local function value_from_bytes(data, pos, size)
  local value = 0
  for offset = 0, size - 1 do
    value = value | ((data:byte(pos + offset) or 0) << (offset * 8))
  end
  return value
end

local function load_items_from_dir(path)
  local manifest, manifest_err = io.open(path .. "/manifest.txt", "rb")
  if not manifest then
    return nil, "manifest open failed: " .. tostring(manifest_err)
  end

  local restored = 0
  local skipped = 0
  for line in manifest:lines() do
    local idx_s, size_s, count_s, bytes_s, filename = line:match("^(%d+)|(%d+)|(%d+)|(%d+)|([^|]+)$")
    local idx = tonumber(idx_s or "")
    local size = tonumber(size_s or "")
    local count = tonumber(count_s or "")
    local bytes_len = tonumber(bytes_s or "")
    if not idx or not size or not count or not bytes_len or not filename then
      manifest:close()
      return nil, "bad manifest line: " .. tostring(line)
    end

    local item = emu.item(idx)
    if not item or item.size ~= size or item.count ~= count then
      manifest:close()
      return nil, "save item mismatch at " .. tostring(idx)
    end

    local f, file_err = io.open(path .. "/" .. filename, "rb")
    if not f then
      manifest:close()
      return nil, "item file open failed: " .. tostring(file_err)
    end
    local data = f:read("*a")
    f:close()
    if #data ~= bytes_len then
      manifest:close()
      return nil, "item data length mismatch at " .. tostring(idx)
    end

    if count > 0 and save_item_supported(size) then
      for entry = 0, count - 1 do
        item:write(entry, value_from_bytes(data, (entry * size) + 1, size))
      end
      restored = restored + 1
    elseif count > 0 then
      skipped = skipped + 1
    end
  end

  manifest:close()
  return restored, skipped
end

return { save = save_items_to_dir, load = load_items_from_dir }
