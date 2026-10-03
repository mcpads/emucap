#!/usr/bin/env python3
"""Verify PC-98 debug VRAM reads bypass the stateful graphics charger branch."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'adapters/mame-pc98/work/mame-src/src/mame/nec'


def function(file, signature):
    text = (SOURCE / file).read_text()
    start = text.index(signature)
    return text[start:text.index('\n}', start) + 2]


code = r'''
#include <array>
#include <cassert>
#include <cstdint>
#include <vector>
using offs_t=uint32_t;
template<unsigned N,typename T,typename... B> uint16_t bitswap(T value,B... bits) {
    const std::array<unsigned,N> positions{static_cast<unsigned>(bits)...};
    uint16_t result=0;
    for(unsigned bit:positions) result=(result<<1)|((value>>bit)&1);
    return result;
}
struct Machine {
    bool disabled=true;
    bool side_effects_disabled() const { return disabled; }
};
struct pc9801vm_state {
    Machine host;
    struct { uint8_t mode=0; std::array<uint8_t,4> tile{0x21,0x43,0x65,0x87}; } m_grcg;
    std::array<std::vector<uint16_t>,2> m_video_ram;
    std::array<bool,8> m_ex_video_ff{};
    unsigned m_vram_bank=0,egc_calls=0;
    Machine& machine() { return host; }
    uint16_t egc_blit_r(uint32_t,uint16_t) { ++egc_calls; return 0xbeef; }
    uint16_t upd7220_grcg_r(offs_t,uint16_t);
    uint16_t grcg_gvram_r(offs_t,uint16_t);
    uint16_t grcg_gvram0_r(offs_t,uint16_t);
};
''' + '\n'.join([
    function('pc9801_v.cpp', 'uint16_t pc9801vm_state::upd7220_grcg_r('),
    function('pc9801.cpp', 'uint16_t pc9801vm_state::grcg_gvram_r('),
    function('pc9801.cpp', 'uint16_t pc9801vm_state::grcg_gvram0_r('),
]) + r'''
int main() {
    pc9801vm_state s;
    s.m_video_ram[1].resize(0x20000);
    for(unsigned i=0;i<0x20000;i++) s.m_video_ram[1][i]=(i*37)^0xa5c3;
    const auto before=s.m_video_ram;
    for(unsigned mode=0;mode<256;mode++)
    for(unsigned bank=0;bank<2;bank++)
    for(bool egc:{false,true}) {
        s.m_grcg.mode=mode; s.m_vram_bank=bank; s.m_ex_video_ff[2]=egc;
        const auto tiles=s.m_grcg.tile;
        for(unsigned offset:{0u,1u,0x3fffu,0x7fffu,0xbfffu})
        for(uint16_t mask:{0x00ff,0xff00,0xffff}) {
            auto expected=before[1][(offset+0x4000)|(bank<<16)];
            assert(s.upd7220_grcg_r(offset+0x4000,mask)==expected);
            // Independent bytewise bit reversal oracle for the mapped CPU view.
            uint16_t reversed=0;
            for(unsigned b=0;b<16;b++)
                reversed|=((expected>>b)&1)<<((b&8)+(7-(b&7)));
            assert(s.grcg_gvram_r(offset,mask)==reversed);
            expected=before[1][offset|(bank<<16)];
            reversed=0;
            for(unsigned b=0;b<16;b++)
                reversed|=((expected>>b)&1)<<((b&8)+(7-(b&7)));
            assert(s.grcg_gvram0_r(offset,mask)==reversed);
        }
        assert(s.egc_calls==0 && s.m_grcg.mode==mode && s.m_grcg.tile==tiles);
        assert(s.m_vram_bank==bank && s.m_ex_video_ff[2]==egc);
    }
    assert(s.m_video_ram==before);
    // Positive control: the same actual read dispatcher reaches EGC during execution.
    s.host.disabled=false; s.m_grcg.mode=0x80; s.m_ex_video_ff[2]=true;
    assert(s.upd7220_grcg_r(0,0xffff)==0xbeef && s.egc_calls==1);
}
'''

with tempfile.TemporaryDirectory(prefix='mame-pc98-peek-') as directory:
    temp = Path(directory)
    (temp / 'test.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++17', '-O1', '-g', '-Wall', '-Wextra', '-Werror',
                    '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
                    str(temp / 'test.cpp'), '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('PC-98 actual graphics read dispatcher: all charger modes/banks/masks, zero EGC side effects PASS')
