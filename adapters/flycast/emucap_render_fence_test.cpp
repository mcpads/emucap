#include "emucap_render_fence.h"
#include <cassert>
#include <future>
#include <array>

using Fence = EmucapRenderFence;
using Result = Fence::Result;
using Clock = std::chrono::steady_clock;

int main() {
  Fence fence;
  auto issue = [&] { return fence.issue(Clock::now() + std::chrono::seconds(2)); };
  auto first = issue();
  assert(first.id() && first.generation());
  assert(fence.observe(first) == Result::pending);
  assert(fence.acknowledge(first));
  assert(!fence.acknowledge(first));
  assert(fence.wait_until(first, Clock::now()) == Result::complete);

  auto queued = issue();
  fence.new_generation();
  assert(fence.observe(first) == Result::invalidated);
  assert(fence.observe(queued) == Result::invalidated);
  auto replacement = issue();
  assert(replacement.id() > queued.id());
  assert(replacement.generation() > queued.generation());
  assert(!fence.acknowledge(queued));
  assert(fence.observe(replacement) == Result::pending);

  Fence other;
  assert(!other.acknowledge(replacement));
  assert(other.observe(replacement) == Result::invalidated);
  assert(fence.observe(Fence::Ticket{}) == Result::invalidated);

  assert(fence.wait_until(replacement, Clock::now()) == Result::timed_out);
  assert(!fence.acknowledge(replacement));
  auto after_timeout = issue();
  assert(fence.acknowledge(after_timeout));
  assert(fence.observe(replacement) == Result::timed_out);
  // A late acknowledgment is rejected even before the waiter has run.
  auto expired = fence.issue(Clock::now());
  assert(!fence.acknowledge(expired));
  assert(fence.observe(expired) == Result::timed_out);

  auto failed = issue();
  auto failure_wait = std::async(std::launch::async, [&] {
    return fence.wait_until(failed, Clock::now() + std::chrono::seconds(2));
  });
  fence.fail();
  assert(failure_wait.get() == Result::failed);
  assert(!fence.acknowledge(failed));
  assert(fence.observe(issue()) == Result::failed);
  fence.new_generation();
  assert(fence.observe(failed) == Result::invalidated);

  // Deterministic synthetic renderer: the queue consumer owns both writes and
  // acknowledges only after them. The waiting owner cannot observe completion early.
  std::array<int, 2> memory{{0, 0}};
  auto writer_ticket = issue();
  std::promise<void> started, release;
  auto allow_write = release.get_future();
  auto writer = std::async(std::launch::async, [&] {
    memory[0] = 7;
    started.set_value();
    allow_write.wait();
    memory[1] = 9;
    assert(fence.acknowledge(writer_ticket));
  });
  started.get_future().wait();
  assert(fence.observe(writer_ticket) == Result::pending);
  auto reader = std::async(std::launch::async, [&] {
    assert(fence.wait_until(writer_ticket, Clock::now() + std::chrono::seconds(2)) == Result::complete);
    assert(memory[0] == 7 && memory[1] == 9);
  });
  assert(reader.wait_for(std::chrono::milliseconds(0)) == std::future_status::timeout);
  release.set_value();writer.get();reader.get();

  auto cancelled = issue();
  auto cancel_wait = std::async(std::launch::async, [&] {
    return fence.wait_until(cancelled, Clock::now() + std::chrono::seconds(2));
  });
  fence.new_generation();
  assert(cancel_wait.get() == Result::invalidated);
  assert(!fence.acknowledge(cancelled));
  fence.fail();
  fence.new_generation(false);
  assert(fence.observe(issue()) == Result::failed);
  fence.new_generation();
  auto recovered = issue();
  assert(fence.acknowledge(recovered));

}
