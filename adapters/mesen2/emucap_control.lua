-- Authenticated producer control. All hooks and native calls run on the emulation owner.
local Owner, Wire = require('emucap_owner'), require('emucap_control_wire')
local Advance, Step = require('emucap_control_advance'), require('emucap_step')
local M = {}
local READ = {hello=true, status=true, get_rom_info=true, poll_events=true,
  list_breakpoints=true, get_state=true, read_memory=true, read_memory_batch=true,
  screenshot=true, disassemble=true, call_stack=true, get_trace=true}
local function same(a,b)
  return a and b and a.runtime==b.runtime and a.owner_id==b.owner_id and a.operation_id==b.operation_id
end
function M.new(runtime, host, hooks)
  local owner = assert(Owner.new(runtime))
  local attachment, broker, pinned, epoch
  local child, pause, cleanup, stop_deadline, generation
  local self = {}
  local function observe()
    local ok,s = pcall(host.getControlState)
    if not ok or type(s)~='table' or math.type(s.generation)~='integer'
      or math.type(s.monotonicMs)~='integer' or type(s.halted)~='boolean' then return nil end
    if generation and generation~=s.generation then return nil end
    return s
  end
  local function reply(r, value, err)
    if r and r.epoch==epoch and Wire.same_attachment(r.attachment,attachment) then
      if err then hooks.error(r.id, err, value) else hooks.reply(r.id,value) end
    end
  end
  local function fail(message)
    owner:retire()
    local s=observe()
    if s and not s.halted then pcall(host.breakExecution); pcall(host.endPacingWait) end
    if child then reply(child.request,message,'temporal_unverified') end
    if pause then reply(pause,message,'temporal_unverified') end
    if cleanup then reply(cleanup.request,message,'temporal_unverified') end
    child,pause,cleanup=nil,nil,nil
    hooks.retired(message)
  end
  local function stop()
    local s=observe(); if not s then fail('native generation or observation changed'); return false end
    generation=generation or s.generation
    stop_deadline=stop_deadline or s.monotonicMs+5000
    if s.monotonicMs>stop_deadline then fail('native cleanup deadline exceeded'); return false end
    if not s.halted then
      local ok=pcall(host.breakExecution); local wake=pcall(host.endPacingWait)
      if not ok or not wake then fail('native stop request failed'); return false end
    end
    return true
  end
  local function terminal(plan)
    local ports={}; for port in pairs(plan.input_ports) do ports[#ports+1]=tonumber(port) end
    table.sort(ports)
    local r={status='completed',parent=plan.key,cleanup_verified=true,
      released_ports=hooks.array(ports),effects_started=plan.effects_started}
    if plan.effects_started then r.state='frozen' end
    return r
  end
  local function start_cleanup(plan, request)
    cleanup={plan=plan,request=request}
    if child then
      local ok,err=child.driver:cancel('cancelled'); if not ok then fail(err); return end
      stop_deadline=stop_deadline or child.driver:stop_deadline()
    end
    if plan.effects_started then stop() end
  end
  function self:connected(value)
    epoch=value; broker=false; pinned=nil; attachment=nil
  end
  function self:disconnected()
    local old=attachment; attachment=nil
    if old then
      local ok,plan=owner:detach(old)
      if not ok then fail(plan); return end
      if plan then start_cleanup(plan) end
    end
    if child then
      local ok,err=child.driver:cancel('connection_closed')
      stop_deadline=stop_deadline or child.driver:stop_deadline()
      if not ok then fail(err) end
    end
  end
  function self:busy() return child~=nil or pause~=nil or cleanup~=nil end
  function self:owns_parent() return owner:active_key()~=nil end
  function self:retired() return owner:is_retired() end
  function self:input_failure(message) fail(message) end
  function self:interrupt(why, bp)
    if not child then return false end
    child.breakpoint_id=bp
    local ok,err=child.driver:cancel(why)
    stop_deadline=stop_deadline or child.driver:stop_deadline()
    if not ok then fail(err) end
    return true
  end
  function self:decorate(r)
    r.control_session_lifecycle=true
    r.temporal_cancellation_capability={methods=hooks.array({'step','step_instructions'}),
      control_service_ms=50,stop_host_ms=5000}
    if r.methods then r.methods[#r.methods+1]='cancel_operation' end
    return r
  end
  function self:poll()
    if owner:is_retired() then return end
    if child then
      local r,err=child.driver:poll()
      if err then fail(err); return end
      if r then
        if r.stop_deadline_ms then stop_deadline=stop_deadline or r.stop_deadline_ms end
        r.frame=hooks.frame(); r.breakpoint_id=child.breakpoint_id
        hooks.stopped(r)
        reply(child.request,r)
        child=nil
        if not owner:active_key() and not cleanup and not pause then generation=nil; stop_deadline=nil end
      end
    end
    if pause or cleanup then
      local s=observe()
      if not s or (stop_deadline and s.monotonicMs>stop_deadline) then
        fail('native cleanup proof expired or changed'); return
      end
      local plan=cleanup and cleanup.plan
      if not s.halted and (not plan or plan.effects_started) then return end
      if pause then
        local normal_stop=pause.normal_stop
        hooks.stopped({reason='paused'}); reply(pause,{state='frozen'}); pause=nil
        if normal_stop and not cleanup then stop_deadline=nil end
      end
      if plan then
        local ok,err=owner:authorize_cleanup(plan); if not ok then fail(err); return end
        local released={}
        for port in pairs(plan.input_ports) do
          s=observe()
          if not s or not s.halted or (stop_deadline and s.monotonicMs>stop_deadline) then
            fail('input release authority expired'); return
          end
          local called,wrote,why=pcall(hooks.release,tonumber(port))
          if not called or not wrote then fail(why or 'input release failed'); return end
          released[port]=true
        end
        s=observe()
        if not s or (plan.effects_started and not s.halted)
          or (stop_deadline and s.monotonicMs>stop_deadline) then fail('input cleanup proof expired'); return end
        ok,err=owner:finish_cleanup(plan,{stop_verified=s.halted,released_ports=released})
        if not ok then fail(err); return end
        if plan.effects_started then hooks.stopped({reason='temporal_cleanup'}) end
        reply(cleanup.request,terminal(plan)); cleanup=nil; generation=nil; stop_deadline=nil
      end
    end
  end
  function self:dispatch(line)
    local r,err=Wire.decode(line)
    if not r then hooks.error(0,'bad_params',err); return nil end
    if r.kind=='lifecycle' then
      local e=r.event; local a=Owner.attachment_key(e.attachment)
      if e.runtime~=runtime or (pinned and (pinned.broker_instance~=a.broker_instance
        or pinned.registration~=a.registration)) then hooks.disconnect(); return nil end
      if e.kind=='detach' then
        if Wire.same_attachment(attachment,e.attachment) then self:disconnected() end
        return nil
      end
      if cleanup or child or pause then hooks.disconnect(); return nil end
      if attachment and not broker then
        if owner:active_key() then hooks.disconnect(); return nil end
        owner:detach(attachment); attachment=nil
      end
      local ok; ok,err=owner:attach(e.attachment)
      if not ok then hooks.disconnect(); return nil end
      pinned=a; broker=true; attachment=e.attachment
      return nil
    end
    -- Bootstrap hello precedes the broker's first authenticated attachment.
    if r.method=='hello' and not attachment and not r.parent and not r.child
      and not r.params._control_attachment then return r end
    if not attachment and not broker and not self:busy() and not owner:is_retired() then
      attachment={broker_instance='direct',registration=1,session=epoch}
      local ok; ok,err=owner:attach(attachment)
      if not ok then attachment=nil; hooks.error(r.id,'busy',err); return nil end
    end
    if not Wire.authenticate(r,attachment,broker) then hooks.error(r.id,'bad_state','wrong control attachment'); return nil end
    r.attachment=attachment; r.epoch=epoch
    if owner:is_retired() then reply(r,'native control retired; restart session','temporal_unverified'); return nil end
    if hooks.healthy and not hooks.healthy() then
      reply(r,'native policy is unverified; restart session','temporal_unverified'); return nil
    end
    if hooks.validate then
      local valid,why=pcall(hooks.validate,r.method,r.params)
      if not valid or why then reply(r,why or 'invalid request','bad_params'); return nil end
    end
    if r.method=='cancel_operation' then
      local key=Owner.key(r.params)
      if not key then reply(r,'invalid cancel identity','bad_params'); return nil end
      local requested=false
      if child and same(key,child.key) then requested=self:interrupt('cancelled')
      elseif same(key,owner:active_key()) and (pause or cleanup) then
        requested=true
        -- A synchronous parent request that already replied is no longer active.
        -- Its late cancel must not close the parent before Core releases input.
        if pause then pause.normal_stop=false; stop() end
      end
      reply(r,{status=requested and 'requested' or 'not_active'}); return nil
    end
    if r.method=='finish_temporal_operation' then
      local retained=owner:terminal(attachment,r.parent)
      if retained then reply(r,terminal(retained)); return nil end
      if cleanup then reply(r,'cleanup in progress','busy'); return nil end
      local ok,plan=owner:start_cleanup(attachment,r.parent)
      if not ok then reply(r,plan,'bad_state'); return nil end
      start_cleanup(plan,r); return nil
    end
    if self:busy() and r.method~='hello' and r.method~='status' then reply(r,'owned execution in progress','busy'); return nil end
    if r.method=='begin_temporal_operation' then
      if hooks.legacy_busy() then reply(r,'legacy execution in progress','busy'); return nil end
      local fresh=owner:active_key()==nil
      local ok; ok,err=owner:begin(attachment,r.parent)
      if not ok then reply(r,err,'busy'); return nil end
      if fresh then generation=nil; stop_deadline=nil end
      reply(r,{status='admitted',parent=r.parent}); return nil
    end
    local ok
    if r.parent then ok,err=owner:authorize(attachment,r.parent)
    else ok,err=owner:authorize_unscoped(attachment,READ[r.method]==true) end
    if not ok then reply(r,err,'busy'); return nil end
    if r.child then
      if stop_deadline then reply(r,'cancelled parent awaits cleanup','busy'); return nil end
      if r.child.runtime~=runtime or hooks.legacy_busy() then reply(r,'invalid controlled execution admission','bad_state'); return nil end
      local unit,count,why=Step.parse_wire_step(r.method,r.params,5000)
      if not unit then reply(r,why,'bad_params'); return nil end
      count=math.tointeger(count) -- legacy step accepts integral JSON float syntax
      local s=observe()
      if not s or not s.halted then reply(r,'native halt required','not_paused'); return nil end
      generation=s.generation
      if r.parent then owner:effect(attachment,r.parent) end
      hooks.advancing()
      child={request=r,key=r.child,driver=Advance.new(host,unit,count)}
      return nil
    end
    if r.parent and not READ[r.method] then
      if r.method~='set_input' and r.method~='pause' then reply(r,'method outside parent operation','unsupported'); return nil end
      local port
      if r.method=='set_input' then
        local called,valid,why=pcall(hooks.validate_input,r.params)
        if not called or not valid then reply(r,why or 'invalid input','bad_params'); return nil end
        port=r.params.port or 0
      end
      local s=observe()
      if not s then reply(r,'native generation changed','temporal_unverified'); fail('native generation changed'); return nil end
      generation=s.generation
      ok,err=owner:effect(attachment,r.parent,port)
      if not ok then reply(r,err,'bad_state'); return nil end
      if r.method=='pause' then pause=r; pause.normal_stop=stop_deadline==nil; stop(); return nil end
    end
    return r
  end
  return self
end
return M
