# emucap — MAME PC-98 adapter PoC

This adapter is a PC-98 experiment (proof-of-concept).  The default path uses a
repo-local MAME Lua plugin (`emucap_gdbstub`) plus the Rust `emucap-mame-pc98-bridge`.  The
plugin exposes enough GDB remote protocol for the bridge to reuse the existing
emucap TCP protocol.  Treat this as an adapter-owned emulator control path, not
as instructions to rely on a user's global MAME/RetroArch setup: launch flags,
runtime directories, debugger protocol, and crash workarounds live in this repo.
PC-98 parity work continues through a repo-owned MAME source build: `build.sh`
fetches a pinned MAME release into `work/mame-src`, applies `patches/*.patch`,
and writes `work/mame` as a safe headless wrapper.  The raw executable is linked
as `work/mame.raw` only for explicit diagnostics.  `launch.sh` prefers
`work/mame` only when it is an executable regular file; a stale source directory
at that path is ignored with a warning.  A global `mame` binary is only a
bootstrap smoke fallback while the adapter has no local hook for a specific
behavior.

## Current result

- The pinned MAME 0.288 source target exposes PC-98 drivers: `pc9801`,
  `pc9801vm`, `pc9821`, `pc9821ap2`.
- `pc9821` and `pc9821ap2` accept `.d88`, `.hdm`, and `.hdi` media.
- `pc9821` is the closest match for existing RetroArch-style BIOS names
  (`itf.rom`, `bios.rom`, `font.rom`, sound ROM), but MAME still requires a
  complete MAME romset with additional board ROMs.  The existing RetroArch BIOS
  files are not enough for a clean MAME boot.
- DOSBox-X can start `machine=pc98` in SDL dummy mode, but its CLI process did
  not honor `-time-limit` in the tested boot command, so it is not a good first
  emucap control surface.
- Stock MAME's built-in C++ `-debugger gdbstub` is not the PC-98 control
  surface.  The unpatched binary exits with `gdbstub: cpuname i386sx not found
  in gdb stub descriptions`, and a patched experiment only exposed generic RSP
  behavior while failing full register restore against MAME's packet buffer
  limit.  The repo-local Lua plugin owns the supported control path.
- The supported MCP control surface runs through `pc9801rs`; the live method set
  is listed under Supported methods below.
- MAME's native `machine:load()`/debugger `stateload` path does not restore RAM
  in the headless debugger setup, so the bridge implements emucap-owned PC-98
  state bundles instead.

## What the user must provide

The agent runs the build and launch itself (`build.sh`, then `launch.sh`).  Two
inputs are user-supplied — the agent should name the exact path, walk the user
through providing each, and confirm before launching.

1. **MAME machine romset (required).**  The PC-98 board ROMs are copyrighted, are
   not shipped in this repo, and must not be committed.  MAME needs the *complete*
   romset for the machine — every board ROM, each under MAME's exact filename —
   not a RetroArch/NP2Kai-style `bios.rom` + `font.rom` + `itf.rom` set.  Those
   RetroArch-style names alone are NOT enough for a MAME boot.
   - Rompath: the launcher defaults to `~/mame/roms` (override with
     `MAME_ROMPATH=/path/to/mame-roms`).
   - The machine's romset goes in its own driver-named folder under the rompath.
     For the `pc9801rs` machine that is `<rompath>/pc9801rs/` (by default
     `~/mame/roms/pc9801rs/`).  MAME resolves romsets by driver name, not by
     nearby folder labels, so the folder must be named for the machine.
   - The exact required filenames and checksums come from MAME — see the BIOS
     note below (`-listroms`).  If the romset is missing or incomplete, MAME
     refuses to start the machine: instead of booting it prints the missing ROM
     filenames, which tells the user exactly what is still needed.

2. **PC-98 disk image (required).**  The game or OS disk to run.  The agent passes
   its path to `launch.sh`.  The file extension decides how it mounts: a `.hdi`
   image loads as a hard disk; other extensions (`.hdm`, `.d88`) load as a floppy.
   The user supplies this file; it is not in the repo.

### OS reality

macOS is the tested path.  Building the repo-local MAME with `build.sh` is slow
and disk-heavy — it fetches and compiles a pinned MAME source tree.  Linux builds
the same way and is experimental.  On Windows this adapter is BETA; treat it as
unverified rather than assuming Windows-specific steps that are not documented
here.

## Launch

For source-owned PC-98 work, build the repo-local MAME first:

```sh
adapters/mame-pc98/build.sh
```

The build output is exposed through a safe wrapper:

```text
adapters/mame-pc98/work/mame
```

The extracted source tree lives separately:

```text
adapters/mame-pc98/work/mame-src
```

The raw built executable is also linked for explicit diagnostics:

```text
adapters/mame-pc98/work/mame.raw
```

`launch.sh` automatically uses that binary before looking for `mame` on
`PATH`.  If an older checkout has `work/mame` as a directory, `launch.sh`
ignores it and `build.sh` removes it before writing the wrapper.  Put
emulator-source changes under `adapters/mame-pc98/patches/` as normal
`patch -p1` files; they are applied in sorted order on each clean source
extract.  Do not treat the `PATH` fallback as the target architecture.  It is
there so agents can keep proving boot, memory, screenshot, and input behavior
while the next C++ hook is being developed.

Set `MAME_WORK=/path/to/build-dir` only to an empty directory or a directory
previously created by `build.sh`.  The script refuses non-empty custom work
directories unless they carry its ownership marker, so a mistaken path such as
the user's home directory is not cleaned as a build cache.

First call emucap MCP `bootstrap` and use its `listening_port` plus
`runtime_paths`. If the content path is known, call `launch_plan(content_path,
system="pc98")`; it returns preferred MCP `launch` tool args plus legacy fallback
argv for the current port. Prefer `launch_plan.preferred_launcher` over searching
the filesystem; `runtime_paths` also includes the repo root, build path, wrapper
paths, token file, and legacy command templates. Calling `status` first is still
valid, but `bootstrap` is the intended first tool when an agent does not know
what to launch yet. If content or system is unknown, ask the question returned
by `bootstrap.question_to_user_if_content_unknown`; do not infer a launch
command from `status` command templates alone.

If an agent cannot see `bootstrap`/`launch_plan`, or `status` does not include
`runtime_paths`, it is talking to an older `emucap-mcp` release.  Rebuild
`target/release/emucap-mcp` and reconnect the MCP server before attempting a
PC-98 launch.  The path-collision regression is fixed on
`main`; a current launcher ignores stale `work/mame` directories and only uses
`work/mame` when it is an executable regular file.

```sh
MAME_ROMPATH=/path/to/mame-roms \
adapters/mame-pc98/launch.sh "/path/to/system.hdm" <listening_port> pc98-session pc9821
```

The MAME romset is user-supplied — see "What the user must provide" above.  MAME
resolves each machine's romset by driver name, under the rompath in a folder
named for the machine:

```text
<rompath>/<mame-driver-name>/     # e.g. <rompath>/pc9801rs/ for the pc9801rs machine
```

The default rompath is an existing `~/mame/roms` or `%USERPROFILE%/mame/roms`
(override with `MAME_ROMPATH`).  You can point MAME at an existing local set
with a symlink named for the driver:

```text
<rompath>/pc9801rs -> <your-local-romset>
```

The launcher uses `~/mame/roms` or `%USERPROFILE%/mame/roms` as the default
rompath when either directory exists.  Otherwise it creates an emucap-owned
empty rompath under the per-OS data root.  It also writes MAME runtime state
under that emucap data root by default so probe runs do not create `cfg/`,
`nvram/`, or `snap/` in the repo.  Fallback pidfiles and logs also live in the
same per-port emucap directory unless `EMUCAP_LOG` overrides the log path.
It ignores the user's global `mame.ini` by default and forces
`SDL_VIDEODRIVER=dummy`, `-video none`, `-window`, and `-nomaximize` in
headless mode; this prevents saved fullscreen/window settings from opening a
visible MAME window during agent-run probes.  Visible mode is blocked unless it
is explicit: use both `MAME_HEADLESS=0` and `MAME_ALLOW_VISIBLE=1` only when a
window is intentional.  The launcher still requests windowed, non-maximized mode
for visible launches.

Do not run raw MAME directly for PC-98 probes.  If a direct diagnostic command
is needed, use `adapters/mame-pc98/work/mame` or
`EMUCAP_MAME_RAW_BIN=/path/to/mame adapters/mame-pc98/mame-headless.sh ...`.
Those paths append the headless options after caller arguments, so even saved
fullscreen options in a local MAME configuration cannot steal focus.  Running
`work/mame.raw` or a system `mame` directly is only allowed with
`MAME_ALLOW_VISIBLE=1` when a visible window is intentional.

The current local `pc9801rs` set is enough for a headless boot when the default
`pc9801_26` sound card is disabled.  `launch.sh` now applies that default for
`pc9801rs` when `MAME_CBUS0` is not set, so the normal command is:

```sh
adapters/mame-pc98/launch.sh "/path/to/system.hdm" <listening_port> pc98-session pc9801rs
```

Set `MAME_CBUS0=<slot>` explicitly only when you have the matching sound-card
ROMs and want to override that headless default.

Optional:

```sh
MAME_FLOP2="/path/to/sampling.hdm" \
MAME_GDB_PORT=3264 \
MAME_HEADLESS=1 \
MAME_HOME=/path/to/custom-mame-home \
EMUCAP_LOG=/path/to/custom.log \
adapters/mame-pc98/launch.sh "/path/to/system.hdm" <listening_port>
```

Current launch-time floppy mounting is static.  `pc9801rs` exposes `flop1` and
`flop2`; titles that require a boot/game disk plus more data/demo disks still
need either a title-specific repack workaround or MCP-level disk swap/mount
control that has not been implemented yet.

For HDI media, MCP connection only proves that MAME and the bridge reached a
PC-98 machine.  If the HDD controller ROMs or boot path are missing, the machine
can connect and still land at N88-BASIC instead of DOS/game code; confirm with a
screenshot, TVRAM, or game-visible memory before treating an HDI smoke as a game
boot.

The launcher starts:

1. MAME with `-debug -debugger none` and repo-local Lua plugin
   `emucap_gdbstub`.
2. `emucap-gdb-bridge.py`, which connects to MAME's GDB port and then to
   emucap's TCP listener.

It refuses to launch if the emucap listener is missing or if the port already
has an established emulator/bridge connection after cleaning its own pidfiles.

## Supported methods

- `status`
- `read_memory`
- `write_memory`
- `find_pattern`
- `dump_memory`
- `screenshot`
- `get_rom_info`
- `save_state`
- `load_state`
- `probe`
- `set_input`
- `press_buttons`
- `get_state`
- `pause`
- `resume`
- `step`
- `step_instructions`
- `run_frames`
- `disassemble`
- `set_breakpoint`
- `watch_register`
- `clear_breakpoint`
- `list_breakpoints`
- `clear_all_breakpoints`
- `set_trace`
- `get_trace`
- `call_stack`
- `reset`
- `break_on_reset`
- `poll_events`

Memory types:

| memory_type | Base |
| --- | ---: |
| `physical`, `cpu`, `ram` | `0x00000` |
| `tvram` | `0xA0000` |
| `gvram_b` | `0xA8000` |
| `gvram_r` | `0xB0000` |
| `gvram_g` | `0xB8000` |
| `gvram_i` | `0xE0000` |

`dump_memory` writes `ram.bin`, `tvram.bin`, `gvram_b.bin`, `gvram_r.bin`,
`gvram_g.bin`, `gvram_i.bin`, `regions.json`, and the MCP-side `state.json`.
The result can be used with `emucap diff`.

`screenshot` uses the MAME Lua screen snapshot API and works in the default
headless `-video none` launcher mode.

`get_rom_info` hashes the disk image passed to `launch.sh` through
`EMUCAP_CONTENT` and returns `name`, absolute `path`, `sha1`, `size`, and
`media_type`.  For PC-98 this identifies the mounted HDI/HDM/D88 image rather
than a cartridge ROM; regression ROM checks use the same `sha1` field.

`save_state`/`load_state` use an emucap-specific zip bundle with format
`emucap-mame-pc98-state-v2`, not MAME's native `.sta` format.  The bundle stores
the i386 register packet, RAM, TVRAM, GVRAM regions, and MAME save-manager items
exposed through Lua; legacy `emucap-mame-pc98-state-v1` bundles remain readable.
Loading restores save-manager items first, writes the memory regions back, and
restores registers through the Lua bridge's `regload` command.  This is useful
for memory-surface inspection and includes the registered MAME device items.
After `regload`, the Lua plugin keeps servicing the GDB socket while the
debugger is stopped, so MCP `read_memory` and `get_state` observe the restored
instruction slot before it executes.  This is deterministic replay at the MCP
surface, but it is still not a native C++ MAME machine-state load.
`load_state` returns `restore_strategy`, `post_restore_instruction_exact`, and
the observed post-load `observed_pc`/`observed_eip`/`observed_cs` fields.  The
adapter reports `state_restore.deterministic_replay=true`,
`state_restore.hidden_device_state=true`, and
`state_restore.post_restore_instruction_exact=true` from `hello`, `status`,
`save_state`, and `load_state`, while also reporting
`native_atomic_machine_state_load=false`.  Tools that edit a state bundle's RAM
or CPU register packet must discard the stale `saveitems/` payload unless they
can update those MAME-internal items consistently.

`probe` is supported for PC-98 state bundles.  It stops MAME, restores any
bundled save-manager items, writes the bundle's memory regions, applies the
register packet inside one Lua bridge command, then advances frames and reads
the target memory before returning to MCP.  This closes the old load/read
network gap for bisect-style memory predicates, but it does not make
`load_state` use MAME's native `.sta` machinery.

Use `scripts/make_atomic_restore_sled.py` as the promotion gate before marking
PC-98 state restore as deterministic.  The workflow is:

```sh
# 1. In MCP, save a PC-98 state bundle while frozen:
#    save_state("/tmp/pc98_base.state.zip")
adapters/mame-pc98/scripts/make_atomic_restore_sled.py \
  /tmp/pc98_base.state.zip /tmp/pc98_atomic_restore_sled.state.zip
# 2. In MCP, load the generated state and immediately read RAM 0x9000:
#    load_state("/tmp/pc98_atomic_restore_sled.state.zip")
#    read_memory("ram", 0x9000, 1)
```

An exact post-load restore returns `00` and leaves EIP at `0x8000` until the
caller explicitly steps or resumes; `step_instructions(1)` then advances the
counter and EIP.  The Lua/GDB bridge passes this gate: `load_state` reports
`post_restore_instruction_exact=true` and `restore_strategy=lua_register_load_hold`.
A future C++ hook can replace this Lua/GDB hold with a native MAME machine-state
load, but it is not required for MCP-level exact post-load observation or
savestate-based regression.

For regression, savestate cases use PC-98 `probe` for memory predicates, and
`input_replay` cases may start from either `reset` or a deterministic
`load_state` bundle.  Reset-start input replay still avoids state-bundle
identity concerns: MCP replays frozen keyboard input with `set_input` plus frame
`step`, clears transient breakpoints, and reads the predicate target.

`set_input` and `press_buttons` use MAME I/O port field overrides for the
PC-98 keyboard.  Canonical key names include `enter`, `esc`, `space`, directions,
`backspace`, `tab`, `del`, `ins`, `home`, `help`, `stop`, `copy`, `shift`,
`ctrl`, `f1`..`f10`, `vf1`..`vf5`, letters `a`..`z`, and digits `0`..`9`.
Aliases include `start`/`return` -> `enter`, `escape` -> `esc`, and `select` ->
`space`.  `press_buttons(["enter"], frames=8)` was verified to advance the
N88-BASIC boot prompt, and `press_buttons(["l"], frames=6)` typed `l` at the
prompt.  `tap(["enter"], press_frames=2, after_frames=10)` was verified to
advance from the boot prompt while returning to frozen state, and
`tap_sequence([["l"], ["i"], ["s"], ["t"]], press_frames=2)` typed `list`.

`status`, `step(frames=N)`, and `run_frames(N)` include the MAME screen frame
counter as `frame` when a screen is available.  `step(frames=N)` is a
deterministic frame-step implemented by the Lua plugin: it runs MAME until the
screen frame counter reaches the target, then returns to debugger stop.
`step_instructions(count)` and `step(unit="instructions")` keep the old GDB
single-instruction path for CPU-level narrowing.  `run_frames(N)` waits for N
frames and leaves the emulator running.  With frame-step available, MCP
`tap`/`tap_sequence` can drive PC-98 keyboard input from a frozen state without
relying on host keyboard focus.

`set_breakpoint` supports `exec`, `read`, `write`, and `access` (read+write).  The Lua plugin uses MAME
debugger console commands (`bpset`, `wp`, `bpclear`, `wpclear`) rather than the
Lua `cpu.debug:bpset()` API, because direct `bpset()` crashes the tested
`pc9801rs` build.  For read/write watchpoints, `memory_type` offsets are mapped
through the PC-98 memory table, so `memory_type="tvram", start=0` watches
physical `0xA0000`.  `breakpoint_hit` events include the emucap breakpoint id,
the hit address, hit-time i386 registers, and any requested `snapshot`
captures.  `pc_min`/`pc_max` and `value`/`value_mask`/`value_len` filters are
compiled by the bridge into MAME debugger condition expressions on the
repo-local `bpset`/`wp` command path; direct `cpu.debug:bpset()` is still not
used.  `watch_register` defaults to `pause_on_hit=true`, but `set_breakpoint` defaults
to `pause_on_hit=false` (the MCP path always forwards the flag, so an unset
breakpoint is a tracepoint — pass `pause_on_hit=true` to freeze on hit).  With
`pause_on_hit=true` a hit re-asserts debugger stop and holds the frozen socket so
the emulator stays frozen for inspection; with `pause_on_hit=false` the hit is
only queued through `poll_events` and the emulator keeps running.
`list_breakpoints` includes the generated condition string for audit.

`disassemble(address, count)` uses MAME debugger `dasm` through the same
repo-local Lua bridge and returns i386sx instruction rows with `addr`, `text`,
and `bytes`.  This was verified both at the live `cpu.pc` and immediately after
an exec breakpoint hit.

`watch_register` uses MAME debugger registerpoints (`rpset`/`rpclear`) through
the same repo-local console-command path.  The bridge maps emucap register names
such as `pc`, `sp`, `cpu.eip`, and `cpu.esp` to MAME debugger expressions and
returns `register_break` events with the emucap id, register name, bounds,
hit-time i386 registers, `pc`, and the offending value.

`set_trace`/`get_trace` use MAME debugger `trace`/`traceflush` with an
adapter-owned temporary trace file.  `get_trace` parses recent trace rows into
`pc`, `text`, optional `bytes`, and `raw`.  `call_stack` is a best-effort i386
call/return stack reconstructed from that trace log; it is useful for local
hunting, but less authoritative than Mesen's callback-maintained SNES call
stack.

`break_on_reset` arms the repo-local Lua plugin's MAME machine reset notifier.
When a reset occurs while armed, the plugin forces debugger stop and stores a
hit-time reset record.  `poll_events` drains that record through the bridge and
returns a `reset` event with hit-time i386 registers and `pc`.

## Not supported yet

- native C++ MAME machine-state load parity

## BIOS note

Do not commit BIOS files.  For MAME, the required names and checksums are
available from the repo-local binary after `build.sh`, or from the bootstrap
fallback if no local build exists:

```sh
adapters/mame-pc98/work/mame -listroms pc9801rs
adapters/mame-pc98/work/mame -listroms pc9821
adapters/mame-pc98/work/mame -listroms pc9821ap2
```

The existing RetroArch PC-98 BIOS directory in this workstation was useful for
comparison, but did not satisfy MAME's PC-98 romset requirements.

A local PC-9801RS BIOS set matches MAME's `pc9801rs` driver
better than `pc9821`.  Expose it through a `pc9801rs` romset directory or symlink
under the rompath; MAME resolves romsets by driver name, not by nearby folder
labels.

`archtaurus/RetroPieBIOS` has the same kind of PC-98 payload: a
RetroArch/NP2Kai-style set with names such as `bios.rom`, `font.rom`,
`itf.rom`, `sound.rom`, and `ym2608.zip`.  That is a good reference for an
NP2Kai/libretro PoC, but it is not enough for MAME `pc9821` or `pc9821ap2`,
which require additional MAME romset files such as the `24256c-x*.bin` board
ROMs and other driver-specific dumps.
