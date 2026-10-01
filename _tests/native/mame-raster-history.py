#!/usr/bin/env python3
"""Check actual screen-device save/restore methods with distinct buffers and bad geometry."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, default=Path('adapters/mame-neogeo/work/mame-src'))
a = p.parse_args()
source = (a.source/'src/emu/screen.cpp').read_text()
start = source.index('bool screen_device::raster_state_geometry_valid() const')
end = source.index('//  timer events', start)
methods = source[start:end].rstrip().removesuffix('//-------------------------------------------------').rstrip()
code = r'''
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <stdexcept>
#include <vector>
using s32=int32_t; using u32=uint32_t; using u8=uint8_t;
enum { BITMAP_FORMAT_RGB32=1, BLENDMODE_NONE=0 };
#define PRIMFLAG_BLENDMODE(x) (x)
#define PRIMFLAG_SCREENTEX(x) (x)
struct emu_fatalerror : std::runtime_error {
    emu_fatalerror(const char* message,const char*) : std::runtime_error(message) {}
};
struct rectangle { int min_x=0,max_x=2,min_y=0,max_y=1; };
struct rgb_t { rgb_t(){} rgb_t(int,int,int,int){} rgb_t operator-(rgb_t) const { return {}; } };
template<class T> struct bitmap {
    int w=3,h=2; std::vector<T> data=std::vector<T>(6);
    int width()const{return w;} int height()const{return h;}
    void resize(int width,int height){ w=width;h=height;data.assign(w*h,T{}); }
    T& pix(int y){return data.at(y*w);}
};
struct screen_bitmap : bitmap<u32> {
    int kind=BITMAP_FORMAT_RGB32;
    int format()const{return kind;} int texformat()const{return 1;}
    bitmap<u32>& as_rgb32(){assert(kind==BITMAP_FORMAT_RGB32);return *this;}
};
struct texture { screen_bitmap* source=nullptr; int bindings=0;
    void set_bitmap(screen_bitmap& b,rectangle,int){source=&b;++bindings;}
};
struct container { texture* presented=nullptr; int clears=0;
    void empty(){++clears;presented=nullptr;}
    void add_quad(float,float,float,float,rgb_t,texture* t,int){presented=t;}
};
struct screen_device {
    int m_state_max_width=4,m_state_max_height=3;
    int m_state_bitmap_width=0,m_state_bitmap_height=0;
    int m_width=3,m_height=2;
    rectangle m_visarea;
    u8 m_curbitmap=1,m_curtexture=0,m_brightness=255;
    bool m_changed=true;
    int m_partial_scan_hpos=2,reallocations=0;
    rgb_t m_color;
    std::vector<u32> m_state_bitmap[2]={std::vector<u32>(12),std::vector<u32>(12)};
    std::vector<u8> m_state_priority=std::vector<u8>(12);
    screen_bitmap m_bitmap[2]; bitmap<u8> m_priority;
    texture textures[2]; texture* m_texture[2]={&textures[0],&textures[1]};
    container display; container* m_container=&display;
    const char* tag()const{return "screen";}
    void realloc_screen_bitmaps(){
        ++reallocations;
        for(auto& b:m_bitmap)b.resize(std::max(m_width,m_visarea.max_x+1),std::max(m_height,m_visarea.max_y+1));
        m_priority.resize(m_bitmap[0].width(),m_bitmap[0].height());
    }
    bool raster_state_geometry_valid()const;
    void device_pre_save(); void device_post_load(); void publish_restored_raster();
};
'''+methods+r'''
int main(){
    for(unsigned displayed=0;displayed!=2;++displayed){
        screen_device s;s.m_curtexture=displayed;s.m_curbitmap=1-displayed;
        s.m_bitmap[0].data={1,2,3,4,5,6};s.m_bitmap[1].data={11,12,13,14,15,16};
        s.m_priority.data={1,0,2,0,3,0};
        auto const* address=s.m_state_bitmap[0].data();
        s.device_pre_save();
        assert((s.m_bitmap[0].data==std::vector<u32>{1,2,3,4,5,6}));
        assert(s.m_curtexture==displayed && s.m_changed);
        // Live allocations and contents may differ at the time this snapshot is restored.
        for(auto& b:s.m_bitmap){b.resize(4,3);std::fill(b.data.begin(),b.data.end(),99);}
        s.device_post_load();
        assert(s.m_state_bitmap[0].data()==address);
        assert((s.m_bitmap[0].data==std::vector<u32>{1,2,3,4,5,6}));
        assert((s.m_bitmap[1].data==std::vector<u32>{11,12,13,14,15,16}));
        assert((s.m_priority.data==std::vector<u8>{1,0,2,0,3,0}));
        assert(s.m_curtexture==displayed && s.m_curbitmap==1-displayed);
        assert(s.m_changed && s.m_partial_scan_hpos==2);
        assert(s.display.presented==s.m_texture[displayed]);
        assert(s.display.presented->source==&s.m_bitmap[displayed]);
        assert(s.textures[0].bindings==1 && s.textures[1].bindings==1);
    }
    for(int defect=0;defect!=8;++defect){
        screen_device s;s.device_pre_save();
        switch(defect){
        case 0:s.m_state_bitmap_width=0;break;
        case 1:s.m_state_bitmap_height=4;break;
        case 2:s.m_visarea.min_x=-1;break;
        case 3:s.m_visarea.max_y=9;break;
        case 4:s.m_curbitmap=2;break;
        case 5:s.m_curtexture=255;break;
        case 6:s.m_width=4;break;
        case 7:s.m_height=-1;break;
        }
        bool failed=false;try{s.device_post_load();}catch(emu_fatalerror&){failed=true;}
        assert(failed && s.reallocations==0 && s.display.clears==0);
    }
    screen_device inconsistent;inconsistent.m_bitmap[1].resize(2,2);
    bool failed=false;try{inconsistent.device_pre_save();}catch(emu_fatalerror&){failed=true;}
    assert(failed && inconsistent.m_bitmap[0].width()==3);
    screen_device unsupported;for(auto& b:unsupported.m_state_bitmap)b.clear();
    unsupported.device_pre_save();unsupported.device_post_load();
    assert(unsupported.reallocations==1 && unsupported.display.clears==0);
}
'''
with tempfile.TemporaryDirectory(prefix='mame-raster-history-') as temp:
    cpp=Path(temp)/'test.cpp';exe=Path(temp)/'test';cpp.write_text(code)
    subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined','-fno-omit-frame-pointer',str(cpp),'-o',str(exe)],check=True)
    subprocess.run([str(exe)],check=True)
print('Native raster preservation, both display indices, stable storage and geometry rejection pass')
