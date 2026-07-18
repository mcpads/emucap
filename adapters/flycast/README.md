# emucap — Flycast (Dreamcast) adapter

Live-debug the Dreamcast (SH-4) with emucap.

## What the user provides (agent: relay these by name)

You (the agent) run `build.sh` and launch yourself. Three inputs come from the **user** — walk them
through each by exact filename/path and confirm before proceeding:

1. **Flycast source checkout, optional** — `build.sh` uses `FLYCAST_SRC` only as a read-only Git object source.
   It fetches the commit pinned in `upstream.lock` into an emucap-owned cache, checks out the pinned recursive
   submodule graph there, and only then copies source into the patch/build tree. It does not read modified
   working files from, patch, or remove the user's checkout. Normally the agent handles the source:
   ```bash
   adapters/flycast/build.sh
   ```
   — or set `FLYCAST_SRC=<path to an existing checkout>` to reuse its local Git objects as the fetch origin.
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

## Native adapter

A native adapter is the supported path. The build injects `emucap.cpp`/`emucap.h` into the Flycast
work tree (no GDB bridge needed), and `emucap_service()` connects directly to the Control MCP over
NDJSON from `vblank()`. Its advertised methods include status·read_memory·write_memory·get_state
(SH-4 registers)·save_state·load_state·run_frames·screenshot (running or frozen)·set_input·pause·
resume·step (frame)·reset·set_breakpoint·clear_breakpoint·clear_all_breakpoints·list_breakpoints·
poll_events·find_pattern·disassemble·get_rom_info. Server-composed verbs such as `tap` are
available when their primitive dependencies exist. `status.methods` is
authoritative; atomic frame-boundary search is unavailable because the native adapter has no `probe`.

A replacement Control MCP can reconnect without restarting Flycast. Do not treat a disconnected
socket as permission to relaunch: inspect `status.continuity`, `status.runtime_instance`, and
`get_failure_context` first. A blocked fatal SH-4 exception preserves its exact registers and recent
PC ring before upstream state changes, then permits read-only diagnostics in a bounded quarantine.
After collecting the evidence, `dismiss_failure` explicitly ends the quarantine and continues the
existing termination path; it is not guest recovery.

Native adapter limitations (graceful refusal): read/write watchpoints·instruction-unit step (given the freeze model)·dump_memory
(a flat-address 16MB dump is a read8 loop, so it is slow). The native adapter does implement
`set_trace`/`get_trace`/`watch_register`/`call_stack`; the fatal PC ring is separate from opt-in tracing.

**The exec breakpoint is instruction-precise via a hook in the interpreter's Run() loop** — build.sh injects
`if (g_emucap_bp_armed && emucap_exec_bp_check(pc)) emucap_bp_spin(pc);` into sh4_interpreter.cpp (when armed is false it only
checks the guard flag). On a hit, emucap_bp_spin stops and services the socket before that instruction executes.
Read/write watchpoints and `step(unit="instructions")` are refused because the required memory-access and instruction-step
contracts are not available under the native adapter's vblank-frame freeze model.

Mute: sound can be turned on with `EMUCAP_MUTE=0` (default 1 = muted). The launcher writes `aica.Volume` only in
the emucap-owned config copy.

⚠ **screenshot works via a continuous buffer.** GetLastFrame needs the GL context (UI thread), but freeze (vblank-spin) blocks
UI rendering, so a gui_runOnUiThread/deferred approach deadlocks. Instead, mainui_rend_frame copies the latest raw frame into a
buffer on every render via `emucap_capture_latest()`, and on a screenshot request the emu thread PNG-encodes that buffer
(no GL needed) → it works even while frozen (buffer = the frame just before freeze = the frozen frame). ⚠ After a load_state while
frozen the screen buffer is not refreshed (UI rendering is stopped). Until a new rendered frame is captured, `screenshot`
fails with `bad_state` instead of returning the pre-load image; advance one frame with `step(1)` and retry.

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
The adapter `build` identity uses the form
`<emucap-revision>@flycast-<upstream-revision>`, so `status.emulator_build` distinguishes both inputs.

⚠ macOS arm64: a rebuilt .app has no JIT signature, so **dynarec can crash before the adapter connects**. The build skips
recompiler initialization when the interpreter is selected, and the launcher also forces `Dynarec.Enabled=no` for the isolated
instance.

## Usage

The launcher runs Flycast from an emucap-owned runtime copy and seeds an isolated `emu.cfg` under
`EMUCAP_EMU_HOME/flycast/<port>/`; it also copies an existing user `emu.cfg` as input when present.
When a BIOS is required, the seeded `[config]` includes:
```ini
Dreamcast.BiosPath = <directory containing dc_boot.bin>
```
`Dreamcast.BiosPath` is the **directory** holding the user-supplied `dc_boot.bin` (see "What the user provides"); omit it if HLE-booting.

Procedure:
```bash
# 1) call emucap-mcp bootstrap/status and use the returned listening_port.
# 2) Prefer the MCP launch tool; it prepares the runtime copy, config, and native Flycast adapter.
# 3) Legacy fallback when running outside the MCP launch tool:
adapters/flycast/launch.sh "<disc.gdi>" <listening_port> [name]
# 4) control via emucap MCP tools: status → confirm {adapter:"flycast"}, then pause/get_state/read_memory/step/set_breakpoint
```

Addresses are all SH-4 addresses (main RAM `0x8C......`, 1ST_READ.BIN from `0x8C010000`). hex strings accepted.
For an accurate snapshot, read after `pause` (emucap determinism convention).
