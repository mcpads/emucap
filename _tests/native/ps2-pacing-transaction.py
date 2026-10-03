#!/usr/bin/env python3
"""Exercise maintained PCSX2 pacing methods and PINE transaction with controlled CPU dispatch."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, required=True, help='patched pcsx2 directory')
a = p.parse_args()
s = (a.source / 'VMManager.cpp').read_text()

def body(signature):
    start = s.index(signature)
    brace = s.index('{', start)
    i, depth = brace + 1, 1
    while depth:
        depth += (s[i] == '{') - (s[i] == '}')
        i += 1
    return s[start:i]

pine = (a.source / 'PINE.cpp').read_text()
start = pine.index('\t\t\tcase MsgEmuCapPacing:')
end = pine.index('\n\t\t\tdefault:', start)
handler = pine[start:end]
code = r'''
#include <algorithm>
#include <cassert>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <functional>
#include <optional>
#include <span>
#include <vector>
using u8=uint8_t; using u32=uint32_t; using u64=uint64_t; using s64=int64_t;
enum class LimiterModeType {Nominal,Slomo,Turbo,Unlimited};
static LimiterModeType s_limiter_mode=LimiterModeType::Nominal;
static float s_target_speed=1, s_emucap_nominal_speed=0;
static bool s_target_speed_can_sync_to_host, s_target_speed_synced_to_host, s_use_vsync_for_timing, s_limiter_restarted;
static s64 s_limiter_ticks_per_frame;
static u64 g_FrameCount=23;
static bool valid=true, fast_boot=false;
static float refresh_rate=60;static std::function<void()> on_refresh;
static int notifications=0, callbacks=0;
static std::function<void()> before_dispatch, after_dispatch;
struct {
 bool EnableFastBootFastForward=true;
 struct {float NominalScalar=1,SlomoScalar=.5,TurboScalar=2; bool SyncToHostRefreshRate=false,UseVSyncForTiming=false;} EmulationSpeed;
 struct {bool SkipDuplicateFrames=false,VsyncEnable=false;} GS;
} EmuConfig;
struct Logger {template<class... T> void WriteLn(T...) {}} Console,DevCon;
namespace fmt {template<class... T> int format(T...) {return 0;}}
#define ERROR_LOG(...) ((void)0)
#define ASSUME(x) assert(x)
u64 GetTickFrequency(){return 1000000;}
std::optional<float> GSGetHostRefreshRate(){float result=refresh_rate;if(on_refresh)on_refresh();return result;}
namespace MTGS {void UpdateVSyncMode(){++notifications;}}
namespace SPU2 {void OnTargetSpeedChanged(){}}
namespace VMManager {
 struct EmuCapPacingSnapshot {u32 mode,nominal;float target;u64 revision,frame;};
 EmuCapPacingSnapshot GetEmuCapPacingSnapshot();
 u32 ApplyEmuCapPacing(u32,EmuCapPacingSnapshot&,EmuCapPacingSnapshot&);
 LimiterModeType GetLimiterMode();void SetLimiterMode(LimiterModeType);
 float GetTargetSpeed();float GetNominalSpeed();float GetTargetSpeedForLimiterMode(LimiterModeType);
 void UpdateTargetSpeed();bool SetEmucapSpeedPercent(u32);
 bool HasValidVM(){return valid;} float GetFrameRate(){return 60;} void ResetFrameLimiter(){}
 namespace Internal {bool IsFastBootInProgress(){return fast_boot;}}
}
namespace Host {
 void RunOnCPUThread(std::function<void()> fn,bool block){
  assert(block);++callbacks;
  if(before_dispatch)before_dispatch();fn();if(after_dispatch)after_dispatch();
 }
}
template<class T> T FromSpan(std::span<u8> b,u32 at){T n;std::memcpy(&n,b.data()+at,sizeof(n));return n;}
template<class T> void ToResultVector(std::vector<u8>& b,T n,u32 at){b.resize(at+sizeof(n));std::memcpy(b.data()+at,&n,sizeof(n));}
bool SafetyChecks(u32 at,u32 count,u32 ret,u32 result,u32 size){return at+count<=size && ret+result<=4096;}
constexpr u8 MsgEmuCapPacing=0x95,MsgEmuCapSetPacing=0x96;
''' + '\n'.join(body(x) for x in [
 'LimiterModeType VMManager::GetLimiterMode()', 'void VMManager::SetLimiterMode(',
 'float VMManager::GetTargetSpeed()', 'float VMManager::GetTargetSpeedForLimiterMode(',
 'void VMManager::UpdateTargetSpeed()', 'float VMManager::GetNominalSpeed()',
 'VMManager::EmuCapPacingSnapshot VMManager::GetEmuCapPacingSnapshot()',
 'bool VMManager::SetEmucapSpeedPercent(', 'u32 VMManager::ApplyEmuCapPacing(']) + r'''
std::vector<u8> dispatch(std::vector<u8> input){
 std::span<u8> buf(input);u32 buf_cnt=1,ret_cnt=0,buf_size=buf.size();std::vector<u8> ret_buffer;
 const auto command=buf[0]; switch(command) {
''' + handler + r'''
 default: error: return {};
 }
 return ret_buffer;
}
std::vector<u8> set(u32 percent){std::vector<u8> b{MsgEmuCapSetPacing};ToResultVector(b,percent,1);auto reply=dispatch(b);if(reply.empty())return reply;
 assert(reply.size()==60);u32 outcome=FromSpan<u32>(reply,0);
 assert(outcome==(fast_boot && percent>0 ? 1u:0u));reply.erase(reply.begin(),reply.begin()+4);return reply;}
u64 revision(std::vector<u8> b,u32 offset=0){return FromSpan<u64>(b,offset+12);}
u32 nominal(std::vector<u8> b,u32 offset=0){return FromSpan<u32>(b,offset+4);}
int main(){
 auto initial=dispatch({MsgEmuCapPacing});assert(initial.size()==28 && nominal(initial)==10000);
 // An actual settings update can run between the diagnostic query and the transaction.
 before_dispatch=[] {EmuConfig.EmulationSpeed.NominalScalar=2.5;VMManager::UpdateTargetSpeed();};
 after_dispatch=[] {VMManager::SetEmucapSpeedPercent(400);};
 int prior_callbacks=callbacks;
 auto result=set(50);assert(callbacks==prior_callbacks+1 && result.size()==56);
 assert(nominal(result)==25000 && nominal(result,28)==5000);
 assert(revision(result,28)==revision(result)+1);
 assert(FromSpan<u64>(result,20)==23 && FromSpan<u64>(result,48)==23);
 before_dispatch=nullptr;after_dispatch=nullptr;
 assert(nominal(dispatch({MsgEmuCapPacing}))==40000); // Later writer preserved.
 // Native A->B->A changes must advance revision even without a PINE observation.
 auto before=VMManager::GetEmuCapPacingSnapshot();
 VMManager::SetLimiterMode(LimiterModeType::Turbo);VMManager::SetLimiterMode(LimiterModeType::Nominal);
 assert(VMManager::GetEmuCapPacingSnapshot().revision==before.revision+2);
 for(u32 percent:{0u,1u,100u,10000u}){
  auto reply=set(percent);assert(reply.size()==56);
  auto same=set(percent);assert(revision(same)==revision(same,28));
 }
 auto unchanged=dispatch({MsgEmuCapPacing});
 for(u32 bad:{10001u,UINT32_MAX})assert(set(bad).empty());
 assert(dispatch({MsgEmuCapSetPacing,1}).empty());
 assert(dispatch({MsgEmuCapPacing})==unchanged);
 before_dispatch=[] {valid=false;};assert(set(50).empty());before_dispatch=nullptr;valid=true;
 assert(dispatch({MsgEmuCapPacing})==unchanged);
 fast_boot=true;VMManager::UpdateTargetSpeed();auto overridden=set(50);
 assert(FromSpan<u32>(overridden,36)==0 && nominal(overridden)==nominal(overridden,28)); // Native custom target stays visible, never fabricated success.
 // Host-refresh constraints can change independently. Failed restoration must remain explicit.
 fast_boot=false;s_emucap_nominal_speed=1;EmuConfig.EmulationSpeed.SyncToHostRefreshRate=true;
 refresh_rate=59;VMManager::UpdateTargetSpeed();
 VMManager::EmuCapPacingSnapshot prior{},final{};
 on_refresh=[] {refresh_rate=60;};
 assert(VMManager::ApplyEmuCapPacing(100,prior,final)==2);
 assert(prior.target!=final.target);
 on_refresh=nullptr;
 puts("PCSX2 native transaction passed: previous identity, CPU serialization, revision, admission, override");
}
'''
with tempfile.TemporaryDirectory() as d:
    source = Path(d) / 'test.cpp'
    binary = Path(d) / 'test'
    source.write_text(code)
    subprocess.run(['clang++', '-std=c++20', '-fsanitize=address,undefined', '-g', str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
