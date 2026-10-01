// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#include "AudioCommon/Mixer.h"
#include <atomic>
#include <cassert>
#include <cstdlib>
#include <iostream>
#include <memory>
#include <thread>
#include "AudioCommon/Enums.h"
#include "Common/Config/Config.h"
#include "Common/Logging/Log.h"
#include "Core/Config/MainSettings.h"
#include "Core/Core.h"
#include "Core/System.h"

// Native Config storage, callback dispatch and Mixer reads/writes are under test.
// A frozen core view permits silent underflow. Unused surround/file output aborts if called;
// these fixtures make no claim about device playback or guest execution.
class DPL2FSDecoder
{
};
namespace AudioCommon
{
SurroundDecoder::SurroundDecoder(u32 rate, u32 block)
    : m_sample_rate(rate), m_frame_block_size(block)
{
}
SurroundDecoder::~SurroundDecoder() = default;
size_t SurroundDecoder::QueryFramesNeededForSurroundOutput(size_t) const
{
  std::abort();
}
void SurroundDecoder::PutFrames(const short*, size_t)
{
  std::abort();
}
void SurroundDecoder::ReceiveFrames(float*, size_t)
{
  std::abort();
}
void SurroundDecoder::Clear()
{
  std::abort();
}
}  // namespace AudioCommon
namespace File
{
IOFile::IOFile() : m_file(nullptr), m_good(true)
{
}
IOFile::~IOFile() = default;
}  // namespace File
WaveFileWriter::WaveFileWriter() : m_current_sample_rate_divisor(1)
{
}
WaveFileWriter::~WaveFileWriter() = default;
bool WaveFileWriter::Start(const std::string&, u32)
{
  std::abort();
}
void WaveFileWriter::Stop()
{
  std::abort();
}
void WaveFileWriter::AddStereoSamplesBE(const short*, u32, u32, int, int)
{
  std::abort();
}
namespace Core
{
struct System::Impl
{
};
System::System() = default;
System::~System() = default;
State GetState(System&)
{
  return State::Paused;
}

}  // namespace Core
namespace Common::Log
{
void GenericLogFmtImpl(LogLevel, LogType, const char*, int, fmt::string_view,
                       const fmt::format_args&)
{
}
}  // namespace Common::Log
namespace Config
{
const Info<float> MAIN_EMULATION_SPEED{{System::Main, "Core", "EmulationSpeed"}, 1.0f};
const Info<AudioCommon::DPL2Quality> MAIN_DPL2_QUALITY{{System::Main, "DSP", "DPL2Quality"},
                                                       AudioCommon::DPL2Quality::Low};
const Info<bool> MAIN_AUDIO_PRESERVE_PITCH{{System::Main, "DSP", "PreservePitch"}, false};
const Info<bool> MAIN_AUDIO_FILL_GAPS{{System::Main, "DSP", "FillGaps"}, false};
const Info<int> MAIN_AUDIO_BUFFER_SIZE{{System::Main, "DSP", "BufferSize"}, 80};
const Info<bool> MAIN_WIIMOTE_AUDIO_ROUTING_ENABLED{{System::Main, "DSP", "Routing"}, false};
const std::array<Info<bool>, WIIMOTE_SPEAKER_COUNT> MAIN_WIIMOTE_AUDIO_OUTPUT_ENABLED{
    {{{System::Main, "DSP", "Wii0"}, false},
     {{System::Main, "DSP", "Wii1"}, false},
     {{System::Main, "DSP", "Wii2"}, false},
     {{System::Main, "DSP", "Wii3"}, false}}};
}  // namespace Config
int main(int argc, char**)
{
  Config::Init();
  auto mixer = std::make_unique<Mixer>(48000);
  std::atomic<bool> start = false;
  std::thread writer([&] {
    while (!start.load())
    {
    };
    for (int i = 0; i < 10000; ++i)
    {
      if (argc > 1)
      {
        mixer->SetDMAInputSampleRateDivisor(i % 2 ? 2250 : 3375);
        mixer->SetGBAInputSampleRate(0, i % 2 ? 32768 : 65536);
      }
      else
      {
        Config::SettingsTransaction transaction;
        Config::SetCurrent(Config::MAIN_EMULATION_SPEED, i % 2 ? 0.5f : 4.0f);
        Config::SetCurrent(Config::MAIN_AUDIO_PRESERVE_PITCH, i % 2 == 0);
        Config::SetCurrent(Config::MAIN_AUDIO_FILL_GAPS, i % 2 == 0);
      }
    }
  });
  std::thread audio([&] {
    short silence[32]{};
    start = true;
    for (int i = 0; i < 20000; ++i)
    {
      assert(mixer->Mix(silence, 16) == 16);
      for (short sample : silence)
        assert(sample == 0);
    }
  });
  writer.join();
  audio.join();
  mixer.reset();
  Config::Shutdown();
  std::cout << (argc > 1 ? "Native Mixer sample-rate concurrency passed\n" :
                           "Native Mixer playback-config concurrency passed\n");
}
