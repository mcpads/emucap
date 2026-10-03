#!/usr/bin/env python3
"""Check PCSX2 native nominal-policy ownership when external settings change."""
import argparse
from pathlib import Path
import subprocess
import tempfile
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,required=True)
a=p.parse_args();s=a.source.read_text()
def body(sig):
 start=s.index(sig);i=s.index('{',start)+1;depth=1
 while depth:
  depth+=(s[i]=='{')-(s[i]=='}');i+=1
 return s[start:i]
code=r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
using u32=uint32_t;
struct Speed {float NominalScalar=1,TurboScalar=2;bool operator==(const Speed&o)const{return NominalScalar==o.NominalScalar&&TurboScalar==o.TurboScalar;}};
struct Pcsx2Config {Speed EmulationSpeed;} EmuConfig;
struct {void WriteLn(const char*){}} Console;
enum class LimiterModeType {Nominal,Unlimited};
static LimiterModeType s_limiter_mode=LimiterModeType::Nominal;
static float s_emucap_nominal_speed=0,target=1;
static int updates=0;
namespace VMManager {
 float GetNominalSpeed();bool SetEmucapSpeedPercent(u32);void CheckForEmulationSpeedConfigChanges(const Pcsx2Config&);
 void UpdateTargetSpeed(){++updates;target=s_limiter_mode==LimiterModeType::Unlimited?0:GetNominalSpeed();}
 void SetLimiterMode(LimiterModeType mode){s_limiter_mode=mode;UpdateTargetSpeed();}
}
'''+ '\n'.join(body(sig) for sig in ['float VMManager::GetNominalSpeed()','bool VMManager::SetEmucapSpeedPercent(u32 percent)','void VMManager::CheckForEmulationSpeedConfigChanges(const Pcsx2Config& old_config)'])+r'''
int main(){
 assert(VMManager::SetEmucapSpeedPercent(400));assert(target==4);
 auto old=EmuConfig;EmuConfig.EmulationSpeed.NominalScalar=2;
 VMManager::CheckForEmulationSpeedConfigChanges(old);
 assert(VMManager::GetNominalSpeed()==2 && target==2);
 assert(VMManager::SetEmucapSpeedPercent(50));assert(target==.5f);
 old=EmuConfig;EmuConfig.EmulationSpeed.TurboScalar=3;
 VMManager::CheckForEmulationSpeedConfigChanges(old);assert(target==.5f);
 int before=updates;VMManager::CheckForEmulationSpeedConfigChanges(EmuConfig);assert(updates==before);
 assert(VMManager::SetEmucapSpeedPercent(0));old=EmuConfig;EmuConfig.EmulationSpeed.NominalScalar=1.5f;
 VMManager::CheckForEmulationSpeedConfigChanges(old);assert(target==0 && s_limiter_mode==LimiterModeType::Unlimited);
 VMManager::SetLimiterMode(LimiterModeType::Nominal);assert(target==1.5f);
 assert(!VMManager::SetEmucapSpeedPercent(10001));assert(target==1.5f);
 puts("PS2 native config: latest nominal writer, unrelated edits, no-op and mode preservation passed");
}
'''
with tempfile.TemporaryDirectory() as t:
 f=Path(t)/'probe.cpp';f.write_text(code)
 subprocess.run(['c++','-std=c++17','-fsanitize=address,undefined',str(f),'-o',t+'/probe'],check=True)
 subprocess.run([t+'/probe'],check=True)
