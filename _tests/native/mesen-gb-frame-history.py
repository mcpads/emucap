#!/usr/bin/env python3
"""Exercise the actual GB submitted-frame owner with single/linked decoder reads."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, default=Path('adapters/mesen2/work/mesen'))
a = p.parse_args()
source = (a.source/'Core/Gameboy/GbPpu.cpp').read_text()
body = source[source.index('void GbPpu::SubmitFrame('):source.index('void GbPpu::SendLinkedFrame(')]
constants = (a.source/'Core/Gameboy/GbConstants.h').read_text()
code = r'''
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <iterator>
#include <vector>
#include <functional>
'''+constants.replace('#pragma once', '').replace('#include "pch.h"', '')+r'''
struct RenderedFrame { void* FrameBuffer; uint32_t Width, Height; void* Data = nullptr; };
struct Decoder {
    std::function<void()> pending;
    unsigned waits = 0;
    void WaitForAsyncFrameDecode() { ++waits; if(pending) { pending(); pending = {}; } }
    void UpdateFrame(RenderedFrame& frame, bool synchronous, bool) {
        assert(!pending); // The previous decoder read must finish before a new submission.
        auto ptr = static_cast<uint16_t*>(frame.Data);
        std::vector<uint16_t> expected(ptr, ptr+frame.Width*frame.Height);
        auto check = [ptr,expected]() { assert(std::equal(expected.begin(),expected.end(),ptr)); };
        if(synchronous) check(); else pending = check;
    }
};
struct Emulator { Decoder decoder; Decoder* GetVideoDecoder() { return &decoder; } };
struct GbPpu;
struct Gameboy {
    bool primary; Gameboy* other = nullptr; GbPpu* ppu = nullptr;
    bool IsPrimaryConsole() { return primary; }
    Gameboy* GetLinkedConsole() { return other; }
    GbPpu* GetPpu() { return ppu; }
};
struct GbPpu {
    Emulator* _emu; Gameboy* _gameboy;
    uint16_t _completedFrame[GbConstants::LinkedPixelCount] = {};
    uint16_t _previousCompletedFrame[GbConstants::LinkedPixelCount] = {};
    uint32_t _completedPixelCount = 0;
    void SubmitFrame(RenderedFrame&, bool, bool);
};
'''+body+r'''
int main() {
    Emulator emu; Gameboy main{true}, sub{false};
    auto first = new GbPpu{&emu,&main}; auto second = new GbPpu{&emu,&sub};
    main.ppu=first; sub.ppu=second; main.other=&sub; sub.other=&main;
    std::vector<uint16_t> pixels(GbConstants::LinkedPixelCount, 0x1234);
    RenderedFrame frame{pixels.data(),GbConstants::ScreenWidth,GbConstants::ScreenHeight};
    first->SubmitFrame(frame,false,false);
    assert(first->_previousCompletedFrame[0]==0);
    std::fill(pixels.begin(),pixels.end(),0x4321);
    second->SubmitFrame(frame,false,false);
    assert(first->_completedFrame[0]==0x4321 && first->_previousCompletedFrame[0]==0x1234);
    assert(second->_completedPixelCount==0); // Sub-only output still belongs to primary.
    frame.Width*=2;
    first->SubmitFrame(frame,true,false);
    assert(first->_completedPixelCount==GbConstants::LinkedPixelCount);
    assert(std::all_of(std::begin(first->_previousCompletedFrame),std::end(first->_previousCompletedFrame),[](auto p){return p==0;}));
    std::fill(pixels.begin(),pixels.end(),0x7fff);
    first->SubmitFrame(frame,false,false);
    assert(first->_previousCompletedFrame[GbConstants::LinkedPixelCount-1]==0x4321);
    frame.Width/=2;
    second->SubmitFrame(frame,false,false);
    assert(first->_previousCompletedFrame[0]==0 && first->_completedPixelCount==GbConstants::PixelCount);
    emu.decoder.WaitForAsyncFrameDecode();
    assert(emu.decoder.waits==6);
    delete second; delete first;
}
'''
with tempfile.TemporaryDirectory(prefix='mesen-gb-history-') as temp:
    root=Path(temp); cpp=root/'test.cpp'; exe=root/'test'; cpp.write_text(code)
    subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined','-fno-omit-frame-pointer',str(cpp),'-o',str(exe)],check=True)
    subprocess.run([str(exe)],check=True)
print('GB submitted history: owner, merged dimensions, transitions and pending decoder reads pass')
