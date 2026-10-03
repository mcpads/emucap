#!/usr/bin/env python3
"""Actual RDRAM batch admission with an address-sensitive counted native getter."""
from pathlib import Path
import json
import os
import subprocess
import tempfile
import tomllib

root = Path(__file__).resolve().parents[2]
source = (root / 'src/n64_adapter_observation.rs').read_text()
capability = source[source.index('    pub(super) fn memory_batch_capability()'):source.index('    pub(super) fn execution_speed_capability()')]
batch = source[source.index('    pub(super) fn read_rdram_bytes('):].split('#[cfg(test)]', 1)[0].rstrip()
assert batch.endswith('}\n}')
batch = batch[:-1]
debug = (root / 'src/n64_adapter_debug.rs').read_text()
def function(text, signature):
    start = text.index(signature)
    end = text.index('{', start) + 1
    depth = 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]
debug_read = function(debug, '    fn read_debug_bytes(')
address_map = function(debug, 'pub(super) fn r4300_rdram_offset(')
setup = r'''
use serde_json::{json,Value};
use std::sync::atomic::{AtomicU64,Ordering};
use emucap::live::memory_batch::{BatchRange,MemoryBatchCapability,MemoryWindow,CONSISTENCY_FROZEN_BOUNDARY};
use emucap::n64_adapter::N64Error;
type N64Result<T> = Result<T,N64Error>;
const RDRAM_SIZE:u64=8*1024*1024;
const BATCH_MAX_RANGES:u64=64;
const BATCH_MAX_BYTES:u64=65536;
static BOUNDARY_SEQ:AtomicU64=AtomicU64::new(3);
static READS:AtomicU64=AtomicU64::new(0);
static CALLS:AtomicU64=AtomicU64::new(0);
static LEGACY:AtomicU64=AtomicU64::new(0);
unsafe extern "C" fn legacy(address:u32)->u8{LEGACY.store(u64::from(address),Ordering::Relaxed);0x9b}
struct Api{debug_mem_read8:unsafe extern "C" fn(u32)->u8,debug_mem_read_rdram:unsafe extern "C" fn(u32,*mut u8,u32)->i32}
fn check_core(_: &str,result:i32)->N64Result<()>{if result==0{Ok(())}else{Err(N64Error::BadState("native read failed".into()))}}
struct Mupen64PlusHost{api:Api,launch_id:Option<String>,display:bool,frozen:bool}
fn required_num(value:&Value,key:&str)->N64Result<u64>{value[key].as_u64().ok_or_else(||N64Error::BadParams(key.into()))}
unsafe extern "C" fn read(offset:u32,output:*mut u8,length:u32)->i32{
 assert!(u64::from(offset)+u64::from(length)<=RDRAM_SIZE);
 CALLS.fetch_add(1,Ordering::Relaxed);
 READS.fetch_add(u64::from(length),Ordering::Relaxed);
 for i in 0..length{*output.add(i as usize)=match u64::from(offset+i){1=>0xa5,2=>0xc3,n if n==RDRAM_SIZE-1=>0xe7,_=>0x42};}
 0
}
impl Mupen64PlusHost{
 fn public_frame(&self)->u64{7}
 fn require_frozen(&self,_:&str)->N64Result<()>{if self.frozen{Ok(())}else{Err(N64Error::BadState("running".into()))}}
'''
checks = r'''
pub fn run(){
 for display in [false,true]{
  let mut host=Mupen64PlusHost{api:Api{debug_mem_read8:legacy,debug_mem_read_rdram:read},launch_id:Some("fixed".into()),display,frozen:true};
  let range=|address:u64,length:u64|json!({"memory_type":"rdram","address":address,"length":length});
  READS.store(0,Ordering::Relaxed); CALLS.store(0,Ordering::Relaxed);
  let value=host.read_memory_batch(&json!({"ranges":[range(1,2),range(2,1),range(1,2),range(RDRAM_SIZE-1,1)]})).unwrap();
  for (i,expected) in ["a5c3","c3","a5c3","e7"].iter().enumerate(){assert_eq!(value["reads"][i]["hex"],*expected);assert_eq!(value["reads"][i]["index"],i);}
  assert_eq!(READS.load(Ordering::Relaxed),6);
  assert_eq!(CALLS.load(Ordering::Relaxed),4);
  for bad in [range(RDRAM_SIZE-1,2),range(u64::MAX,2),range(0,0),json!({"memory_type":"io","address":0,"length":1}),json!({"memory_type":"rdram","address":-1,"length":1})]{
   READS.store(0,Ordering::Relaxed); CALLS.store(0,Ordering::Relaxed);
   assert!(host.read_memory_batch(&json!({"ranges":[range(1,2),bad]})).is_err());
   assert_eq!(READS.load(Ordering::Relaxed),0); assert_eq!(CALLS.load(Ordering::Relaxed),0);
  }
  for ranges in [vec![],vec![range(1,1);65],vec![range(0,65536),range(0,1)]]{
   assert!(host.read_memory_batch(&json!({"ranges":ranges})).is_err());
   assert_eq!(READS.load(Ordering::Relaxed),0); assert_eq!(CALLS.load(Ordering::Relaxed),0);
  }
  for address in [0x80000001,0xa0000001]{
   LEGACY.store(0,Ordering::Relaxed);
   assert_eq!(host.read_debug_bytes(address,2).unwrap(),vec![0xa5,0xc3]);
   assert_eq!(LEGACY.load(Ordering::Relaxed),0);
  }
  for address in [1,0x40000001,0xa4000000,0xb0000000]{
   CALLS.store(0,Ordering::Relaxed);
   assert_eq!(host.read_debug_bytes(address,1).unwrap(),vec![0x9b]);
   assert_eq!(LEGACY.load(Ordering::Relaxed),u64::from(address));
   assert_eq!(CALLS.load(Ordering::Relaxed),0);
  }
  READS.store(0,Ordering::Relaxed); CALLS.store(0,Ordering::Relaxed);
  host.frozen=false;
  assert!(host.read_memory_batch(&json!({"ranges":[range(1,1)]})).is_err());
  assert_eq!(READS.load(Ordering::Relaxed),0); assert_eq!(CALLS.load(Ordering::Relaxed),0);
  assert_eq!(BOUNDARY_SEQ.load(Ordering::Relaxed),3);
 }
 println!("N64 actual batch: RDRAM offset/nonzero/order/overlap/tail, late-invalid zero reads and running rejection passed");
}
'''
with tempfile.TemporaryDirectory(prefix='n64-batch-preflight-') as directory:
    project = Path(directory)
    (project / 'src').mkdir()
    name = 'n64-batch-preflight-check'
    (project / 'Cargo.toml').write_text('[package]\nname=' + json.dumps(name) + '\nversion="0.0.0"\nedition="2021"\n[dependencies]\nemucap={path=' + json.dumps(str(root)) + '}\nserde_json="1"\nhex="0.4"\n')
    (project / 'src/main.rs').write_text('mod checked{\n' + setup + capability + batch + debug_read + '}\n' + address_map + checks + '}\nfn main(){checked::run()}\n')
    (project / 'Cargo.lock').write_text((root / 'Cargo.lock').read_text() + '\n[[package]]\nname = ' + json.dumps(name) + '\nversion = \"0.0.0\"\ndependencies = [\"emucap\", \"serde_json\", \"hex\"]\n')
    env = dict(os.environ, CARGO_TARGET_DIR=str(root / 'target'))
    subprocess.run(['cargo','update','--offline','-p',name],cwd=project,env=env,check=True)
    def identities(path):
        return {(p['name'],p['version'],p.get('source'),p.get('checksum')) for p in tomllib.loads(path.read_text())['package']}
    assert identities(project/'Cargo.lock') - {(name,'0.0.0',None,None)} <= identities(root/'Cargo.lock'), 'dependency version drift'
    subprocess.run(['cargo','run','--release','--offline','--locked','--quiet'],cwd=project,env=env,check=True)
