#!/usr/bin/env python3
"""Actual GBA debug input route must preserve native input/lag bookkeeping."""
from pathlib import Path
import os
import subprocess
import tempfile
root=Path(__file__).resolve().parents[2]
core=root/'adapters/mesen2/work/mesen/Core'
def function(path,signature):
    source=(core/path).read_text();start=source.index(signature);end=source.index('{',start)+1;depth=1
    while depth:
        depth+=(source[end]=='{')-(source[end]=='}');end+=1
    return source[start:end]
setup=r'''
#include <cstdint>
#include <cassert>
#include <cstdio>
#include <string>
namespace BitUtilities {template<int start,class T> uint8_t GetBits(T n){return n>>start;}}
namespace HexUtilities {std::string ToHex32(uint32_t){return "";}}
struct BaseControlManager{bool _wasInputRead=false;unsigned _lagCounter=0;void SetInputReadFlag();void ProcessEndOfFrame();};
struct GbaControlManager:BaseControlManager{struct{uint16_t ActiveKeys=0x02a5,KeyControl=0x5678;} _state;uint8_t ReadInputPort(uint32_t);uint8_t PeekInputPort(uint32_t)const;};
struct Device{uint8_t ReadRegister(uint32_t,bool=false){return 0;}uint8_t ReadRam(uint32_t,uint32_t){return 0;}uint8_t Read(uint32_t){return 0;}};
struct Config{bool EnableMgbaLogApi=false;};struct Settings{Config GetGbaConfig(){return {};}};struct Emu{Settings settings;Settings*GetSettings(){return &settings;}};
struct GbaConsole{static constexpr unsigned BootRomSize=0x4000,ExtWorkRamSize=0x40000,IntWorkRamSize=0x8000,PaletteRamSize=0x400,SpriteRamSize=0x400;};
struct GbaMemoryManager{
 Device device,*_ppu=&device,*_apu=&device,*_dmaController=&device,*_timer=&device,*_serial=&device,*_cart=&device,*_mgbaLog=&device;
 GbaControlManager*_controlManager;Emu emu,*_emu=&emu;
 uint8_t data[0x40000]={};uint8_t *_bootRom=data,*_extWorkRam=data,*_intWorkRam=data,*_palette=data,*_vram=data,*_oam=data,*_prgRom=data;
 uint32_t _prgRomSize=sizeof(data);
 struct{uint16_t IE=0,IF=0,WaitControl=0;uint8_t IME=0,PostBootFlag=0,InternalOpenBus[4]={};} _state;
 void LogDebug(const std::string&){}
 uint32_t ReadRegister(uint32_t);uint8_t DebugRead(uint32_t);
};
'''
code=setup+'\n'.join(function(path,sig) for path,sig in [
('Shared/BaseControlManager.cpp','void BaseControlManager::SetInputReadFlag('),
('Shared/BaseControlManager.cpp','void BaseControlManager::ProcessEndOfFrame('),
('GBA/GbaControlManager.cpp','uint8_t GbaControlManager::ReadInputPort('),
('GBA/GbaControlManager.cpp','uint8_t GbaControlManager::PeekInputPort('),
('GBA/GbaMemoryManager.cpp','uint32_t GbaMemoryManager::ReadRegister('),
('GBA/GbaMemoryManager.cpp','uint8_t GbaMemoryManager::DebugRead(')])
checks=r'''
int main(){
 GbaControlManager control;GbaMemoryManager memory;memory._controlManager=&control;
 for(bool already_read:{false,true}){
  control._wasInputRead=already_read;control._lagCounter=7;
  const uint8_t expected[]={0xa5,0x02,0x56,0x78};
  for(unsigned i=0;i<4;i++){
   assert(memory.DebugRead(0x04000130+i)==expected[i]);
   assert(control._wasInputRead==already_read && control._lagCounter==7);
  }
  control.ProcessEndOfFrame();assert(control._lagCounter==(already_read?7:8));
 }
 for(unsigned i=0;i<4;i++){
  control._wasInputRead=false;
  assert(memory.ReadRegister(0x130+i)==memory.DebugRead(0x04000130+i));
  assert(control._wasInputRead==(i<2));
 }
 puts("Mesen actual GBA DebugRead/ReadRegister/input route: four register bytes, input-read preservation and next-frame lag oracle passed");
}
'''
with tempfile.TemporaryDirectory(prefix='mesen-gba-input-peek-') as directory:
    path=Path(directory);source=path/'check.cpp';source.write_text(code+checks);binary=path/'check'
    subprocess.run(['clang++','-std=c++17','-fsanitize='+os.environ.get('EMUCAP_SANITIZERS','address,undefined'),str(source),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)
    mutant=code.replace('return _controlManager->PeekInputPort(addr);','return _controlManager->ReadInputPort(addr);')
    assert mutant!=code
    source.write_text(mutant+checks)
    subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(source),'-o',str(binary)],check=True)
    failed=subprocess.run([str(binary)],capture_output=True,text=True)
    assert failed.returncode!=0 and 'control._wasInputRead==already_read' in failed.stderr, failed
    print('Prior-route mutant rejected by unchanged-input-read assertion')
