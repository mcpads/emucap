#pragma once

#include <chrono>
#include <algorithm>
#include <condition_variable>
#include <cstdint>
#include <memory>
#include <mutex>

// Renderer completion tickets. Queue integration must acknowledge only after all
// preceding callbacks return. This class does not itself drain a native queue.
class EmucapRenderFence {
 public:
  enum class Result { pending, complete, invalidated, timed_out, failed };

 private:
  struct State {
    std::mutex mutex;
    std::condition_variable changed;
    std::uint64_t generation = 1;
    std::uint64_t next_ticket = 1;
    bool healthy = true;
  };
  struct Completion {
    std::uint64_t generation;
    std::uint64_t id;
    std::chrono::steady_clock::time_point deadline;
    Result result;
  };

 public:
  class Ticket {
    friend class EmucapRenderFence;
    std::shared_ptr<State> owner;
    std::shared_ptr<Completion> completion;
    Ticket(std::shared_ptr<State> state, std::shared_ptr<Completion> value)
        : owner(std::move(state)), completion(std::move(value)) {}
   public:
    Ticket() = default;
    std::uint64_t generation() const { return completion ? completion->generation : 0; }
    std::uint64_t id() const { return completion ? completion->id : 0; }
  };

  EmucapRenderFence() : state_(std::make_shared<State>()) {}
  EmucapRenderFence(const EmucapRenderFence&) = delete;
  EmucapRenderFence& operator=(const EmucapRenderFence&) = delete;
  ~EmucapRenderFence() { fail(); }

  Ticket issue(std::chrono::steady_clock::time_point deadline) {
    const std::lock_guard<std::mutex> lock(state_->mutex);
    // Retire instead of ever reusing a ticket identity.
    if (state_->next_ticket == UINT64_MAX) state_->healthy = false;
    auto value = std::make_shared<Completion>(Completion{
        state_->generation, state_->healthy ? state_->next_ticket++ : 0, deadline,
        state_->healthy ? Result::pending : Result::failed});
    return Ticket(state_, std::move(value));
  }

  bool acknowledge(const Ticket& ticket) {
    if (!belongs(ticket)) return false;
    const std::lock_guard<std::mutex> lock(state_->mutex);
    if (observe_locked(ticket) != Result::pending) return false;
    ticket.completion->result = Result::complete;
    state_->changed.notify_all();
    return true;
  }

  Result observe(const Ticket& ticket) const {
    if (!belongs(ticket)) return Result::invalidated;
    const std::lock_guard<std::mutex> lock(state_->mutex);
    return observe_locked(ticket);
  }

  Result wait_until(const Ticket& ticket, std::chrono::steady_clock::time_point deadline) {
    if (!belongs(ticket)) return Result::invalidated;
    std::unique_lock<std::mutex> lock(state_->mutex);
    deadline = std::min(deadline, ticket.completion->deadline);
    if (!state_->changed.wait_until(lock, deadline, [&] {
          return observe_locked(ticket) != Result::pending;
        })) {
      ticket.completion->result = Result::timed_out;
      state_->changed.notify_all();
    }
    return observe_locked(ticket);
  }

  // Queue cancellation/reset invalidates even previously acknowledged tickets.
  // Recovery is explicit: a failed renderer cannot become healthy on query.
  void new_generation(bool recover = true) {
    const std::lock_guard<std::mutex> lock(state_->mutex);
    if (state_->generation == UINT64_MAX || state_->next_ticket == UINT64_MAX) {
      state_->healthy = false;
    } else {
      ++state_->generation;
      state_->healthy = recover || state_->healthy;
    }
    state_->changed.notify_all();
  }

  void fail() {
    const std::lock_guard<std::mutex> lock(state_->mutex);
    state_->healthy = false;
    state_->changed.notify_all();
  }

 private:
  std::shared_ptr<State> state_;
  bool belongs(const Ticket& ticket) const {
    return ticket.owner == state_ && ticket.completion != nullptr;
  }
  Result observe_locked(const Ticket& ticket) const {
    if (ticket.completion->generation != state_->generation) return Result::invalidated;
    if (!state_->healthy) return Result::failed;
    if (ticket.completion->result == Result::pending
        && std::chrono::steady_clock::now() >= ticket.completion->deadline)
      ticket.completion->result = Result::timed_out;
    return ticket.completion->result;
  }
};
