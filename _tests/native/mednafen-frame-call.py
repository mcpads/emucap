#!/usr/bin/env python3
"""Check active/between-frame native wrapper restoration ownership."""
import ast
import os
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[2]
source = root / 'adapters/mednafen/work/mednafen'
adapter = root / 'adapters/mednafen'
fixture = ast.parse(Path(__file__).with_name('mednafen-frame-output.py').read_text())
code = next(ast.literal_eval(n.value) for n in fixture.body
            if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'code' for t in n.targets))
code = code[:code.index('int main()')]
native = (source / 'src/mednafen.cpp').read_text()
start = native.index('void MDFNI_Emulate(EmulateSpecStruct *espec)')
entry = native[start:native.index(' MDFNGameInfo->Emulate(espec);', start)]
entry = entry.replace('MDFNGameInfo->soundchan', '2').replace('void MDFNI_Emulate', 'void test_native_entry')
driver = (source / 'src/drivers/main.cpp').read_text()
binding_start = driver.index('EmulateSpecStruct emucap_frame_binding(bool skip)')
binding = driver[binding_start:driver.index('static int GameLoop(void *arg)\n{', binding_start)]
format_start = native.index('static void PrepareFrameFormat(')
format_binding = native[format_start:native.index('static void PrepareFrameOutput(', format_start)]
code += r'''
#include "emucap_frame_call.h"
static EmucapFrameCall EmucapOutputCall;
static unsigned wrapper_prepares = 0;
static MDFN_PixelFormat last_pixel_format;
static double last_sound_rate=-1;
static uint8 palette[]={1,2,3};
static uint8* CustomPalette=palette;
static uint32 CustomPaletteNumEntries=1;
struct ResamplerBinding { size_t size=0;void buffer_size(size_t value){size=value;} };
static ResamplerBinding ff_resampler;
''' + format_binding + r'''
void PrepareFrameOutput(EmulateSpecStruct* spec) {
 ++wrapper_prepares;
 spec->DisplayRect={0,0,0,0};
 spec->SoundBufSize=0;
}
''' + entry + r'''
 // Emulated guest/device seam: a capture here must refer to this actual call.
 assert(EmucapOutputCall.capture());
}
template<class T> struct Borrowed { T* ptr; T* get() const { return ptr; } };
struct FrameBinding { Borrowed<MDFN_Surface> surface; Borrowed<int32> lw; };
static FrameBinding SoftFB[2];
static bool SoftFB_BackBuffer=false, DNeedRewind=false;
static double live_rate=48000,live_ratio=1;
static unsigned live_volume=100;
static int16* live_sound=nullptr;
static int32 live_capacity=0;
static double Sound_GetRate() { return live_rate; }
static double emucap_host_audio_ratio() { return live_ratio; }
static int16* Sound_GetEmuModBuffer(int32* capacity) { *capacity=live_capacity;return live_sound; }
uint64 Mednafen::MDFN_GetSettingUI(const char* name) { assert(std::string(name)=="sound.volume");return live_volume; }
''' + binding + r'''
int main() {
 // Cold process caches must bind without erasing saved partial output.
 Output cold(2,8,7);
 cold.spec.VideoFormatChanged=cold.spec.SoundFormatChanged=false;
 const auto prior=cold.spec;
 PrepareFrameFormat(&cold.spec);
 assert(cold.spec.VideoFormatChanged && cold.spec.SoundFormatChanged);
 assert(cold.spec.CustomPalette==palette && cold.spec.CustomPaletteNumEntries==1);
 assert(cold.spec.DisplayRect.w==prior.DisplayRect.w && cold.spec.SoundBufSize==prior.SoundBufSize);
 assert(cold.spec.MasterCycles==prior.MasterCycles && cold.spec.SoundVolume==prior.SoundVolume);
 assert(cold.spec.SoundBuf==prior.SoundBuf && cold.spec.soundmultiplier==prior.soundmultiplier);
 assert(ff_resampler.size==48000);
 cold.spec.VideoFormatChanged=cold.spec.SoundFormatChanged=false;
 PrepareFrameFormat(&cold.spec);
 assert(!cold.spec.VideoFormatChanged && !cold.spec.SoundFormatChanged);
 // Preparation borrows the destination driver buffers, with fresh output cursors.
 // Both raster roles, muted audio and host policy changes remain authoritative.
 Output buffers[2]={{2,0,4},{2,0,5}};
 for(unsigned index=0;index<2;++index)
  SoftFB[index]={{buffers[index].spec.surface},{buffers[index].spec.LineWidths}};
 for(unsigned index=0;index<2;++index) for(bool audible : {false,true}) {
  SoftFB_BackBuffer=index;DNeedRewind=bool(index);
  live_rate=audible ? 48000 : 0;live_sound=audible ? buffers[index].sound : nullptr;
  live_capacity=audible ? 16 : 0;live_ratio=3.5;live_volume=37;
  const auto bound=emucap_frame_binding(bool(index));
  assert(bound.surface==buffers[index].spec.surface && bound.LineWidths==buffers[index].spec.LineWidths);
  assert(bound.SoundBuf==live_sound && bound.SoundBufMaxSize==live_capacity && bound.SoundRate==live_rate);
  assert(bound.soundmultiplier==3.5 && bound.SoundVolume==.37 && bound.skip==int(index) && bound.NeedRewind==bool(index));
  assert(bound.SoundBufSize==0 && bound.SoundBufSize_InternalProcessed==0 && bound.SoundBufSize_DriverProcessed==0);
  assert(bound.MasterCycles==0 && bound.MasterCycles_InternalProcessed==0 && bound.MasterCycles_DriverProcessed==0);
  assert(bound.DisplayRect.w==0 && bound.DisplayRect.h==0 && !bound.InterlaceOn);
  assert(buffers[index].widths[0]==-1 && buffers[index].sound[0]==int(index+4));
  auto saved=EmucapFrameCall::Output(new EmucapFrameOutput(EmucapFrameOutput::capture(buffers[index].spec,2)));
  // Muted destinations reject audible history before any live output changes.
  EmucapFrameCall parked;
  try { auto staged=parked.prepare(std::move(saved),bound,2);assert(audible); }
  catch(const std::runtime_error&) { assert(!audible); }
  assert(!parked.pending() && !parked.capture());
 }
 Output wrapper_source(2,8,7),wrapper_fresh(2,0,4);
 auto wrapper_saved=EmucapFrameCall::Output(new EmucapFrameOutput(EmucapFrameOutput::capture(wrapper_source.spec,2)));
 auto wrapper_replacement=EmucapOutputCall.prepare(std::move(wrapper_saved),wrapper_fresh.spec,2);
 assert(EmucapOutputCall.commit(wrapper_replacement,2));
 test_native_entry(&wrapper_fresh.spec);
 assert(wrapper_prepares==0 && equal_output(wrapper_fresh.spec,wrapper_source.spec,2));
 assert(!EmucapOutputCall.capture());
 // An ordinary fresh wrapper still initializes once, even after a restore.
 wrapper_fresh.spec.SoundBufSize_InternalProcessed=wrapper_fresh.spec.SoundBufSize_DriverProcessed=0;
 test_native_entry(&wrapper_fresh.spec);assert(wrapper_prepares==1);

 EmucapFrameCall runtime;
 Output source(2,8,7),destination(2,12,18),second(2,3,21);
 unsigned initialized=0;
 auto initialize=[&](EmulateSpecStruct& s){++initialized;s.SoundBufSize=0;s.SoundBufSize_InternalProcessed=s.SoundBufSize_DriverProcessed=0;};
 auto saved=[&](Output& output){return EmucapFrameCall::Output(new EmucapFrameOutput(EmucapFrameOutput::capture(output.spec,2)));};
 assert(!runtime.capture() && !runtime.pending());
 {
  EmucapFrameCall::Scope call(runtime,destination.spec,2);
  assert(!call.restored());
  assert(runtime.capture());
  auto next=runtime.prepare(saved(source),destination.spec,2);
  assert(!runtime.commit(next,1));
  fail_alloc=true;
  assert(runtime.commit(next,2));
  assert(!runtime.commit(next,2));
  assert(equal_output(destination.spec,source.spec,2));
  runtime.resume(initialize);assert(initialized==0);
  fail_alloc=false;
  // Multiple loads before resume choose the final source phase.
  auto completed=runtime.prepare({},destination.spec,2);
  assert(runtime.commit(completed,2));
  assert(!runtime.capture());assert(initialized==0);
  auto partial=runtime.prepare(saved(second),destination.spec,2);
  assert(runtime.commit(partial,2));runtime.resume(initialize);
  assert(initialized==0 && equal_output(destination.spec,second.spec,2));
  auto fresh=runtime.prepare({},destination.spec,2);
  assert(runtime.commit(fresh,2));assert(initialized==0);
  runtime.resume(initialize);runtime.resume(initialize);assert(initialized==1);
 }
 assert(!runtime.capture() && !runtime.restored_active());
 auto pending=runtime.prepare(saved(source),destination.spec,2);
 assert(runtime.commit(pending,2));assert(runtime.pending() && runtime.capture());
 Output fresh_binding(2,0,4);
 fresh_binding.spec.SoundVolume=.6;fresh_binding.spec.soundmultiplier=3;
 {
  EmucapFrameCall::Scope call(runtime,fresh_binding.spec,2);
  assert(call.restored() && !runtime.pending() && runtime.restored_active());
  assert(equal_output(fresh_binding.spec,source.spec,2));
  assert(fresh_binding.spec.SoundVolume==.6 && fresh_binding.spec.soundmultiplier==3);
  try {EmucapFrameCall::Scope nested(runtime,fresh_binding.spec,2);assert(false);}
  catch(const std::runtime_error&){}
  assert(runtime.capture());
 }
 assert(!runtime.capture());
 auto stale=runtime.prepare(saved(source),destination.spec,2);
 auto moved=std::move(stale);assert(!runtime.commit(stale,2));
 assert(runtime.commit(moved,2));
 auto incompatible=fresh_binding.spec;incompatible.SoundRate=44100;
 try {EmucapFrameCall::Scope bad(runtime,incompatible,2);assert(false);}
 catch(const std::runtime_error&){}
 assert(runtime.pending()); // Rejected entry did not consume saved output or bind a call.
 {
  EmucapFrameCall::Scope recovered(runtime,fresh_binding.spec,2);
  assert(recovered.restored());
 }
 assert(!runtime.pending());
 auto abandoned=runtime.prepare(saved(source),destination.spec,2);
 assert(runtime.commit(abandoned,2) && runtime.pending());
 runtime.invalidate();assert(!runtime.pending() && !runtime.capture());
 try {
  EmucapFrameCall::Scope unwound(runtime,fresh_binding.spec,2);
  throw std::runtime_error("injected native call failure");
 } catch(const std::runtime_error&) {}
 assert(!runtime.capture());
 {
  EmucapFrameCall::Scope clean(runtime,fresh_binding.spec,2);
  assert(!clean.restored());
 }
}

'''
with tempfile.TemporaryDirectory(prefix='mednafen-frame-call-') as temp:
    cpp = Path(temp) / 'test.cpp'
    binary = Path(temp) / 'test'
    cpp.write_text(code)
    units = ['video/surface.cpp', 'video/convert.cpp', 'error.cpp']
    platform_libs = ['-liconv', '-framework', 'CoreFoundation'] if sys.platform == 'darwin' else []
    subprocess.run(['clang++', '-std=c++11', '-DHAVE_CONFIG_H', '-O1',
        '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
        '-I' + str(source / 'include'), '-I' + str(source / 'intl'), '-I' + str(adapter),
        str(cpp), *[str(source / 'src' / unit) for unit in units],
        str(source / 'src/libtrio.a'), str(source / 'intl/libintl.a'), *platform_libs,
        '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True,
        env=dict(os.environ, UBSAN_OPTIONS='halt_on_error=1:print_stacktrace=1'))
print('Native frame call: cold/warm format and driver buffer/policy binding, muted admission, active/idle replacement, deferred fresh initialization, repeated replacement, one-shot commit, binding rejection and scope lifetime pass ASan/UBSan')
