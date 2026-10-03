local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Input = require("emucap_input")
local valid = { a = true, b = true, start = true }
local devices = {
  [0] = { a = false, b = false, start = false, x = 17 },
  [1] = { a = true, b = true, start = true },
}
local writes = 0
local host = {
  -- Mesen's native API leaves keys absent from the supplied table unchanged.
  setInput = function(state, port)
    writes = writes + 1
    if devices[port] then
      for name, value in pairs(state) do devices[port][name] = value end
    end
  end,
  getInput = function(port) return devices[port] or {} end,
}
local a = { a = true }
assert(Input.apply(host, valid, a, 0))
assert(devices[0].a and not devices[0].b)
assert(Input.apply(host, valid, {b = true}, 0))
assert(not devices[0].a and devices[0].b, 'replacement must release preceding buttons')
assert(Input.apply(host, valid, {}, 0))
assert(not devices[0].a and not devices[0].b and not devices[0].start)
assert(devices[0].x == 17, 'unowned numeric controls must survive')
assert(devices[1].a and devices[1].b and devices[1].start, 'other port must survive')
assert(a.a and a.b == nil, 'caller state must remain unchanged')
assert(not Input.apply(host, valid, {}, 2), 'absent device cannot prove release')
local before = writes
assert(not Input.apply(host, valid, {unknown = true}, 0))
assert(not Input.apply(host, valid, {a = 1}, 0))
assert(writes == before, 'invalid state must fail before native write')
devices[0].a = true
host.setInput = function() end
assert(not Input.apply(host, valid, {}, 0), 'ignored release cannot succeed')
host.setInput = function() error('write failed') end
assert(not Input.apply(host, valid, {}, 0))
host.setInput = function() end
host.getInput = function() error('readback failed') end
assert(not Input.apply(host, valid, {}, 0))
print('Mesen native input replacement and release: passed')
