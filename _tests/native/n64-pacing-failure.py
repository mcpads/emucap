#!/usr/bin/env python3
"""Execute extracted N64 pacing methods against a controlled native transaction owner."""
from pathlib import Path
import json
import os
import subprocess
import tempfile
import tomllib
root=Path(__file__).resolve().parents[2]
source=(root/'src/n64_adapter_observation.rs').read_text()
prefix=source[:source.index('impl Mupen64PlusHost {')].replace('use super::*;', '').replace('crate::live::', 'emucap::live::')
prefix='\n'.join(line for line in prefix.splitlines() if not line.startswith('//!'))
methods=source[source.index('    pub(super) fn execution_speed_capability()'):source.index('    pub(super) fn read_memory_batch(')]
setup=r'''
use std::ffi::c_int;
use std::sync::atomic::{AtomicBool,AtomicU64,AtomicI32,Ordering};
use std::time::Duration;
use serde_json::{json,Value};
use emucap::n64_adapter::N64Error;
type N64Result<T> = Result<T,N64Error>;
struct Api { debug_get_state: unsafe extern "C" fn(c_int)->c_int, core_emucap_pacing: unsafe extern "C" fn(u32,u32,i32,*mut NativePacingResult,u32)->c_int }
struct Mupen64PlusHost { api: Api, frozen: bool, frame_paused: bool, display: bool }
impl Mupen64PlusHost {
 fn require_connected(&self)->N64Result<()> { Ok(()) }
 fn public_frame(&self)->u64 { 7 }
 fn drain_debug_update(&mut self)->N64Result<Option<Value>> {
  if OWNER.lock().unwrap().2==7 { return Err(N64Error::BadState("stop attribution failure".into())); }
  Ok(None)
 }
}
fn check_core(_: &str, result:c_int)->N64Result<()> {
 if result==0 { Ok(()) } else { Err(N64Error::BadState("injected core failure".into())) }
}
const M64P_DBG_RUN_STATE:i32=1;
const M64P_DBG_RUNSTATE_PAUSED:i32=0;
static RUN_STATE:AtomicI32=AtomicI32::new(2);
static FRAME_GATE:AtomicBool=AtomicBool::new(false);
fn frame_gate_is_blocked()->bool { FRAME_GATE.load(Ordering::Acquire) }
unsafe extern "C" fn debug_state(_:c_int)->c_int { RUN_STATE.load(Ordering::Acquire) }
static OWNER: std::sync::Mutex<(i32,i32,i32,u32)> = std::sync::Mutex::new((100,1,0,0));
unsafe extern "C" fn core(version:u32,operation:u32,percent:i32,data:*mut NativePacingResult,size:u32)->c_int {
 assert_eq!(version,1); assert_eq!(size,std::mem::size_of::<NativePacingResult>() as u32);
 let mut owner=OWNER.lock().unwrap();
 if operation==0 && owner.2==5 { return 1; }
 let mut previous=NativePolicy {factor:owner.0,limiter:owner.1,revision:7,..NativePolicy::default()};
 let mut applied=previous;
 let mut outcome=0;
 if operation==1 {
   owner.3+=1;
   if owner.2==1 { return 1; }
   if owner.2==6 { RUN_STATE.store(0,Ordering::Release); }
   if owner.2==11 { previous.factor=250; }
   if owner.2==9 { previous.netplay=1; applied=previous; outcome=1; }
   else {
     applied=previous;
     if owner.2!=2 { if percent!=0 {applied.factor=percent;} applied.limiter=i32::from(percent!=0); }
     if owner.2==4 {applied.limiter=0;}
     if owner.2==12 {applied.fast_forward=1;}
     if owner.2==13 {applied.netplay=1;}
     applied.revision=previous.revision+u64::from(!previous.same_policy(applied));
     if owner.2==3 {applied.revision+=1;}
     if owner.2==10 {outcome=1;}
   }
   owner.0=applied.factor; owner.1=applied.limiter;
 }
 unsafe { *data=NativePacingResult {version:if owner.2==8 {2}else{1},outcome,previous,applied}; }
 0
}
'''
check=r'''
pub fn run() {
 for fault in [0,1,2,3,4,6,7,9,10,11,12,13] {
  for frozen in [false,true] {
   CONTROL_UNVERIFIED.store(false,Ordering::Release);
   *OWNER.lock().unwrap()=(100,1,fault,0);
   RUN_STATE.store(if frozen {0} else {2},Ordering::Release);
   let mut host=Mupen64PlusHost {api:Api {debug_get_state:debug_state,core_emucap_pacing:core},frozen,frame_paused:false,display:true};
   let result=host.execution_speed(&json!({"mode":"limited","percent":50}));
   assert_eq!(result.is_ok(),[0,6,11].contains(&fault),"fault {fault}");
   if let Ok(ref value)=result { assert_eq!(value["state"],if frozen || fault==6 {"frozen"} else {"running"}); }
   assert_eq!(CONTROL_UNVERIFIED.load(Ordering::Acquire),![0,6,9,11].contains(&fault));
   assert_eq!(OWNER.lock().unwrap().3,1,"one native transaction; no speculative rollback");
   if fault==11 {assert_eq!(result.as_ref().unwrap()["previous"]["percent"],250,"native previous must replace probe");}
   if let Err(error)=result { assert!(error.to_string().contains(if fault==9 {"rejected without mutation"} else {"unverified"})); }
  }
 }
 for gate in [false,true] {
  CONTROL_UNVERIFIED.store(false,Ordering::Release);
  *OWNER.lock().unwrap()=(100,1,0,0);
  RUN_STATE.store(2,Ordering::Release);
  FRAME_GATE.store(gate,Ordering::Release);
  let mut host=Mupen64PlusHost {api:Api {debug_get_state:debug_state,core_emucap_pacing:core},frozen:true,frame_paused:gate,display:true};
  let value=host.execution_speed(&json!({"mode":"limited","percent":50})).unwrap();
  assert_eq!(value["state"],if gate {"frozen"} else {"running"});
 }
 FRAME_GATE.store(false,Ordering::Release);
 for (fault, percent) in [(5,50), (8,50), (0,0)] {
  CONTROL_UNVERIFIED.store(false,Ordering::Release);
  *OWNER.lock().unwrap()=(100,1,fault,0);
  let mut host=Mupen64PlusHost {api:Api {debug_get_state:debug_state,core_emucap_pacing:core},frozen:true,frame_paused:false,display:true};
  assert!(host.execution_speed(&json!({"mode":"limited","percent":percent})).is_err());
  assert!(!CONTROL_UNVERIFIED.load(Ordering::Acquire));
  assert_eq!(OWNER.lock().unwrap().3,0,"pre-mutation failure wrote native settings");
 }
 println!("PASS N64: running/frozen policy faults, concurrent stop, stop-attribution failure, frame gate and stale state");
}
'''
with tempfile.TemporaryDirectory(prefix='emucap-n64-policy-') as temp:
    project=Path(temp);(project/'src').mkdir()
    (project/'Cargo.toml').write_text('[package]\nname="n64-policy-owner-check"\nversion="0.0.0"\nedition="2021"\n[dependencies]\nemucap={path='+json.dumps(str(root))+'}\nserde_json="1"\n')
    (project/'src/main.rs').write_text('#[allow(dead_code,unused_imports)] mod checked {\n'+setup+prefix+'\nimpl Mupen64PlusHost {\n'+methods+'\n}\n'+check+'\n}\nfn main(){checked::run()}\n')
    env=dict(os.environ,CARGO_TARGET_DIR=str(root/'target'))
    (project/'Cargo.lock').write_text((root/'Cargo.lock').read_text() + '\n[[package]]\nname = "n64-policy-owner-check"\nversion = "0.0.0"\ndependencies = ["emucap", "serde_json"]\n')
    subprocess.run(['cargo','+1.98.0','update','--offline','-p','n64-policy-owner-check'],cwd=project,env=env,check=True)
    def identities(path):
        return {(p['name'], p['version'], p.get('source'), p.get('checksum')) for p in tomllib.loads(path.read_text())['package']}
    assert identities(project/'Cargo.lock') - {('n64-policy-owner-check','0.0.0',None,None)} <= identities(root/'Cargo.lock'), 'dependency version drift'
    subprocess.run(['cargo','+1.98.0','run','--release','--offline','--locked','--quiet'],cwd=project,env=env,check=True)
