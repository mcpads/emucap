local dir = os.getenv('EMUCAP_ADAPTER_DIR') or '.'
package.path = dir .. '/?.lua;' .. package.path
local Json, Wire, Rx = require('emucap_json'), require('emucap_control_wire'), require('emucap_rx')
local parent = '{"runtime":"native","owner_id":"owner","operation_id":"parent"}'
local child = '{"runtime":"native","owner_id":"owner","operation_id":"child"}'
local attachment = '{"broker_instance":"broker","registration":18446744073709551615,"session":18446744073709551614}'
local other = '{"broker_instance":"broker","registration":18446744073709551615,"session":18446744073709551615}'
local function request(method, params)
  return '{"v":1,"id":42,"method":"' .. method .. '","params":' .. params .. '}'
end
local event = assert(Wire.decode('{"_control_session":{"kind":"attach","runtime":"native","attachment":' .. attachment .. '}}'))
assert(event.kind == 'lifecycle' and event.event.kind == 'attach')
assert(Wire.same_attachment(event.event.attachment, Json.decode(attachment)))
assert(not Wire.same_attachment(event.event.attachment, Json.decode(other)))
local admitted = assert(Wire.decode(request('begin_temporal_operation', '{"parent":' .. parent .. ',"_temporal_owner":' .. parent .. '}')))
assert(admitted.parent.operation_id == 'parent')
local controlled = assert(Wire.decode(request('step', '{"frames":10,"_control":' .. child .. ',"_temporal_owner":' .. parent .. ',"_control_attachment":' .. attachment .. '}')))
assert(not Wire.authenticate(controlled, Json.decode(other), true))
assert(not Wire.authenticate(controlled, Json.decode(attachment), false))
assert(Wire.authenticate(controlled, Json.decode(attachment), true))
assert(controlled.params._control_attachment == nil)
local params = Wire.native_params(controlled)
assert(params.frames == 10 and params._control == nil and params._temporal_owner == nil)
local direct = assert(Wire.decode(request('step', '{"count":1,"unit":"instructions","_control":' .. child .. '}')))
assert(not Wire.authenticate(direct, Json.decode(attachment), true))
assert(Wire.authenticate(direct, Json.decode(attachment), false))
assert(Wire.decode(request('status', 'null')))
for _, input in ipairs({
  request('status', '[]'), request('status', '1'),
  request('begin_temporal_operation', '{"parent":' .. parent .. ',"_temporal_owner":' .. child .. '}'),
  request('step', '{"_control":' .. parent .. ',"_temporal_owner":' .. parent .. '}'),
  request('pause', '{"_control":' .. child .. '}'),
  request('status', '{"_control_session":{}}'), request('_control_session', '{}'),
  '{"v":1,"id":1,"method":"status","params":{},"_control_attachment":' .. attachment .. '}',
  '{"_control_session":{"kind":"attach","runtime":"native","attachment":' .. attachment .. '},"id":1}',
  '{"_control_session":{"kind":"steal","runtime":"native","attachment":' .. attachment .. '}}',
  '{"v":1.0,"id":1,"method":"status","params":{}}',
  '{"v":1,"id":-1,"method":"status","params":{}}',
}) do assert(Wire.decode(input) == nil, input) end
assert(Json.is_object(Json.decode('{}')) and not Json.is_array(Json.decode('{}')))
assert(Json.is_array(Json.decode('[]')) and not Json.is_object(Json.decode('[]')))

local function socket(chunks)
  local state = { calls = 0, maximum_read = 0 }
  function state:receive(n)
    assert(n > 0 and n <= 4096)
    self.maximum_read = math.max(self.maximum_read, n)
    self.calls = self.calls + 1
    local part = table.remove(chunks, 1)
    if not part then return nil, 'timeout', '' end
    return table.unpack(part, 1, 3)
  end
  return state
end
local rx, sock = Rx.new(32), socket({{nil,'timeout','ab'},{nil,'timeout','c\ndef\n'}})
assert(Rx.poll(rx,sock) == nil)
assert(Rx.poll(rx,sock) == 'abc')
assert(Rx.poll(rx,sock) == 'def' and sock.calls == 2, 'queued frames need no new socket read')
assert(Rx.poll(rx,sock) == nil)
rx = Rx.new(3); sock = socket({{nil,'timeout','abc\n'}})
assert(Rx.poll(rx,sock) == 'abc', 'cap excludes newline')
rx = Rx.new(3); sock = socket({{nil,'timeout','abcd'}})
local line, err = Rx.poll(rx,sock); assert(line == nil and err == 'too_large')
Rx.reset(rx); assert(rx.bytes == '')
sock = socket({{nil,'closed','abc'}})
line,err = Rx.poll(rx,sock); assert(line == nil and err == 'closed')
rx = Rx.new(8192); sock = socket({{nil,'timeout',string.rep('a',4096)}})
assert(Rx.poll(rx,sock) == nil and sock.maximum_read == 4096)
rx = Rx.new(32); sock = socket({{nil,'timeout','old\nstale\n'}})
assert(Rx.poll(rx,sock) == 'old')
Rx.reset(rx)
assert(Rx.poll(rx,socket({})) == nil, 'replacement connection must not inherit buffered requests')
-- Entering owned control tightens admission without losing already queued bytes.
rx = Rx.new(8192); rx.bytes = string.rep('a', 4097)
line,err = Rx.poll(rx,socket({}),4096)
assert(line == nil and err == 'too_large')
rx.bytes = string.rep('a',4097) .. '\n'
line,err = Rx.poll(rx,socket({}),4096)
assert(line == nil and err == 'too_large')
Rx.reset(rx); rx.bytes = 'cancel\nnext\n'
assert(Rx.poll(rx,socket({}),4096) == 'cancel')
assert(Rx.poll(rx,socket({}),4096) == 'next')
print('Mesen control wire authentication and bounded framing: passed')
