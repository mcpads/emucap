-- Development-only runtime probe for an instruction-aligned SNES WRAM snapshot.
--
-- This is not an emucap adapter or advertised capability. It measures whether the maintained
-- Mesen host can read and stream a bounded memory member from the same native pre-instruction
-- callback without approaching the script watchdog. The harness supplies an already-listening
-- loopback sink and verifies the received byte count and digest outside Mesen.

local socket = require("socket.core")

local marker = assert(os.getenv("EMUCAP_PROBE_MARKER"), "EMUCAP_PROBE_MARKER is required")
local sink_port = assert(tonumber(os.getenv("EMUCAP_PROBE_SINK_PORT")),
  "EMUCAP_PROBE_SINK_PORT is required")
local length = tonumber(os.getenv("EMUCAP_PROBE_LENGTH") or "131072")
assert(length and length >= 1 and length <= 0x20000 and length == math.floor(length),
  "EMUCAP_PROBE_LENGTH must be in 1..131072")
assert(emu.eventType.snesInstructionExecution,
  "maintained Mesen SNES instruction callback is unavailable")
assert(emu.memType.snesWorkRam, "SNES WRAM memory type is unavailable")

local sink = assert(socket.tcp())
assert(sink:settimeout(1.0))
assert(sink:connect("127.0.0.1", sink_port))

local function send_all(bytes)
  local offset = 1
  while offset <= #bytes do
    local sent, err, partial = sink:send(bytes, offset)
    if sent then
      offset = sent + 1
    elseif partial and partial >= offset then
      offset = partial + 1
    else
      error("snapshot sink write failed: " .. tostring(err))
    end
  end
end

local fired = false
emu.addEventCallback(function()
  if fired then return end
  fired = true

  local event = assert(emu.getSnesObservationEvent(), "instruction event payload is unavailable")
  local started = socket.gettime()
  local chunks = {}
  local chunk_size = 4096
  for base = 0, length - 1, chunk_size do
    local bytes = {}
    local count = math.min(chunk_size, length - base)
    for offset = 0, count - 1 do
      bytes[offset + 1] = string.char(emu.read(
        base + offset, emu.memType.snesWorkRam, false))
    end
    chunks[#chunks + 1] = table.concat(bytes)
  end
  local read_done = socket.gettime()
  for _, chunk in ipairs(chunks) do send_all(chunk) end
  local send_done = socket.gettime()
  sink:close()

  local output = assert(io.open(marker, "wb"))
  output:write(string.format(
    '{"bytes":%d,"pc":%d,"master_clock":%d,"read_ms":%.3f,"send_ms":%.3f,"total_ms":%.3f}\n',
    length, event.pc, event.masterClock,
    (read_done - started) * 1000,
    (send_done - read_done) * 1000,
    (send_done - started) * 1000))
  output:close()
  emu.exit(0)
end, emu.eventType.snesInstructionExecution)
