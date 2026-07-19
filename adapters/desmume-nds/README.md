# emucap — DeSmuME (Nintendo DS) adapter

The adapter that gives emucap Nintendo DS support. It **forks and builds DeSmuME headless**, and the
emucap **NDS bridge** attaches to the **ARM9/ARM7 GDB stubs** DeSmuME exposes to drive memory,
registers, stepping, and breakpoints over GDB-RSP. Same GDB-bridge shape as the PC-98 adapter.

## Architecture

```
emucap Core ──emucap protocol (TCP)──▶ emucap-desmume-nds-bridge ──GDB-RSP──▶ desmume-cli (headless)
                                                              ├─ 127.0.0.1:<arm9 port> = ARM9 stub
                                                              └─ 127.0.0.1:<arm7 port> = ARM7 stub
```

- DeSmuME is dual-CPU (ARM9 ~67 MHz / ARM7 ~33 MHz), so there is **one GDB stub and port per CPU**.
  The bridge attaches to both; tools route to ARM9 by default, and the `arm7` memory_type (or a `cpu`
  parameter) selects ARM7. The two GDB ports are OS-assigned free ports (override with
  `NDS_ARM9_GDB_PORT` / `NDS_ARM7_GDB_PORT`).
- Upstream `desmume-cli` unconditionally creates an X11 (`XInitThreads`) and OpenGL SDL window, so it
  won't start without a display. `patches/0001-headless-cli.patch` removes the X11/window/renderer
  setup and leaves only the emulate loop and the GDB stubs (gated on `#define HEADLESS_SPIKE`, the
  original preserved under `#else`). The adapter owns this fork (like Mednafen / Flycast).

## Build

```sh
adapters/desmume-nds/build.sh
```

Clones TASEmulators/desmume into an emucap-owned work tree (`adapters/desmume-nds/work`) pinned to a
known-good commit, applies the patch stack in order (`0001` headless → `0002` screenshot/input →
`0003` savestate/disasm → `0004` reset → `0005` touch → `0006` GDB buffers → `0007` input status →
`0008` GDB I/O deadlines → `0009` SIGPIPE suppression → `0010` shared-scheduler GDB state),
and builds `desmume-cli` with meson
(`-Dfrontend-cli -Dgdb-stub`; the gdb-stub build disables the JIT and runs the interpreter). Because
later patches extend the same `gdbstub.cpp` regions, build.sh resets the tree and re-applies the whole
stack every build. Point it at a read-only upstream checkout with `EMUCAP_DESMUME_SRC=/path/to/desmume`.

- Dependencies: `meson`, `ninja`, `sdl2`, `glib` (+ the system `libpcap` on macOS), and `git`.
- On macOS it builds with Apple clang (`/usr/bin/clang`) — build.sh normalizes a homebrew-LLVM environment.

## Launch

The agent brings it up with `launch(content_path=<rom.nds>, system="nds")` (the MCP tool). Internally:

```sh
adapters/desmume-nds/launch.sh <rom.nds> <EMUCAP_PORT> [EMUCAP_NAME]
```

The launcher starts desmume-cli headless (`--arm9gdb <p9> --arm7gdb <p7>`), waits for both GDB ports
to open, then attaches the bridge. **No NDS BIOS or firmware is needed** — DeSmuME's HLE BIOS and
direct-boot boot commercial ROMs.

Each accepted GDB connection has bounded send/receive waits. Packet transmission and ACK handling
share a two-second total deadline and at most three attempts; a timeout closes only that GDB
connection and resets the packet reader so a replacement bridge can attach without restarting
DeSmuME. Socket sends suppress `SIGPIPE`, so a peer reset also closes only the failed GDB connection.

## System and content

- System name: `nds` (aliases `ds`, `nintendo-ds`, `desmume`). Content extension: `.nds`.

## memory_types

`status.memory_types` is authoritative. v1:

| memory_type | routed CPU | meaning |
|---|---|---|
| `main` | ARM9 | Main RAM `0x02000000`+ (4 MB, shared by ARM9/ARM7). Game state lives here. |
| `arm9` | ARM9 | The full ARM9 bus (offset = absolute address). |
| `arm7` | ARM7 | The full ARM7 bus (offset = absolute address). |

## Buttons

`a` `b` `x` `y` `l` `r` `start` `select` `up` `down` `left` `right`. Aliases `enter`/`return`→`start`,
`l1`→`l`, `r1`→`r`. The microphone is not injected by name. **The touchscreen is a separate `touch`
tool (screen coordinates)** — see Tier 2.

## Tool availability — Tier 1 / Tier 2 / Tier 3

**Tier 1 (through the GDB stub)**: `read_memory` / `write_memory` (RSP `m` / `M`), `get_state` (ARM
registers r0-r15, pc, sp, lr, cpsr via `g`), `step(unit="instructions")` (`s`, backed by the
adapter's `step_instructions` wire method), `set_breakpoint`
(exec = `Z0`), `clear_breakpoint`, `pause` / `resume` (break / `c`), `poll_events`.

**Tier 2 (outside GDB — custom RSP hooks owned by the fork, `patches/0002-emucap-hooks.patch`)**:
- `screenshot` — the fork's `qEmucap,ss` encodes `GPU->GetDisplayInfo().masterNativeBuffer16` (both
  256×384 screens) to PNG (zlib) and returns it base64. The bridge returns `{png_base64, width:256,
  height:384}`. Headless skips the draw, but the GPU render result is still in the buffer — only the
  backlight scale is omitted.
- `set_input` / `press_buttons` — button name → 12-bit mask → the fork's `QEmucap,input:<mask>[,<frames>]`.
  The fork folds the override in `NDS_beginProcessingInput` every frame (beating the front end's
  per-frame reset). Injected on ARM9. press_buttons' frame countdown only advances while the emulator
  runs, so resume/continue to let the frames elapse.
- `touch` — touch the bottom screen (256×192) at `(x, y)`. The fork's `QEmucap,touch:<hexX>,<hexY>[,<hexframes>]`
  (or `:release`) → `NDS_setEmucapTouchOverride` folds it every frame in `NDS_beginProcessingInput`
  (applied by `NDS_applyFinalInput`), symmetric with the button override. `frames>0` presses for that
  many frames then lifts (a tap), `release:true` lifts, and with neither it holds until the next touch.
  This is a screen-coordinate touch, distinct from a button `tap`. Required to get past touch-only
  titles (e.g. Love Plus). `patches/0005-emucap-touch.patch`.

**Tier 3 (custom RSP hooks, `patches/0003-emucap-state-disasm.patch`; call_stack is bridge-only)**:
- `save_state` / `load_state` — the fork's `QEmucap,{save,load}state:<hexpath>` calls DeSmuME's native
  `savestate_save` / `savestate_load` (saves.cpp). The path is hex-encoded to be RSP-safe. Returns
  `{path, status}`. A savestate is global state (both cores + PPU/SPU), so it rides the ARM9
  connection — call it while stopped.
- `disassemble` — the fork's `qEmucap,disasm:<addr>,<count>[,<mode>]` decodes instructions with
  DeSmuME's disassembler tables (`des_{arm,thumb}_instructions_set`) and returns `<addr>|<opcode>|<text>`
  lines base64-encoded. With no mode, ARM/Thumb is chosen from the CPU's CPSR T-bit (force with `arm` /
  `thumb`). The bridge parses `[{addr, bytes, text}]` (`bytes` is little-endian memory order). Route the
  CPU with the `cpu` parameter (ARM9 default).
- `call_stack` — **bridge-only, best-effort, no fork change**. Reads pc/lr/sp/r11 via `g`, then
  frame0=pc, frame1=lr, and walks the ARM APCS r11 frame-pointer chain (`[fp-4]`=saved lr,
  `[fp-12]`=saved fp) via `m`. It ends shallow when the game doesn't keep r11 as a frame pointer — the
  reply carries `method:"lr+fp-walk (best-effort)"`, a `note`, and an `in_code_region` flag per frame.
- `reset` — the fork's `QEmucap,reset` calls DeSmuME `NDS_Reset` (a power cycle). ARM9 returns to
  `0x02000800`, ARM7 returns to `0x02380000`, and both stay halted (stub breakpoints survive the
  reset).

**Not yet supported (needs more fork hooks)**: `run_frames` (a frame counter), `watch_register`,
`set_trace` / `get_trace`, `break_on_reset`. `status.capability_notes` is authoritative — the
interface doesn't accumulate caveats: names are shared, availability is in `status`.

## Dual-CPU execution model (important)

DeSmuME exposes separate ARM9 and ARM7 GDB endpoints, but both CPUs share one execution scheduler.
They therefore transition together: a session is either running on both CPUs or frozen on both.
The fork keeps both stub states synchronized without sending duplicate stop packets.

- `resume` and `pause` use the ARM9 endpoint by default and change both CPUs' execution state.
- `cpu:"arm7"` selects the ARM7 debugger endpoint for the operation; it does not create an
  independently running ARM7.
- `resume(cpu:"both")` remains a compatibility alias. It sends one ARM9 continue packet, not one
  packet per endpoint.
- A breakpoint or instruction step on either CPU stops the shared scheduler and leaves both
  endpoints available for inspection.

Reads, writes, registers, disassembly, breakpoints, and stepping are still routed to a selected CPU.
A temporary routed stop guards shared Main RAM without a second interrupt, then restores the
session's prior running or frozen state. `status.cpus` reports both endpoint states explicitly.

## Operational notes

- **Halt-on-start**: the stubs start stopped. Drive with `resume` / `step` after the bridge attaches.
- **Ignores SIGTERM**: desmume-cli doesn't die on `kill` (SIGTERM) — the launcher escalates to SIGKILL.
- **Interpreter**: the gdb-stub build disables the JIT (same as Flycast).
- **`M` (write) reply**: DeSmuME performs the write but answers `M` with an empty packet, not "OK" —
  the bridge treats an empty reply as success and only an `E` code as failure.
