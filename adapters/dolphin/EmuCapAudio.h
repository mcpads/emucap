// Host-only evidence from native audio lifecycle operations. No guest state is stored here.
#pragma once

#include <cstdint>
#include <mutex>
#include <string>
#include <utility>

namespace AudioCommon
{
struct OutputObservation
{
  std::uint64_t generation = 0;
  std::string backend;
  bool initialized = false;
  const char* phase = "absent";
  const char* last_run_result = "unattempted";
  bool start_verified = false;
  const char* failure = nullptr;
  const char* fallback = nullptr;
};

// Stream lifetime and commands remain serialized by their native owners. This lock protects
// copied metadata only; callers never retain it across device operations or native callbacks.
class OutputObservationOwner
{
public:
  using Token = std::uint64_t;

  OutputObservation Read() const
  {
    std::lock_guard lock(m_mutex);
    return m_observation;
  }

  Token BeginInitialization()
  {
    std::lock_guard lock(m_mutex);
    const auto generation = m_observation.generation + 1;
    m_observation = {};
    m_observation.generation = generation;
    m_observation.phase = "initializing";
    return ++m_operation;
  }

  void Constructed(Token token, std::string backend, const char* fallback = nullptr)
  {
    std::lock_guard lock(m_mutex);
    if (token != m_operation)
      return;
    m_observation.backend = std::move(backend);
    m_observation.fallback = fallback;
  }

  void Initialized(Token token, bool success)
  {
    std::lock_guard lock(m_mutex);
    if (token != m_operation)
      return;
    m_observation.initialized = success;
    m_observation.failure = success ? nullptr : "initialization_failed";
    m_observation.phase = "ready";
  }

  Token BeginRun(bool running)
  {
    std::lock_guard lock(m_mutex);
    m_observation.phase = running ? "starting" : "stopping";
    return ++m_operation;
  }

  void FinishedRun(Token token, bool running, bool success)
  {
    std::lock_guard lock(m_mutex);
    if (token != m_operation)
      return;
    m_observation.phase = "ready";
    m_observation.last_run_result = success ? (running ? "started" : "stopped") : "failed";
    if (m_observation.initialized)
      m_observation.failure = success ? nullptr : (running ? "start_failed" : "stop_failed");
    if (running && success)
      m_observation.start_verified = true;
  }

  Token BeginClose()
  {
    std::lock_guard lock(m_mutex);
    m_observation.phase = "closing";
    return ++m_operation;
  }

  void Closed(Token token)
  {
    std::lock_guard lock(m_mutex);
    if (token != m_operation)
      return;
    const auto generation = m_observation.generation;
    m_observation = {};
    m_observation.generation = generation;
    ++m_operation;
  }

private:
  mutable std::mutex m_mutex;
  OutputObservation m_observation;
  Token m_operation = 0;
};
}  // namespace AudioCommon
