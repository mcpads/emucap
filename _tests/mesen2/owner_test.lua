local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Json, Owner = require("emucap_json"), require("emucap_owner")
local function a(session)
  return Json.decode('{"broker_instance":"broker","registration":18446744073709551615,"session":' .. (session or '18446744073709551614') .. '}')
end
local function key(id) return {runtime = 'native', owner_id = 'core', operation_id = id or 'parent'} end
local function rejected(expected, ok, err) assert(ok == false and err == expected, tostring(err)) end
local function active()
  local owner = assert(Owner.new('native'))
  assert(owner:attach(a()))
  assert(owner:begin(a(), key()))
  return owner
end
-- Detach between children owns every attempted write, even if a native write
-- failed. Replacement stays blocked until exact stop/release proof is supplied.
do
  local owner = active()
  assert(owner:effect(a(), key(), 2))
  assert(owner:effect(a(), key(), 2))
  local ok, plan = owner:detach(a()); assert(ok and plan.input_ports['2'])
  assert(owner:authorize_cleanup(plan))
  assert(plan.effects_started and not plan.input_ports['0'])
  rejected('busy', owner:attach(a('18446744073709551615')))
  rejected('wrong_attachment', owner:effect(a(), key(), 0))
  assert(owner:finish_cleanup(plan, {stop_verified = true, released_ports = {['2'] = true}}))
  assert(owner:attach(a('18446744073709551615')))
  assert(owner:begin(a('18446744073709551615'), key('next')))
  rejected('not_active', owner:authorize_cleanup(plan))
  local stale, no_plan = owner:detach(a()); assert(stale and no_plan == nil)
  assert(owner:effect(a('18446744073709551615'), key('next')))
end
-- Exact evidence is mandatory; missing stop, missing port, or extra release all
-- retire control across reconnect. Stale plans cannot retire a newer operation.
for _, evidence in ipairs({{released_ports = {['2'] = true}},
    {stop_verified = true, released_ports = {}},
    {stop_verified = true, released_ports = {['1'] = true, ['2'] = true}}}) do
  local owner = active(); assert(owner:effect(a(), key(), 2))
  local ok, plan = owner:detach(a()); assert(ok)
  rejected('retired', owner:finish_cleanup(plan, evidence))
  assert(owner:is_retired())
  rejected('retired', owner:authorize_cleanup(plan))
  rejected('retired', owner:attach(a('1')))
  rejected('retired', owner:begin(a(), key('other')))
  rejected('retired', owner:finish_cleanup(plan, {stop_verified = true, released_ports = {['2'] = true}}))
end
-- An unused admission and a step-only parent acquire no controller release.
for _, effect in ipairs({false, true}) do
  local owner = active()
  if effect then assert(owner:effect(a(), key())) end
  local ok, plan = owner:start_cleanup(a(), key()); assert(ok and next(plan.input_ports) == nil)
  rejected('not_active', owner:effect(a(), key(), 0))
  assert(owner:finish_cleanup(plan, {stop_verified = effect, released_ports = {}}))
  assert(owner:terminal(a(), key()))
  rejected('not_active', owner:begin(a(), key()))
  assert(owner:begin(a(), key('next')))
  assert(owner:terminal(a(), key()) == nil)
  rejected('not_active', owner:finish_cleanup(plan, {}))
  assert(not owner:is_retired())
end
-- Admission is idempotent and all identity checks happen before obligations.
do
  local owner = active()
  assert(owner:begin(a(), key()))
  rejected('busy', owner:begin(a(), key('other')))
  rejected('wrong_attachment', owner:effect(a('18446744073709551615'), key()))
  local wrong = key(); wrong.runtime = 'replacement'
  rejected('invalid_identity', owner:begin(a(), wrong))
  rejected('busy', owner:authorize_unscoped(a(), false))
  assert(owner:authorize_unscoped(a(), true))
  local ok, plan = owner:detach(a()); assert(ok and not plan.effects_started)
  assert(owner:finish_cleanup(plan, {released_ports = {}}))
  rejected('invalid_identity', owner:attach(a('0')))
  assert(owner:attach(a('1')))
  assert(owner:authorize_unscoped(a('1'), false))
end
-- Callers cannot mutate the ledger through an admission argument or returned
-- cleanup/terminal snapshot. Extra plan fields do not constitute the same plan.
do
  local owner = assert(Owner.new('native')); local attached, parent = a(), key()
  assert(owner:attach(attached)); assert(owner:begin(attached, parent))
  attached.broker_instance = 'changed'; parent.operation_id = 'changed'
  assert(owner:effect(a(), key(), 0))
  local ok, plan = owner:start_cleanup(a(), key()); assert(ok)
  plan.input_ports['1'] = true
  rejected('not_active', owner:finish_cleanup(plan, {}))
  ok, plan = owner:start_cleanup(a(), key()); assert(ok and not plan.input_ports['1'])
  assert(owner:finish_cleanup(plan, {stop_verified = true, released_ports = {['0'] = true}}))
  local terminal = owner:terminal(a(), key()); terminal.key.operation_id = 'changed'
  assert(owner:terminal(a(), key()).key.operation_id == 'parent')
end
for _, raw in ipairs({{}, {broker_instance='broker',registration='1',session=1},
    {broker_instance='broker',registration=1,session=1.0},
    {broker_instance='broker',registration=1,session=1,extra=true}}) do
  rejected('invalid_identity', assert(Owner.new('native')):attach(raw))
end
assert(Owner.new('') == nil)
assert(Owner.new(string.rep('x',257)) == nil)
assert(Owner.key({runtime='native',owner_id='core',operation_id='p',extra=true}) == nil)
assert(Owner.child_matches(key(), key('child')))
assert(not Owner.child_matches(key(), key()))
local foreign = key('child'); foreign.owner_id = 'other'
assert(not Owner.child_matches(key(), foreign))
print('Mesen producer parent ownership: passed')
