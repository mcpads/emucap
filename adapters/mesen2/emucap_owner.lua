-- Producer parent ownership. The caller authenticates transport metadata first;
-- native effects and their stop/release evidence remain on the emulation owner.
local Json = require("emucap_json")
local M = {}

local function part(value)
  return type(value) == "string" and #value > 0 and #value <= 256
end
local function fields(value, allowed)
  if type(value) ~= "table" then return false end
  local count = 0
  for key in pairs(value) do
    if not allowed[key] then return false end
    count = count + 1
  end
  return count == 3
end
local KEY_FIELDS = { runtime = true, owner_id = true, operation_id = true }
local ATTACHMENT_FIELDS = { broker_instance = true, registration = true, session = true }
function M.key(value)
  if not fields(value, KEY_FIELDS) or not part(value.runtime)
    or not part(value.owner_id) or not part(value.operation_id) then return nil end
  return { runtime = value.runtime, owner_id = value.owner_id, operation_id = value.operation_id }
end
local function attachment(value)
  if not fields(value, ATTACHMENT_FIELDS) or not part(value.broker_instance) then return nil end
  local registration = Json.unsigned_text(value.registration)
  local session = Json.unsigned_text(value.session)
  if not registration or registration == "0" or not session or session == "0" then return nil end
  return { broker_instance = value.broker_instance, registration = registration, session = session }
end
-- Canonical comparison data; callers retain the original decoded attachment
-- when invoking ledger entrypoints, which validate numeric wire types.
M.attachment_key = attachment
local function copy(value)
  if type(value) ~= "table" then return value end
  local result = {}
  for key, member in pairs(value) do result[key] = copy(member) end
  return result
end
local function equal(a, b)
  if type(a) ~= type(b) then return false end
  if type(a) ~= "table" then return a == b end
  for key, member in pairs(a) do if not equal(member, b[key]) then return false end end
  for key in pairs(b) do if a[key] == nil then return false end end
  return true
end
function M.child_matches(parent, child)
  parent, child = M.key(parent), M.key(child)
  return parent ~= nil and child ~= nil and parent.runtime == child.runtime
    and parent.owner_id == child.owner_id and parent.operation_id ~= child.operation_id
end

function M.new(runtime)
  if not part(runtime) then return nil, "invalid_identity" end
  local current, parent, retained
  local stopping, retired = false, false
  local self = {}
  local function authorize(raw_attachment, raw_key)
    if retired then return false, "retired" end
    local a, key = attachment(raw_attachment), M.key(raw_key)
    if not a or not key then return false, "invalid_identity" end
    if not equal(a, current) then return false, "wrong_attachment" end
    if not parent or stopping or not equal(parent.key, key) then return false, "not_active" end
    return true
  end
  function self:attach(raw)
    if retired then return false, "retired" end
    local a = attachment(raw)
    if not a then return false, "invalid_identity" end
    if equal(a, current) then return true end
    if parent or current then return false, "busy" end
    current, retained = a, nil
    return true
  end
  function self:begin(raw_attachment, raw_key)
    if retired then return false, "retired" end
    local a, key = attachment(raw_attachment), M.key(raw_key)
    if not a or not key or key.runtime ~= runtime then return false, "invalid_identity" end
    if not equal(a, current) then return false, "wrong_attachment" end
    if parent then
      if equal(parent.key, key) and not stopping then return true end
      return false, "busy"
    end
    if retained and equal(retained.key, key) then return false, "not_active" end
    retained = nil
    parent = { key = key, attachment = a, input_ports = {}, effects_started = false }
    stopping = false
    return true
  end
  function self:authorize(raw_attachment, raw_key)
    return authorize(raw_attachment, raw_key)
  end
  function self:authorize_unscoped(raw_attachment, observation)
    if retired then return false, "retired" end
    local a = attachment(raw_attachment)
    if not a then return false, "invalid_identity" end
    if not equal(a, current) then return false, "wrong_attachment" end
    if parent and observation ~= true then return false, "busy" end
    return true
  end
  function self:effect(raw_attachment, raw_key, port)
    local ok, err = authorize(raw_attachment, raw_key)
    if not ok then return false, err end
    if port ~= nil then
      local name = Json.unsigned_text(port)
      if not name then return false, "invalid_identity" end
      parent.input_ports[name] = true
    end
    parent.effects_started = true
    return true
  end
  function self:start_cleanup(raw_attachment, raw_key)
    if retired then return false, "retired" end
    local a, key = attachment(raw_attachment), M.key(raw_key)
    if not a or not key then return false, "invalid_identity" end
    if not parent or not equal(parent.key, key) or not equal(parent.attachment, a) then
      return false, "not_active"
    end
    stopping = true
    return true, copy(parent)
  end
  function self:detach(raw_attachment)
    if retired then return false, "retired" end
    local a = attachment(raw_attachment)
    if not a then return false, "invalid_identity" end
    if not equal(a, current) then return true end
    current, retained = nil, nil
    if parent then
      stopping = true
      return true, copy(parent)
    end
    return true
  end
  function self:authorize_cleanup(plan)
    if retired then return false, "retired" end
    if not parent or not stopping or not equal(parent, plan) then return false, "not_active" end
    return true
  end
  function self:finish_cleanup(plan, evidence)
    local ok, err = self:authorize_cleanup(plan)
    if not ok then return false, err end
    if type(evidence) ~= "table"
      or (parent.effects_started and evidence.stop_verified ~= true)
      or not equal(parent.input_ports, evidence.released_ports) then
      retired = true
      return false, "retired"
    end
    if equal(current, parent.attachment) then retained = copy(parent) end
    parent = nil
    return true
  end
  function self:terminal(raw_attachment, raw_key)
    if retired then return nil end
    local a, key = attachment(raw_attachment), M.key(raw_key)
    if retained and equal(retained.attachment, a) and equal(retained.key, key) then
      return copy(retained)
    end
  end
  function self:active_key() return parent and copy(parent.key) or nil end
  function self:is_retired() return retired end
  function self:retire() retired = true end
  return self
end
return M
