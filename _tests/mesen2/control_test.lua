package.path=(os.getenv('EMUCAP_ADAPTER_DIR') or '.')..'/?.lua;'..package.path
local Control=require('emucap_control')
local function encode(v)
  if type(v)=='string' then return '"'..v..'"' end
  if type(v)~='table' then return tostring(v) end
  local out={}; for k,x in pairs(v) do out[#out+1]='"'..k..'":'..encode(x) end
  return '{'..table.concat(out,',')..'}'
end
local function key(id) return {runtime='runtime',owner_id='core',operation_id=id or 'parent'} end
local function fixture()
  local s={generation=1,monotonicMs=0,halted=true,ppuCycles=0,instructionBoundaries=0,haltedCpuSteps=0}
  local replies,releases={},{}
  local calls={steps=0,breaks=0,disconnected=0}
  local h={stepType={ppuFrame=1,step=2}}
  function h.getControlState() local t={}; for k,v in pairs(s) do t[k]=v end; return t end
  function h.step(n,unit) calls.steps=calls.steps+1;s.halted=false;s.ppuRemaining=n*100;s.instructionRemaining=n end
  function h.breakExecution() calls.breaks=calls.breaks+1 end
  function h.endPacingWait() end
  local hooks={array=function(v)return v end,frame=function()return 7 end,
    reply=function(id,r)replies[id]={ok=true,result=r}end,
    error=function(id,k,m)replies[id]={ok=false,kind=k,message=m}end,
    legacy_busy=function()return false end,validate_input=function()return true end,
    release=function(port)releases[port]=true;return true end,
    advancing=function()end,stopped=function()end,retired=function()end}
  local c
  hooks.disconnect=function()calls.disconnected=calls.disconnected+1;c:disconnected()end
  c=Control.new('runtime',h,hooks);c:connected(1)
  local function request(id,method,params)
    return c:dispatch(encode({v=1,id=id,method=method,params=params or {}}))
  end
  return c,s,replies,releases,calls,request,hooks
end
local function begin(req) req(1,'begin_temporal_operation',{parent=key()}) end
local function input(req,port) return req(2,'set_input',{_temporal_owner=key(),port=port or 0}) end
local function step(req) req(3,'step',{_temporal_owner=key(),_control=key('child'),frames=60}) end
-- Child cancellation drains only after native halt; finish releases the attempted port.
do
 local c,s,r,ports,calls,req=fixture();begin(req);assert(r[1].result.status=='admitted')
 assert(input(req,2));step(req);c:poll();assert(calls.steps==1)
 req(4,'cancel_operation',key('stale'));assert(r[4].result.status=='not_active' and calls.breaks==0)
 s.ppuCycles=750;s.monotonicMs=100;req(5,'cancel_operation',key('child'))
 assert(r[5].result.status=='requested' and not r[3]);c:poll();assert(not r[3])
 s.halted=true;c:poll();assert(r[3].result.count==7 and r[3].result.status=='interrupted')
 req(6,'finish_temporal_operation',{parent=key()});c:poll()
 assert(r[6].result.cleanup_verified and ports[2] and not ports[0])
 req(7,'finish_temporal_operation',{parent=key()});assert(r[7].result.cleanup_verified)
end
-- Loss between child phases still owns native input, while unowned holds survive.
for _,owned in ipairs({true,false}) do
 local c,s,r,ports,calls,req=fixture()
 if owned then begin(req);input(req,1) else assert(req(1,'set_input',{port=1})) end
 c:disconnected();c:poll();assert((ports[1]==true)==owned)
 c:connected(2);req(9,'begin_temporal_operation',{parent=key('next')});assert(r[9].ok)
end
-- Failed input attempts still require cleanup. A release failure retires across reconnect.
do
 local c,s,r,ports,calls,req,hooks=fixture();begin(req);assert(input(req))
 hooks.release=function()return false,'native input unavailable'end
 req(6,'finish_temporal_operation',{parent=key()});c:poll();assert(c:retired() and not r[6].ok)
 c:disconnected();c:connected(2);req(7,'begin_temporal_operation',{parent=key('next')})
 assert(not r[7].ok)
end
-- Parent cleanup shares the original child stop budget, even after a stopped terminal.
do
 local c,s,r,ports,calls,req=fixture();begin(req);input(req);step(req);c:poll()
 s.monotonicMs=100;req(5,'cancel_operation',key('child'));s.halted=true;c:poll()
 s.monotonicMs=5101;req(6,'finish_temporal_operation',{parent=key()});c:poll()
 assert(c:retired() and not ports[0] and not r[6].ok)
end
-- Debugger replacement must not receive an old operation's stop or release.
do
 local c,s,r,ports,calls,req=fixture();begin(req);input(req);step(req);c:poll()
 s.generation=2;c:poll();assert(c:retired() and calls.breaks==0 and not ports[0])
end
-- Broker stamps are required and registration is pinned on the physical connection.
do
 local c,s,r,ports,calls,req=fixture()
 local a={broker_instance='broker',registration=1,session=2}
 c:dispatch(encode({_control_session={kind='attach',runtime='runtime',attachment=a}}))
 req(1,'begin_temporal_operation',{parent=key()});assert(not r[1].ok)
 req(2,'begin_temporal_operation',{parent=key(),_control_attachment=a});assert(r[2].ok)
 local stale={broker_instance='broker',registration=1,session=1}
 c:dispatch(encode({_control_session={kind='detach',runtime='runtime',attachment=stale}}))
 assert(c:owns_parent())
 c:dispatch(encode({_control_session={kind='detach',runtime='runtime',attachment=a}}));c:poll()
 assert(not c:owns_parent())
 a.registration=2
 c:dispatch(encode({_control_session={kind='attach',runtime='runtime',attachment=a}}))
 assert(calls.disconnected==1)
end
-- A normal initial pause does not spend the later advance's cancellation budget.
do
 local c,s,r,ports,calls,req=fixture();s.halted=false;begin(req)
 req(8,'pause',{_temporal_owner=key()});s.halted=true;c:poll();assert(r[8].ok)
 s.monotonicMs=6000;input(req);step(req);c:poll()
 req(5,'cancel_operation',key('child'));s.halted=true;c:poll()
 req(6,'finish_temporal_operation',{parent=key()});c:poll()
 assert(r[6] and r[6].ok and not c:retired())
end
-- Port release exceptions are uncertainty, never successful cleanup.
do
 local c,s,r,ports,calls,req,hooks=fixture();begin(req);input(req)
 hooks.release=function()error('device disappeared')end
 req(6,'finish_temporal_operation',{parent=key()});c:poll()
 assert(c:retired() and not r[6].ok)
end
-- EOF during an advance suppresses the old response and releases only its input.
do
 local c,s,r,ports,calls,req=fixture();begin(req);input(req,3);step(req);c:poll()
 c:disconnected();assert(c:busy());s.halted=true;c:poll()
 assert(not r[3] and ports[3] and not c:busy())
 c:connected(2);req(9,'begin_temporal_operation',{parent=key('next')});assert(r[9].ok)
end
-- A cancelled child cannot admit another advance before parent cleanup.
do
 local c,s,r,ports,calls,req=fixture();begin(req);step(req);c:poll()
 req(4,'cancel_operation',key('child'));s.halted=true;c:poll()
 req(9,'step',{_temporal_owner=key(),_control=key('later'),frames=1})
 assert(not r[9].ok and calls.steps==1)
end
-- Late cancellation of a replied input request leaves Core's cleanup admissible.
do
 local c,s,r,ports,calls,req=fixture();begin(req);assert(input(req))
 req(4,'cancel_operation',key());assert(r[4].result.status=='not_active')
 assert(input(req));req(6,'finish_temporal_operation',{parent=key()});c:poll()
 assert(r[6].ok and ports[0])
end
-- Native selection/policy validation must precede owned effects, just as on legacy dispatch.
for _,bad_policy in ipairs({false,true}) do
 local c,s,r,ports,calls,req,hooks=fixture();begin(req)
 if bad_policy then hooks.healthy=function()return false end
 else hooks.validate=function(method)if method=='step' then return 'unknown cpu' end end end
 step(req);assert(not r[3].ok and calls.steps==0)
 hooks.healthy=nil;hooks.validate=nil
 req(6,'finish_temporal_operation',{parent=key()});c:poll()
 assert(r[6].ok and not r[6].result.effects_started)
end
-- Integral JSON floats retain ordinary step semantics without crashing Lua.
do
 local c,s,r,ports,calls,req=fixture();begin(req)
 req(3,'step',{_temporal_owner=key(),_control=key('child'),frames=1.0})
 c:poll();s.ppuCycles=100;s.halted=true;c:poll()
 assert(r[3].ok and r[3].result.count==1)
end
print('mesen owned dispatcher tests passed')
