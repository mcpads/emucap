# emucap — PPSSPP (PSP) adapter

The adapter that gives emucap PlayStation Portable support. Headless PPSSPP already exposes an
external debugger over its own JSON WebSocket (`debugger.ppsspp.org`); the emucap **PSP bridge**
(`emucap-ppsspp-bridge`) is a pure WebSocket client that relays it to the emucap wire protocol. A
small repo-owned fork adds the two commands PPSSPP's stock API is missing (savestate, and a
screenshot variant that works while the game is running).

## Architecture

```
emucap Core ──emucap protocol (TCP)──▶ emucap-ppsspp-bridge ──WebSocket (JSON)──▶ PPSSPPHeadless
                                                          ws://127.0.0.1:<debugger port>/debugger
                                                          subprotocol: debugger.ppsspp.org
```

- No GDB stub, no per-CPU split — PPSSPP has one CPU context and a single JSON WebSocket endpoint
  (`Core/Debugger/WebSocket.cpp`). A request is `{"event": "<name>", ...params}`; the reply reuses
  the event name; PPSSPP also emits spontaneous events (log lines, breakpoint/stepping hits)
  unprompted, so the bridge queues anything it wasn't waiting for.
- Much of the debugging surface — memory r/w, register r/w, disassembly, input injection, and
  memory (read/write) breakpoints — is stock PPSSPP, no fork needed. The adapter owns a small fork
  (like Mednafen/Flycast/DeSmuME) for the parts headless PPSSPP does not serve on its own:
  - `patches/0001-emucap-savestate-screenshot-ws.patch` adds the two commands stock PPSSPP has no
    WebSocket command for at all: `savestate.save`/`savestate.load` (a new `SaveStateSubscriber.cpp`)
    and `emucap.screenshot` (a `gpu.buffer.screenshot` variant in `GPUBufferSubscriber.cpp` that
    drives GE stepping itself instead of requiring the caller to already be GE-stepping). It also
    keeps the headless run loop alive across GE stepping so a running-game screenshot works.
  - `patches/0002-emucap-headless-exec-control.patch` makes **exec breakpoints and single-step**
    work in the *headless* build. Stock headless never runs the GUI's per-frame
    `g_breakpoints.Frame()` (`UI/NativeApp.cpp`), which is where a newly added breakpoint invalidates
    the JIT/IR block cache so the recompiled block gains its breakpoint check — so under the default
    JIT (and the IR interpreter) an exec breakpoint added over the WebSocket never fired; only the
    plain `-i` interpreter, which checks per instruction, honored it. The patch calls
    `g_breakpoints.Frame()` in the headless run loop (`headless/Headless.cpp`). It also fixes a unit
    bug in the WebSocket single-step path: `Core_PerformCPUStep` treats the step size as bytes, but
    `cpu.stepInto` passed an instruction *count* (1), so `(1/4)=0` instructions executed and the PC
    never advanced (`SteppingSubscriber.cpp`, `CPUCoreSubscriber.cpp`). Both are exercised only by
    the WebSocket debugger, which is why upstream's GUI never hit them.
  - `patches/0005-emucap-gui-debugger-port.patch` makes the **GUI** build honor `--debugger=<port>`.
    Headless PPSSPP acts on that flag in its own `main()`; the SDL/GUI frontend forwarded the arg to
    `NativeInit` but never acted on it, so the GUI's debugger WebServer bound an *auto-assigned* port
    (`iRemoteISOPort` defaults to 0 = OS-assigned). The patch parses `--debugger=<port>` in
    `UI/NativeApp.cpp` after the config `Load()` and pins `iRemoteISOPort` to it plus arms
    `bRemoteDebuggerOnStartup` (both `DoNotSaveSetting`, so the session's ephemeral port and the
    forced-on flag never persist into the user's real config) — the emucap launcher can then hand the
    same reserved port to the bridge. This is what lets `display:true` (below) use one deterministic
    port instead of discovering an auto-assigned one.
  - `patches/0006-emucap-gui-isolated-memstick.patch` makes the **GUI** build honor
    `EMUCAP_PPSSPP_MEMSTICK` — an env var that pins the memory-stick (config + saves) root to an
    isolated per-session directory *before* the config `Load()`. Without it a `display:true` window
    would read and pollute the operator's real PPSSPP profile: on macOS the stock memstick path comes
    from `NSUserDefaults` (`UserPreferredMemoryStickDirectoryPath`, else
    `defaultCurrentDirectory/.config/ppsspp`), which `HOME`/`XDG` alone cannot fully redirect. The
    launcher sets this env (plus `HOME`/`XDG_CONFIG_HOME`) to the emucap-owned per-port run dir, so a
    HITL session never touches the user's real config/saves/control mappings even if they configured a
    custom memory stick in their own PPSSPP.
- `cpu.stepInto`/`stepOver`/`stepOut`/`runUntil`/`nextHLE` ack via a *differently named* spontaneous
  `cpu.stepping` event rather than a reply of their own name (`SteppingSubscriber.cpp`) — the bridge
  handles this with a send-then-wait-for-a-different-event primitive, not a naive call/reply demux.

## Build

```sh
adapters/ppsspp/build.sh
```

Clones `hrydgard/ppsspp` into an emucap-owned work tree (`adapters/ppsspp/work`) pinned to a known
commit, applies the patch stack, and builds **two targets** with CMake (`-DHEADLESS=ON`, one
configure): `PPSSPPHeadless` (the default headless debugging build) and `PPSSPPSDL` (the GUI build
for HITL `display:true`). `HEADLESS=ON` adds the headless target *alongside* the default desktop SDL
target — they are not mutually exclusive, so both come out of the same patched tree and both carry
the identical fork patch stack. The patch also adds new source files, so build.sh resets tracked
files to the pinned commit and drops the patch-created untracked sources before re-applying, making
every build idempotent. `EMUCAP_PPSSPP_SRC=/path/to/ppsspp` supplies a read-only upstream checkout to
`git clone` locally (skips the network) instead of `hrydgard/ppsspp`; the pinned checkout, patch
stack, and build still happen only in the emucap-owned work tree, so the supplied checkout is never
patched or built in.

`PPSSPPHeadless` is the **guaranteed default target** — if it fails to build, build.sh fails.
`PPSSPPSDL` (the HITL `display:true` window) is built **best-effort**: it additionally needs SDL3
(`brew install sdl3 sdl3_ttf`), so on a host that has only the headless prerequisites its target is
allowed to fail without failing the whole build — build.sh prints a warning that headless is ready
and SDL3 is needed for `display:true`, and exits successfully. Set `EMUCAP_PPSSPP_BUILD_GUI=0` to
skip the GUI target entirely (headless-only). On macOS the GUI output is an `.app` bundle
(`build-headless/PPSSPPSDL.app`), whose executable emucap launches directly; it runs under the
adhoc-signed JIT from a rebuilt bundle (no crash) and boots a commercial ISO in a Vulkan window.

- Dependencies: `cmake`, a C++ toolchain, and `git` (the clone pulls submodules, including bundled
  FFmpeg).
- On macOS it builds with Apple clang (`/usr/bin/clang`) — build.sh normalizes a homebrew-LLVM
  environment.

## Launch

The agent brings it up with `launch(content_path=<game.iso|game.cso|game.pbp>, system="psp")` (the
MCP tool). Internally:

```sh
adapters/ppsspp/launch.sh <game.iso|game.cso|game.pbp> <EMUCAP_PORT> [EMUCAP_NAME]
```

This is a two-process launch (headless PPSSPP + the bridge), the same shape as DeSmuME NDS:

- `PPSSPPHeadless --debugger=<port> --graphics=software <content>`. The content is a **positional**
  boot argument — `-m`/`--mount` alone only mounts a *second* image on `umd1:` for ELF+CSO test
  harnesses and leaves the boot list empty. `--timeout` (a headless test-harness flag that aborts
  the run after N wall-clock seconds regardless of debugger activity) is never passed, so the run
  has no deadline.
- `--debugger` forces `startBreak=true` — **PPSSPP halts before boot**; `resume` to run.
- No display, no window, no GPU backend needed (`--graphics=software`) — the default headless path is
  truly headless, so no `caffeinate`/display-awake handling is required.
- No PSP firmware/BIOS is needed to boot a commercial ISO/CSO/PBP.

### HITL display mode (`display:true`)

`launch(content_path=<...>, system="psp", display=true)` opens a **real PPSSPP window a human sees
and plays** while the agent drives the same debugger WebSocket — this is emucap's human-in-the-loop
(HITL) core purpose. It launches the `PPSSPPSDL` GUI build instead of `PPSSPPHeadless`, with the same
positional-content boot and the same reserved `--debugger=<port>` handed to the bridge; the GUI
honors that port via fork patch 0005 (above). Differences from headless:

- **A human plays with PPSSPP's own input.** The human uses PPSSPP's built-in keyboard/gamepad
  mapping (Developer/Controls settings inside the window) — keys go straight into the emulated PSP.
  The agent's `press_buttons`/`set_input` (over the WebSocket) and the human's input coexist on the
  same PSP pad. On macOS, host keyboard input requires the window to be focused and the macOS **input
  source set to a Latin/English layout** — a Hangul/CJK IME intercepts keystrokes before PPSSPP sees
  them (same gotcha as the NDS HITL notes); the mouse/gamepad are unaffected.
- **The game boots running, not halted.** The GUI does not set `startBreak`, so the game runs
  immediately for the human — no initial `resume` needed. The agent attaches to a running core and
  can `pause`/`resume`/read/inject at will.
- **A GPU window (Vulkan/GL), not software.** `--graphics=software` is omitted so the window renders
  on the real backend. On macOS a `caffeinate -d -w <pid>` keeps the display awake for the window's
  lifetime (the window dies if the display sleeps — same as the Flycast/Mesen GUI adapters).
- **Same debugger surface.** Everything under "Tool availability" works identically — the bridge
  connects to the GUI's WebSocket exactly as it does to headless.
- **Isolated per-session profile — never the user's real PPSSPP.** The launcher points the GUI at an
  emucap-owned per-port run dir via `HOME`/`XDG_CONFIG_HOME` and the fork's `EMUCAP_PPSSPP_MEMSTICK`
  (patch 0006). All config, saves, and control mappings the human sets in the window land in that
  disposable dir; the operator's real PPSSPP config/saves are never read or overwritten — verified
  live on macOS to beat a `NSUserDefaults`-configured custom memory stick. So the Developer/Controls
  settings above are per-session and do not persist into the user's own PPSSPP.

**ESC pause-menu gotcha (known follow-up).** When the human opens PPSSPP's own **ESC pause menu**, the
debugger WebSocket goes unresponsive — even a bare `version` request times out — until they close it.
The ESC menu calls `Core_Break(BreakReason::UIPause)` and pushes the pause screen, which backgrounds
the emulation screen so the core run loop stops being pumped; the WebSocket cannot service commands
in that state. So the agent **cannot observe a state the human froze with the ESC menu**. Workaround:
to inspect a paused state, the human should use the **game's own in-game pause** (or ask the agent to
`pause`, which uses `cpu.stepping` and keeps the WebSocket answering) rather than the emulator's ESC
menu; `resume` continues. This is PPSSPP-GUI-specific — the headless build has no pause menu, and NDS
`display:true` (a bare DeSmuME framebuffer window) has no host-level pause menu either, so neither
shares it. Keeping the debugger WebSocket answering read-only commands (`cpu.getAllRegs`/`memory.read`
/`memory.disasm`) while the ESC menu is open is a possible future fork change.

## System and content

- System name: `psp` (aliases `ppsspp`, `playstation-portable`). Content extensions: `.iso`,
  `.cso`, `.pbp`. `.iso` is shared with Saturn/PSX/PCE/MD/Dreamcast — a PSP GAME ISO9660 header
  disambiguates automatically; pass `system="psp"` explicitly if that fails.

## memory_types

`status.memory_types` is authoritative. v1:

| memory_type | meaning |
|---|---|
| `main` | PSP user RAM, base `0x08800000` (`Core/MemMap.h: PSP_GetUserMemoryBase()`). `read_memory`/`write_memory`'s `address`/`start` offset is added to this base. |

`disassemble` and `set_breakpoint` take a raw absolute PSP address instead (no `memory_type` base
added) — e.g. straight from `get_state`'s `cpu.pc`.

## Buttons

`a`→cross(✕), `b`→circle(○), `x`→square(□), `y`→triangle(△), `l`→ltrigger, `r`→rtrigger, plus
`start`, `select`, and the d-pad `up`/`down`/`left`/`right` — mapped to PPSSPP's own PlayStation-style
button names. **Confirm/cancel is game-defined**: Japanese titles typically confirm with circle (`b`)
and cancel with cross (`a`), the opposite of the common Western layout — so a menu may look
unresponsive to `a` while `b` advances it. PPSSPP also has `home`/`screen`/`note`/`hold`/`wlan`/... and
an analog stick, but those are outside the emucap common button surface for v1 (no uniform
analog-input tool yet).

## Tool availability — Tier 1 / Tier 2 / not yet supported

**Tier 1 (stock PPSSPP WebSocket commands, no fork)**: `read_memory`/`write_memory`
(`memory.read`/`memory.write`, base64 on the wire), `dump_memory` (streams each region — today
`main`, user RAM — via `memory.read` in 128 KiB chunks to `<name>.bin` + `regions.json`, plus a
`state.json` register snapshot, the same `.bin`/`regions.json`/`state.json` bundle the cross-ROM
diff loader consumes for every adapter), `get_state` (`cpu.getAllRegs`'s GPR category
flattened to `cpu.<name>`), `disassemble` (`memory.disasm`), `set_breakpoint`/`clear_breakpoint`/
`list_breakpoints`/`clear_all_breakpoints` (`cpu.breakpoint.*` for exec, `memory.breakpoint.*` for
read/write, both with an optional `condition` expression), `step_instructions` (repeated
`cpu.stepInto`, since PPSSPP has no step-count parameter), `pause`/`resume` (`cpu.stepping`/
`cpu.resume`), `poll_events` (draining PPSSPP's spontaneous `cpu.stepping` events), `set_input`/
`press_buttons` (`input.buttons.send`/`input.buttons.press`), `reset` (`game.reset`), and
`get_rom_info` (`game.status` for id/title + a locally computed sha1 of the `EMUCAP_CONTENT` image
— PPSSPP's WS API never exposes a content path or hash itself).

**Tier 2 (the fork's two added commands, `patches/0001-emucap-savestate-screenshot-ws.patch`)**:
- `screenshot` — `emucap.screenshot`, a `gpu.buffer.screenshot` variant that forces GE stepping
  itself, so it works while the game is **running** (stock `gpu.buffer.screenshot` fails unless the
  caller happens to already be GE-stepping, which never naturally overlaps with a CPU-debugger
  halt). If the CPU is halted for the debugger, the EmuThread never reaches a vsync to enter GE
  stepping, so the fork's own 5s wait times out and this fails loudly (`emulator_error`), not a
  hang — resume first.
- `save_state`/`load_state` — `savestate.save`/`savestate.load`. The fork's handler breaks the CPU
  into stepping if it's running, waits for the save/load to complete, then restores the prior
  run/halt state — so this works regardless of whether the CPU is running or halted.

**Not yet supported (`status.capability_notes.planned_methods`/other gaps — `status.methods` is
authoritative, not this list)**:
- `step` (frame-based stepping) — only instruction-based `step_instructions` exists today; no
  fork/PPSSPP primitive advances by video frame yet.
- `run_frames`, and the MCP-composed `tap`/`tap_sequence`/`hold_until` — all depend on a
  frame-level `step`, which this bridge doesn't dispatch.
- `probe`, `find_pattern`, `watch_register`, `set_trace`/`get_trace`, `call_stack`,
  `break_on_reset` — no bridge/fork hook yet.
- The MCP-composed `bisect`/`regression_run`/`verify_determinism` — both of their replay paths
  need either `probe` or a frame-level `step`, neither of which exists here yet, so they report
  `unsupported`.
- Structured breakpoint value-conditions (`value`/`value_mask`/`value_len`) — rejected loudly
  (TODO); use a raw `condition` expression (PPSSPP's own expression language) instead, or
  `pc_min`/`pc_max`, which the bridge compiles into one automatically.
- BP kind `nmi`/`irq`/`dma`, range exec breakpoints (`start` must equal `end`), and
  `auto_savestate`/`snapshot` breakpoint options.

## Operational notes

- **Halt-on-start**: `--debugger` forces `startBreak=true` — `resume` to run after the bridge
  attaches.
- **`reason` is a dead field**: PPSSPP's `cpu.stepping` stop event never actually populates its
  `reason`/`relatedAddress` fields (`Core_Break()` clears the step-command type in the same breath
  it would record the reason) — a breakpoint hit and a plain stepping-completion produce the
  identical bare `{pc, ticks}` event. `poll_events` classifies a hit by matching `pc` against
  tracked exec breakpoints, and for memory breakpoints by a `memory.breakpoint.list` hit-count
  delta (simultaneous memory-breakpoint hits are best-effort — only one is attributed per event).
- **Ack-name mismatch**: `cpu.stepInto`/`stepOver`/`stepOut`/`runUntil`/`nextHLE` ack via a
  `cpu.stepping` event, not a reply of their own name — a naive client that blocks for a reply
  literally named `cpu.stepInto` hangs forever.
- **`press_buttons` needs a running emulator**: PPSSPP's timed press only auto-releases after
  enough emulated frames elapse, which never happens while the CPU is halted for the debugger —
  halted calls are rejected up front rather than hanging. `set_input` (a held state, not timed)
  works regardless of run/halt state.
