#include <cassert>
#include <cstdint>
#include <cstdio>
#include <vector>
using Uint64 = uint64_t;
using Uint32 = uint32_t;
using SDL_bool = bool;
constexpr bool SDL_TRUE = true, SDL_FALSE = false;
struct SDL_cond {};
struct SDL_mutex {};
struct SDL_atomic_t { int value; };
struct Wake { unsigned elapsed; int ticks; int result; };
static Uint64 now;
static unsigned calls;
static std::vector<Wake> wakes;
static SDL_atomic_t passed;
static Uint64 SDL_GetTicks64() { return now; }
static int SDL_AtomicGet(SDL_atomic_t *p) { return p->value; }
static int SDL_CondWaitTimeout(SDL_cond*, SDL_mutex*, Uint32 remaining) {
    ++calls;
    if (wakes.empty()) { now += remaining; return 1; }
    const auto w = wakes.front(); wakes.erase(wakes.begin());
    if (w.elapsed > remaining) { now += remaining; return 1; }
    now += w.elapsed; passed.value += w.ticks; return w.result;
}
#include "emucap_sdl_swap_wait.h"
static void reset() { now = 0; calls = 0; wakes.clear(); passed.value = 0; }
int main() {
    SDL_bool stalled = false;
    reset(); wakes = {{8, 1, 0}, {8, 1, 0}};
    emucap_sdl_wait_swap(nullptr, nullptr, &passed, 2, &stalled);
    assert(!stalled && now == 16 && calls == 2);
    reset();
    emucap_sdl_wait_swap(nullptr, nullptr, &passed, 1, &stalled);
    assert(stalled && now == 100 && calls == 1);
    emucap_sdl_wait_swap(nullptr, nullptr, &passed, 1, &stalled);
    assert(now == 100 && calls == 1);
    passed.value = 1; wakes = {{16, 1, 0}};
    emucap_sdl_wait_swap(nullptr, nullptr, &passed, 1, &stalled);
    assert(!stalled && now == 116 && calls == 2);
    reset(); wakes = {{30, 0, 0}, {30, 0, 0}, {30, 0, 0}, {30, 0, 0}};
    emucap_sdl_wait_swap(nullptr, nullptr, &passed, -1, &stalled);
    assert(stalled && now == 100 && calls == 4);
    reset(); stalled = false; passed.value = 1;
    emucap_sdl_wait_swap(nullptr, nullptr, &passed, -1, &stalled);
    assert(!stalled && calls == 0);
    reset(); wakes = {{1, 0, -1}};
    emucap_sdl_wait_swap(nullptr, nullptr, &passed, 1, &stalled);
    assert(stalled && now == 1 && calls == 1);
    std::puts("FLYCAST SDL SWAP WAIT PASSED");
}
