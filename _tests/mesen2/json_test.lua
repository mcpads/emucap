local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Json = require("emucap_json")

-- Correlation must distinguish adjacent full-width broker identities. Compare
-- against their decimal wire values, never a floating conversion of the oracle.
for _, token in ipairs({ '0', '1', '9007199254740991', '9007199254740992',
  '9007199254740993', '9223372036854775807', '9223372036854775808',
  '18446744073709551614', '18446744073709551615' }) do
  local env = Json.decode('{"registration":' .. token .. '}')
  assert(Json.unsigned_text(env.registration) == token, token)
end
assert(Json.unsigned_text(Json.decode('1.0')) == nil)
assert(Json.unsigned_text(Json.decode('1e0')) == nil)
assert(Json.unsigned_text(Json.decode('-1')) == nil)
assert(Json.unsigned_text('1') == nil)
assert(Json.unsigned_text({text = '1'}) == nil)
assert(Json.decode('-9223372036854775808') == math.mininteger)

local request = Json.decode([[ {"id":7,"method":"step","params":{"frames":30,
  "rate":1.25e2,"ok":true,"off":false,"values":[null,2,null],"empty":null}} ]])
assert(request.id == 7 and request.method == 'step' and request.params.frames == 30)
assert(request.params.rate == 125 and request.params.ok and request.params.off == false)
assert(#request.params.values == 3 and request.params.values[2] == 2)
assert(request.params.values[1] == Json.null and request.params.empty == Json.null)
assert(Json.decode('"\\ud83d\\ude00"') == utf8.char(0x1f600))
assert(Json.decode('"한글\\n"') == '한글\n')

-- Fail closed before any lifecycle/effect dispatch, including duplicate null
-- fields and escape-equivalent keys. Depth and malformed numbers must terminate.
local invalid = { '', 'truefalse', 'nul', 'false x', '01', '-01', '+1', '.1',
  '1.', '1e', '1e+', 'NaN', '1e9999', '18446744073709551616',
  '-9223372036854775809', '[1,]', '{"a":1,}', '{"a" 1}', '{a:1}',
  '{"a":null,"a":2}', '{"a":1,"\\u0061":2}', '{}[]',
  '"\\q"', '"\\uZZZZ"', '"\\ud800"', '"\\udc00"',
  '"\\ud800\\u0041"', '"a\nb"', '"' .. string.char(0xff) .. '"',
  string.rep('[', 66) .. '0' .. string.rep(']', 66) }
for _, input in ipairs(invalid) do
  assert(not pcall(Json.decode, input), 'accepted malformed input: ' .. input)
end
print('Mesen JSON identity and request validation: passed')
