-- Decode internal ownership envelopes. Authentication is against the transport
-- attachment supplied by the session owner, never against caller-chosen IDs.
local Json, Owner = require('emucap_json'), require('emucap_owner')
local M = {}
local function count(t)
  local n = 0
  for _ in pairs(t) do n = n + 1 end
  return n
end
local function part(s) return type(s) == 'string' and #s > 0 and #s <= 256 end
local function same_key(a, b)
  return a and b and a.runtime == b.runtime and a.owner_id == b.owner_id
    and a.operation_id == b.operation_id
end
function M.same_attachment(a, b)
  a, b = Owner.attachment_key(a), Owner.attachment_key(b)
  return a ~= nil and b ~= nil and a.broker_instance == b.broker_instance
    and a.registration == b.registration and a.session == b.session
end
function M.decode(line)
  local ok, env = pcall(Json.decode, line)
  if not ok or not Json.is_object(env) then return nil, 'invalid envelope' end
  if env._control_session ~= nil then
    local event = env._control_session
    if count(env) ~= 1 or not Json.is_object(event) or count(event) ~= 3
      or (event.kind ~= 'attach' and event.kind ~= 'detach') or not part(event.runtime)
      or not Owner.attachment_key(event.attachment) then return nil, 'invalid lifecycle event' end
    return { kind = 'lifecycle', event = event }
  end
  if env._control_attachment ~= nil or math.type(env.v) ~= 'integer' or env.v ~= 1
    or math.type(env.id) ~= 'integer' or env.id < 0 or type(env.method) ~= 'string'
    or env.method == '_control_session' then return nil, 'invalid request envelope' end
  local p = env.params
  if p == Json.null then p = {} elseif not Json.is_object(p) then return nil, 'invalid params' end
  if p._control_session ~= nil then return nil, 'reserved lifecycle field' end
  local request = { kind = 'request', id = env.id, method = env.method, params = p }
  if p._control ~= nil then
    if env.method ~= 'step' and env.method ~= 'step_instructions' then return nil, 'invalid controlled method' end
    request.child = Owner.key(p._control)
    if not request.child then return nil, 'invalid child identity' end
  end
  local parent_method = env.method == 'begin_temporal_operation' or env.method == 'finish_temporal_operation'
  if parent_method then
    if request.child or not Owner.key(p.parent) then return nil, 'invalid parent admission' end
    if p._temporal_owner ~= nil and not same_key(Owner.key(p._temporal_owner), Owner.key(p.parent)) then
      return nil, 'mismatched parent identities'
    end
  end
  if p._temporal_owner ~= nil or parent_method then
    request.parent = Owner.key(p._temporal_owner or p.parent)
    if not request.parent then return nil, 'invalid parent identity' end
    if request.child and not Owner.child_matches(request.parent, request.child) then
      return nil, 'child does not belong to parent'
    end
  end
  return request
end
function M.authenticate(request, current, broker)
  if not current or not Owner.attachment_key(current) then return false end
  local stamped = request.params._control_attachment
  if broker then
    if not stamped or not M.same_attachment(stamped, current) then return false end
  elseif stamped ~= nil then return false end
  request.params._control_attachment = nil
  return true
end
function M.native_params(request)
  local result = {}
  for key, value in pairs(request.params) do
    if key ~= '_control' and key ~= '_temporal_owner' and key ~= '_control_attachment' then
      result[key] = value
    end
  end
  return result
end
return M
