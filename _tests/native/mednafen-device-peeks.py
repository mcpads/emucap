#!/usr/bin/env python3
"""Native PCE CD and MD EEPROM reads preserve device state only in peek mode."""
import argparse
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2];base=root/'adapters/mednafen/work/mednafen/src'
p=argparse.ArgumentParser();p.add_argument('--baseline',action='store_true')
p.add_argument('--source',type=Path,default=base);a=p.parse_args();base=a.source
def function(file,signature):
 s=(base/file).read_text();i=s.index(signature);return s[i:s.index('\n}',i)+2]+'\n'
code=r'''
#include <cassert>
#include <cstdint>
using uint8=uint8_t;using uint16=uint16_t;using uint32=uint32_t;using int32=int32_t;
#define MDFN_FASTCALL
#define ADPCM_DEBUG(...) ((void)0)
int MD_HackyHackyMode=0;
class MD_Cart_Type_EEPROM {public:
 enum {READ_DATA,GET_WORD_ADR_7BITS,GET_SLAVE_ADR,GET_WORD_ADR_HIGH,GET_WORD_ADR_LOW,WRITE_DATA};
 uint8 sda=1,sram[65536]={};int state=READ_DATA,cycles=8;uint16 slave_mask=0,word_address=7;
 struct {unsigned size_mask=255,sda_out_adr=1,sda_out_bit=0;}type;
 unsigned int ReadEEPROM(unsigned int,bool);
};
bool bBRAMEnabled=true;int runs=0;uint8 _Port[16]={};int RawPCMVolumeCache[2]={};
struct {int ReadPending=0,ReadBuffer=0,WritePending=0,LastCmd=0;bool EndReached=false,Playing=false;}ADPCM;
struct {int CanRead(){return 0;}uint8 ReadByte(bool){assert(false);return 0;}}SubChannelFIFO;
void PCECD_Run(uint32){++runs;}bool SCSICD_GetBSY(){return false;}
bool SCSICD_GetREQ(){return false;}bool SCSICD_GetMSG(){return false;}
bool SCSICD_GetCD(){return false;}bool SCSICD_GetIO(){return false;}
uint8 SCSICD_GetDB(){return 0;}void update_irq_state(){assert(false);}
uint8 read_1808(int,bool){return 0;}int CalcNextEvent(int){return 73;}
'''
code+=function('md/cart/map_eeprom.cpp','unsigned int MD_Cart_Type_EEPROM::ReadEEPROM(')
code+=function('pce/pcecd.cpp','MDFN_FASTCALL uint8 PCECD_Read(')
code+=r'''
#define DECLFR(name) uint8 name(uint32 A)
#define PCE_DEBUG(...) ((void)0)
int PCE_InDebug=0,event_updates=0;
bool IsTsushin=false,PCE_IsCD=true,IsHES=false;
struct {void StealCycle(){} bool InBlockMove(){return false;}int GetIODataBuffer(){return 0;}
 int TimerRead(uint32,int){return 0;}void SetIODataBuffer(int){}int Timestamp(){return 100;}
 int IRQStatusRead(uint32,int){return 0;}}HuCPU;
struct Vce {int ReadVDC(uint32){return 0;}int Read(uint32){return 0;}
 void SetCDEvent(int){++event_updates;}} video;Vce* vce=&video;
struct Arcade {int Read(uint32,int){return 0;}};Arcade* arcade_card=nullptr;
int INPUT_Read(int,uint32){return 0;}int PCE_TsushinRead(uint32){return 0;}int ReadIBP(uint32){return 0;}
'''
code+=function('pce/pce.cpp','static DECLFR(IORead)')
code+=r'''
int main(){
 MD_Cart_Type_EEPROM e;e.sram[7]=1;
 MD_HackyHackyMode=1;assert(e.ReadEEPROM(1,false)==1);assert(e.word_address==PEEK_ADDRESS);
 MD_HackyHackyMode=0;e.word_address=7;assert(e.ReadEEPROM(1,false)==1);assert(e.word_address==8);
 int next=0;_Port[3]=2;assert(PCECD_Read(100,0x1803,next,true)==2);
 assert(bBRAMEnabled==PEEK_BRAM && _Port[3]==2 && runs==0);
 bBRAMEnabled=true;assert(PCECD_Read(100,0x1803,next,false)==2);
 assert(!bBRAMEnabled && _Port[3]==0 && runs==1);
 PCE_InDebug=1;IORead(0x1800);assert(event_updates==PEEK_EVENTS && runs==1);
 PCE_InDebug=0;event_updates=0;IORead(0x1800);assert(event_updates==1 && runs==2);
}
'''.replace('PEEK_ADDRESS','8' if a.baseline else '7').replace('PEEK_BRAM','false' if a.baseline else 'true').replace('PEEK_EVENTS','1' if a.baseline else '0')
with tempfile.TemporaryDirectory(prefix='mednafen-device-peeks-') as d:
 p=Path(d);(p/'check.cpp').write_text(code)
 subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined',str(p/'check.cpp'),'-o',str(p/'check')],check=True)
 subprocess.run([str(p/'check')],check=True)
print('REPRODUCED EEPROM advancement, CD backup-RAM disable and peek event update' if a.baseline else 'PASS peeks preserve EEPROM, CD latches and scheduling; execution reads retain side effects')
