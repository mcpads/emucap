#!/usr/bin/env python3
"""Exercise exact geometry serialization with divergent lists and malformed input."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
text = (root/'adapters/desmume-nds/work/src/desmume/src/gfx3d.cpp').read_text()
helper = text[text.index('static void GFX3D_SaveExactState'):text.index('void gfx3d_savestate(EMUFILE &os)')]
poly = text[text.index('void GFX3D_SaveStatePOLY'):text.index('void GFX3D_LoadStatePOLY')]
code = r'''
#include <cassert>
#include <cstdint>
#include <vector>
#include <cstring>
#include <limits>
using u8=uint8_t; using u16=uint16_t; using u32=uint32_t;
using s16=int16_t; using s32=int32_t;
struct EMUFILE {
 std::vector<u8> bytes; size_t pos=0;
 template<class T> int put(T v) { for(size_t i=0;i<sizeof(T);i++)bytes.push_back(u8(uint64_t(v)>>(i*8))); return 1; }
 template<class T> int get(T &v) { if(bytes.size()-pos<sizeof(T))return 0; uint64_t n=0;for(size_t i=0;i<sizeof(T);i++)n|=uint64_t(bytes[pos++])<<(i*8);v=T(n);return 1; }
 template<class T> int read_32LE(T& v){static_assert(sizeof(T)==4);return get(v);}
 template<class T> int read_16LE(T& v){static_assert(sizeof(T)==2);return get(v);}
 int read_u8(u8&v){return get(v);}
 template<class T> int write_32LE(T v){return put(u32(v));}
 template<class T> int write_16LE(T v){return put(u16(v));}
 int write_u8(u8 v){return put(v);}
};
struct Reg32{u32 value;};struct Reg16{u16 value;};
struct GFX3D_State{
 Reg32 DISP3DCNT; Reg16 clearImageOffset;
 u32 clearColor,clearDepth,fogColor;u16 fogOffset;u8 fogShift,alphaTestRef;
 Reg32 SWAP_BUFFERS;u8 fogDensityTable[32];u16 toonTable16[32],edgeMarkColorTable[16];
};
enum PolygonType {POLYGON_TYPE_TRIANGLE=3,POLYGON_TYPE_QUAD=4};
enum PolygonPrimitiveType{TRIANGLE=0,LINE=7};
struct NDSVertex{struct{s32 x,y,z,w;}position;struct{s32 s,t;}texCoord;struct{u8 r,g,b,a;}color;};
struct POLY{PolygonType type;PolygonPrimitiveType vtxFormat;u16 vertIndexes[4];Reg32 attribute,texParam;u32 texPalette;struct{s16 x,y;u16 width,height;}viewport;};
constexpr size_t VERTLIST_SIZE=16,POLYLIST_SIZE=8;
struct GFX3D_GeometryList{NDSVertex rawVtxList[VERTLIST_SIZE];POLY rawPolyList[POLYLIST_SIZE];size_t rawVertCount,rawPolyCount,clippedPolyCount,clippedPolyOpaqueCount;};
''' + poly + helper + r'''
int main(){
 GFX3D_State state{};state.DISP3DCNT.value=0x87654321;state.clearImageOffset.value=0x9876;
 state.clearColor=0x12345678;state.clearDepth=0x33445566;state.fogColor=0xaabbccdd;state.fogOffset=4095;state.fogShift=11;state.alphaTestRef=31;state.SWAP_BUFFERS.value=3;
 for(size_t i=0;i<32;i++){state.fogDensityTable[i]=u8(i+1);state.toonTable16[i]=u16(i*100);}
 for(size_t i=0;i<16;i++)state.edgeMarkColorTable[i]=u16(i*987);
 EMUFILE encoded;GFX3D_SaveExactState(state,encoded);GFX3D_State restored{};
 assert(GFX3D_LoadExactState(restored,encoded));EMUFILE again;GFX3D_SaveExactState(restored,again);assert(encoded.bytes==again.bytes);
 for(size_t n=0;n<encoded.bytes.size();n++){EMUFILE bad;bad.bytes.assign(encoded.bytes.begin(),encoded.bytes.begin()+n);assert(!GFX3D_LoadExactState(restored,bad));}
 GFX3D_GeometryList pending{},applied{},decodedPending{},decodedApplied{};
 applied.rawVertCount=4;applied.rawPolyCount=1;
 for(size_t i=0;i<4;i++){
  auto&v=applied.rawVtxList[i];v.position={-2147483647-1,-4097,s32(i),2147483647};v.texCoord={-17,65537};v.color={1,2,3,4};
 }
 auto&p=applied.rawPolyList[0];p.type=POLYGON_TYPE_QUAD;p.vtxFormat=LINE;
 for(size_t i=0;i<4;i++)p.vertIndexes[i]=u16(i);
 p.attribute.value=0xdeadbeef;p.texParam.value=0x12345678;p.texPalette=0x10203040;p.viewport={-255,191,256,192};
 EMUFILE lists;GFX3D_SaveExactList(pending,lists);GFX3D_SaveExactList(applied,lists);
 assert(GFX3D_LoadExactList(decodedPending,lists));assert(GFX3D_LoadExactList(decodedApplied,lists));
 assert(decodedPending.rawVertCount==0&&decodedApplied.rawVertCount==4);
 EMUFILE roundtrip;GFX3D_SaveExactList(decodedPending,roundtrip);GFX3D_SaveExactList(decodedApplied,roundtrip);assert(roundtrip.bytes==lists.bytes);
 EMUFILE single;GFX3D_SaveExactList(applied,single);
 for(size_t n=0;n<single.bytes.size();n++){EMUFILE bad;bad.bytes.assign(single.bytes.begin(),single.bytes.begin()+n);assert(!GFX3D_LoadExactList(decodedApplied,bad));}
 EMUFILE tooMany;tooMany.write_32LE(u32(VERTLIST_SIZE+1));assert(!GFX3D_LoadExactList(decodedApplied,tooMany));
 EMUFILE badPolyCount;badPolyCount.write_32LE(0);badPolyCount.write_32LE(u32(POLYLIST_SIZE+1));assert(!GFX3D_LoadExactList(decodedApplied,badPolyCount));
 for(u32 type:{0u,2u,5u}){auto bad=single;bad.pos=0;bad.bytes[4+4*28+4]=u8(type);assert(!GFX3D_LoadExactList(decodedApplied,bad));}
 auto badRef=single;badRef.pos=0;badRef.bytes[4+4*28+4+4]=4;assert(!GFX3D_LoadExactList(decodedApplied,badRef));
}
'''
with tempfile.TemporaryDirectory() as directory:
    source=Path(directory)/'probe.cpp';binary=Path(directory)/'probe';source.write_text(code)
    subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(source),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)
print('Exact render-state roundtrip, distinct geometry lists, bounds and truncation passed')
