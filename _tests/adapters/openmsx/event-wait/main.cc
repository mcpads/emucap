// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#include "EventDistributor.hh"
#include "EventListener.hh"
#include "Reactor.hh"
#include <cassert>
#include <chrono>
#include <iostream>
#include <thread>
using namespace openmsx;
using namespace std::chrono_literals;
int main() {
  Reactor reactor;
  EventDistributor events(reactor);
  EventListener listener;
  events.registerEventListener(EventType::Command, listener);
  auto start = std::chrono::steady_clock::now();
  assert(events.sleep(20000));
  assert(std::chrono::steady_clock::now() - start >= 15ms);

  // The notification has already happened: the queue predicate must prevent sleeping.
  events.distributeEvent(Event{EventType::Command});
  start = std::chrono::steady_clock::now();
  assert(!events.sleep(1000000));
  assert(std::chrono::steady_clock::now() - start < 250ms);
  assert(listener.delivered == 0); // Waking does not dispatch on the producer thread.
  events.deliverEvents();
  assert(listener.delivered == 1);
  assert(events.sleep(1000)); // Consumed commands do not leave a permanent wake flag.

  std::thread producer([&] {
    std::this_thread::sleep_for(20ms);
    events.distributeEvent(Event{EventType::Command});
  });
  bool expired = events.sleep(1000000);
  producer.join();
  assert(!expired);
  assert(listener.delivered == 1);
  events.deliverEvents();
  assert(listener.delivered == 2 && reactor.wakes == 2);
  events.distributeEvent(Event{EventType::Unobserved});
  assert(events.sleep(1000));
  events.unregisterEventListener(EventType::Command, listener);
  std::cout << "Native event wait: pending, concurrent, drained and timeout paths passed\n";
}
