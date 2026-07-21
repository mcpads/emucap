-- license:GPL-2.0-or-later
-- Structural PoC only: print the runtime surfaces required before the Neo Geo bridge advertises them.

local subscriptions = {}
local reported = false

local function sorted_keys(container)
  local keys = {}
  if container == nil then
    return keys
  end
  for key, _ in pairs(container) do
    keys[#keys + 1] = tostring(key)
  end
  table.sort(keys)
  return keys
end

local function join(values)
  return table.concat(values, ",")
end

local function report()
  if reported or not manager or not manager.machine then
    return
  end
  reported = true

  local machine = manager.machine
  print("EMUCAP_POC machine=" .. tostring(machine.system.name))

  for tag, device in pairs(machine.devices) do
    local spaces = sorted_keys(device.spaces)
    local states = sorted_keys(device.state)
    if #spaces > 0 or #states > 0 then
      print(string.format(
        "EMUCAP_POC device=%s shortname=%s spaces=%s states=%s",
        tostring(tag), tostring(device.shortname), join(spaces), join(states)))
    end
    if tostring(tag) == ":maincpu" and device.spaces and device.spaces["program"] then
      local space = device.spaces["program"]
      print(string.format(
        "EMUCAP_POC address_space=maincpu.program mask=%X width=%s endian=%s shift=%s",
        tonumber(space.address_mask), tostring(space.data_width), tostring(space.endianness),
        tostring(space.shift)))
      if space.map then
        for _, entry in ipairs(space.map.entries) do
          local read_type = entry.read and entry.read.handlertype or "none"
          local write_type = entry.write and entry.write.handlertype or "none"
          print(string.format(
            "EMUCAP_POC map=%X-%X mirror=%X read=%s write=%s share=%s region=%s",
            tonumber(entry.address_start), tonumber(entry.address_end),
            tonumber(entry.address_mirror), tostring(read_type), tostring(write_type),
            tostring(entry.share), tostring(entry.region)))
        end
      end
    end
  end

  for tag, screen in pairs(machine.screens) do
    local frame = screen.frame_number
    if type(frame) == "function" then
      frame = frame(screen)
    end
    print(string.format(
      "EMUCAP_POC screen=%s frame=%s width=%s height=%s",
      tostring(tag), tostring(frame), tostring(screen.width), tostring(screen.height)))
  end

  for tag, port in pairs(machine.ioport.ports) do
    local fields = {}
    for _, field in pairs(port.fields) do
      fields[#fields + 1] = string.format(
        "%s|player=%s|type=%s|class=%s",
        tostring(field.name), tostring(field.player), tostring(field.type), tostring(field.type_class))
    end
    table.sort(fields)
    if #fields > 0 then
      print("EMUCAP_POC port=" .. tostring(tag) .. " fields=" .. join(fields))
    end
  end
end

-- Autoboot scripts begin after the machine is available. `emu.register_start` belongs to the
-- plugin bootstrap environment and is not present here.
report()
subscriptions[#subscriptions + 1] = emu.add_machine_reset_notifier(report)
