# emucap — Flycast (Dreamcast) adapter

> 한국어: [README.ko.md](README.ko.md)

Live-debug the Dreamcast (SH-4) with emucap. The third platform, after SNES (Mesen) and Saturn/PSX (Mednafen).

## What the user provides (agent: relay these by name)

You (the agent) run `build.sh` and launch yourself. Three inputs come from the **user** — walk them
through each by exact filename/path and confirm before proceeding:

1. **Flycast source checkout, optional** — `build.sh` uses `FLYCAST_SRC` only as a read-only source input.
   It copies/clones Flycast into an emucap-owned build tree before injecting `emucap.cpp`; it does not patch
   the user's checkout or remove that checkout's `build/` directory. Normally the agent handles the source:
   if no source is present, the script clones it recursively into the emucap build cache —
   ```bash
   adapters/flycast/build.sh
   ```
   — or set `FLYCAST_SRC=<path to an existing recursive checkout>` to reuse an existing checkout as input.
   Set `EMUCAP_FLYCAST_BUILD_HOME` only to an empty directory or one previously created by `build.sh`.
   Only involve the user if you cannot reach GitHub or need them to pick a location.

2. **Dreamcast BIOS `dc_boot.bin`** — user-supplied. It is copyrighted Dreamcast firmware, **not included**
   with Flycast or emucap, and **must not be committed** to the repo; it comes from the user's own Dreamcast
   console / their own dumps. Put `dc_boot.bin` in a folder and set that folder (the **directory**, not the
   file itself) as `Dreamcast.BiosPath` in `emu.cfg` (see Usage). Flycast can **HLE-boot many games without a
   BIOS**, so this is often optional — ask the user for `dc_boot.bin` only when a game refuses to boot without it.

3. **Game disc** — a `.gdi`, `.cdi`, or `.chd` image the user provides. Pass its path to the MCP `launch`
   tool. `launch.sh` is a legacy fallback.

**OS reality:** macOS (arm64) is the tested runtime path; Linux is experimental; Windows is **BETA**. The Rust
launcher handles Flycast's Windows config model by copying `Flycast.exe` into an emucap-owned portable directory
and writing `emu.cfg` next to that copy. Building Flycast itself on Windows is still unverified here.

## Current status: native fork done (emucap.cpp — all capture/control methods live-verified)

A native adapter that builds by injecting `emucap.cpp`/`emucap.h` into the Flycast tree (no GDB bridge needed). It connects
directly to emucap-mcp over NDJSON, serviced by `emucap_service()` injected into `vblank()`. **Live-verified methods** (2026-06-27,
Puyo Puyo 4): status·read_memory·write_memory·get_state (SH-4 registers)·**save_state·load_state**
(registers confirmed to restore exactly)·**run_frames** (keepalive keeps even long runs from timing out)·**screenshot** (running+frozen)·
**set_input·tap·tap_sequence** (title→mode-select transition)·pause·resume·step (frame)·reset·**set_breakpoint·
clear_breakpoint·clear_all_breakpoints·list_breakpoints·poll_events** (exec BP, instruction-precise stop verified: pc stops
exactly at the BP address)·**find_pattern** (addrspace scan)·**disassemble** (SH4, OpDesc decode)·**get_rom_info** (gameId
HDR-0014, etc.). Server-composed verbs (tap/bisect/hold_until/regression) run on top of these primitives.

Not implemented (graceful refusal / GDB bridge): read/write watchpoints·step_instructions (given the freeze model)·dump_memory
(a flat-address 16MB dump is a read8 loop, so it is slow)·watch_register/get_trace/call_stack (some Mesen-specific ones).

**The exec breakpoint is instruction-precise via a hook in the interpreter's Run() loop** — build.sh injects
`if (g_emucap_bp_armed && emucap_exec_bp_check(pc)) emucap_bp_spin(pc);` into sh4_interpreter.cpp (when armed is false it reads a
single bool, so the hot-loop cost is 0). On a hit, emucap_bp_spin stops and services the socket before that instruction executes.
read/write watchpoints and instruction-level step use the GDB bridge (below). step_instructions is refused because it is
impossible under the vblank-frame freeze model.

Mute: sound can be turned on with `EMUCAP_MUTE=0` (default 1 = muted). The launcher writes `aica.Volume` only in
the emucap-owned config copy.

⚠ **screenshot works via a continuous buffer.** GetLastFrame needs the GL context (UI thread), but freeze (vblank-spin) blocks
UI rendering, so a gui_runOnUiThread/deferred approach deadlocks. Instead, mainui_rend_frame copies the latest raw frame into a
buffer on every render via `emucap_capture_latest()`, and on a screenshot request the emu thread PNG-encodes that buffer
(no GL needed) → it works even while frozen (buffer = the frame just before freeze = the frozen frame). ⚠ After a load_state while
frozen the screen buffer is not refreshed (UI rendering is stopped), so you must advance one frame with `step 1` to re-capture the
buffer before the loaded screen becomes visible.

⚠ **Input is injected at the game's consumption point, not into `kcode[]`.** The source of Flycast input is `kcode[4]` (Lua
`pressButtons` writes here too), but writing to the `kcode[]` global gets reset every frame by `os_UpdateInputState` (UI thread) and
races the emu thread's maple polling → dropped input. So build.sh **overrides `pjs->kcode` with the emucap-injected value in
`MapleConfigMap::GetInput` (emu-thread maple DMA, the point the game actually reads)** — deterministic, without races.
(Writing `mapleInputState` directly fails, overwritten by the kcode→mapleInputState copy.)

Build / run:
```bash
adapters/flycast/build.sh                  # sync source into the emucap build tree, inject hooks there, then build
# Preferred: MCP launch {"content_path": "<disc.gdi>", "system": "dc"}
# Fallback: adapters/flycast/launch.sh "<disc.gdi>" <listening_port>
```
The fallback launcher requires the current `status.listening_port`; it no longer defaults to `47800`. Its
per-port config copy, pidfile, and log live under the emucap data root (`EMUCAP_EMU_HOME` override, otherwise
the OS default shown below).
Default build output:
- macOS: `~/Library/Application Support/emucap/flycast-build/work/build/Flycast.app/Contents/MacOS/Flycast`
- Linux: `${XDG_DATA_HOME:-~/.local/share}/emucap/flycast-build/work/build/flycast`
- Windows BETA: `%LOCALAPPDATA%\emucap\flycast-build\work\build\Flycast.exe`
`FLYCAST_APP` may point to either the executable or a macOS `Flycast.app` bundle.

⚠ macOS arm64: a rebuilt .app has no JIT signature, so **dynarec crashes** → the launcher forces the interpreter
(Dynarec.Enabled=no), which is enough for debugging.

## Earlier approach: GDB-stub bridge PoC

`emucap-gdb-bridge.py` — a PoC that relays Flycast's **built-in GDB stub** (SH-4) to emucap NDJSON.
It proves the emucap loop on Dreamcast without a fork or a build. Live-verified (2026-06-27, Puyo Puyo 4).

**Supported (advertised) methods**: `read_memory`·`write_memory`·`get_state` (SH-4 registers)·`status`·`pause`·`resume`·
`step` (1 instruction)·`set_breakpoint` (exec/SW only)·`clear_breakpoint`·`list_breakpoints`·`poll_events`.
**Unsupported (GDB-stub limits — graceful downgrade)**: screenshot·set_input·save/load_state·run_frames·HW watchpoint.
→ filled in by the native fork (Flycast fork + emucap.cpp socket hooks).

⚠ Attaching the GDB stub turns off dynarec and slows things down (Flycast's design). Fine for instruction-level tracing.

## Usage

Prerequisite: Flycast must be built with `ENABLE_GDB_SERVER=ON` (the emucap build sets this).
The launcher runs Flycast from an emucap-owned runtime copy and seeds an isolated `emu.cfg` under
`EMUCAP_EMU_HOME/flycast/<port>/`; it also copies an existing user `emu.cfg` as input when present.
The seeded `[config]` includes:
```ini
Debug.GDBEnabled = yes
Debug.GDBPort = 3263
Debug.GDBWaitForConnection = no
Dreamcast.BiosPath = <directory containing dc_boot.bin>
```
`Dreamcast.BiosPath` is the **directory** holding the user-supplied `dc_boot.bin` (see "What the user provides"); omit it if HLE-booting.

Procedure:
```bash
# 1) call emucap-mcp bootstrap/status and use the returned listening_port.
# 2) Prefer the MCP launch tool; it prepares the runtime copy, config, Flycast, and bridge.
# 3) Legacy fallback when running outside the MCP launch tool:
adapters/flycast/launch.sh "<disc.gdi>" <listening_port> [name]
# 4) control via emucap MCP tools: status → confirm {adapter:"flycast-gdb"}, then pause/get_state/read_memory/step/set_breakpoint
```

Addresses are all SH-4 addresses (main RAM `0x8C......`, 1ST_READ.BIN from `0x8C010000`). hex strings accepted.
For an accurate snapshot, read after `pause` (emucap determinism convention).

## Native-fork plan

Using the Flycast fork entry points, add emucap.cpp socket hooks to provide all 10
methods + full speed (dynarec kept): `addrspace::read/write*`·`Sh4cntx`·`dc_savestate/loadstate`·
`renderer->GetLastFrame`·`mapleInputState[]`·`Emulator::run/step/stop/start`. GdbServer's asio · emu-thread
stop/start handshake is the threading template.
