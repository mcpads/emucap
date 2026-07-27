# Mupen64Plus N64 adapter

This experimental adapter pins the upstream 2.6.0 source bundle, builds the core
with debugger support, and keeps build products under `work/`. Standard N64 cartridge images do
not require a BIOS. N64DD is outside this adapter's current scope.

Build the pinned bundle and verify its structural PoC:

```sh
./adapters/mupen64plus/build.sh
./adapters/mupen64plus/poc/inspect.sh
```

The bundled `m64p_test_rom.v64` is the upstream project's GPL test ROM. Its digest is pinned in
`upstream.lock`; the ROM and all generated binaries remain in the ignored work tree. The local
core patch makes savestate and worker initialization fail closed. Its digest is also pinned, and
the build restores every patched source from the verified archive before applying it.

The MCP `launch` path validates the build metadata against `upstream.lock` and gives each port an
emucap-owned configuration directory. Both modes load the pinned SDL input and RSP plugins;
`display: true` also loads the pinned Rice video plugin. The frontend writes its controller and
screenshot configuration only inside that runtime directory.

The common surface is status and ROM identity, pause/resume, R4300 state and exact instruction
stepping, bounded RDRAM reads/writes while frozen, and port-0 `set_input`. An empty input set
releases the injected scancodes without disabling native keyboard state.

Visible launch additionally advertises rendered-frame stepping, bounded `press_buttons`, PNG
screenshot, and native save/load. Its callback barrier freezes inside the exact Mupen64Plus frame
callback and rearms before releasing the preceding frame, so multiple display lists in one R4300
task cannot escape between requests. The adapter does not combine that barrier with the core's
separate frame-advance pause. Visible readiness remains false until the first rendered callback,
so a caller cannot pause renderer initialization and move its latency into the first frame
request. Frame and instruction counts use the shared 5,000-operation admission limit and a
250-second total backend budget, below the outer deferred-request deadline.

Screenshot and state requests are asynchronous upstream commands. Screenshot completion is
reported by the video plugin after the core's display-list callback, so capture does not block the
first callback while waiting for work that can only finish later. It waits for the native
completion notification, freezes at the first following callback, and reports both the frame
before the request and that post-completion boundary. Those values may differ by more than one;
ordinary `step(frames, 1)` still requires exactly one callback. The core's host worker may still
finish a save while guest execution is blocked, so the adapter gives its completion callback a
separate bounded one-second window. A plain frame-operation failure is recovered to a debugger
instruction boundary before the error response. If a scheduled screenshot or state operation, or
transient input cleanup, cannot be closed, the dedicated launch generation is stopped rather than
carrying an unowned effect into a later request. A save is first written to a unique sibling and
then published through rollback-aware replacement. Loading invalidates the pre-load frame
sequence and resynchronizes it at the next rendered callback.

The pinned core and plugin bundle completed a clean Clang `scan-build` run with no analyzer
reports. Compiler warnings in the excluded N64DD and Transfer Pak paths remain outside this
cartridge profile; this result is not a claim that those families are ready. The upstream test ROM
passed the maintained control smoke after this ownership correction. A representative commercial
cartridge passed three consecutive cold-launch runs. The smoke covers
consecutive exact frame steps, return to an R4300 instruction boundary, persistent-input
restoration after a bounded pulse, `set_input([])` ownership release, PNG identity, a non-empty
native state, RDRAM restoration after load, immediate post-load PNG completion, and exact frame
step after that capture. This proves the injected plugin path and cleanup, but it does not
substitute for manual native-keyboard testing in every window environment.

Preparation is rollback-owned. If configuration, ROM loading, plugin startup/attachment, or
callback registration fails after `CoreStartup`, the frontend detaches every attached plugin,
closes the ROM, shuts down the core, and unloads each library in reverse order. Dropping a prepared
frontend before execution follows the same closure path.

Headless launch has no video callback. It keeps instruction stepping and persistent input but
omits frame step, input pulse, screenshot, and save/load because their completion currently
depends on the rendered-frame barrier. Breakpoints, reset, `run_frames`, and RSP state remain
unadvertised.
