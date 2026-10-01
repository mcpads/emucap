/* Included by the pinned SDL Cocoa OpenGL backend. The caller owns the swap mutex.
 * A missing display link must not keep Flycast's renderer and guest threads parked. */
static void emucap_sdl_wait_swap(SDL_cond *condition, SDL_mutex *mutex,
                                SDL_atomic_t *passed, int setting, SDL_bool *stalled)
{
    const Uint64 deadline = SDL_GetTicks64() + 100;
    SDL_bool wait_once = setting > 0 ? SDL_TRUE : SDL_FALSE;
    if (*stalled && SDL_AtomicGet(passed) == 0)
        return;
    *stalled = SDL_FALSE;
    while (wait_once || (setting < 0 ? SDL_AtomicGet(passed) == 0
                                    : SDL_AtomicGet(passed) % setting != 0)) {
        const Uint64 now = SDL_GetTicks64();
        if (now >= deadline || SDL_CondWaitTimeout(condition, mutex, (Uint32)(deadline - now)) != 0) {
            *stalled = SDL_TRUE;
            break;
        }
        wait_once = SDL_FALSE;
    }
}
