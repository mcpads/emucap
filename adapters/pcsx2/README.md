# emucap — PCSX2 (PlayStation 2) adapter

The PlayStation 2 adapter uses a pinned PCSX2 fork and a separate Rust bridge. The fork extends
PCSX2's local PINE protocol with bounded, terminally acknowledged control operations; the bridge
translates those operations to the emucap protocol.

`status.methods`, `status.memory_types`, and `status.contracts` are authoritative for a live
session. Stock PCSX2 is rejected because its PINE surface does not provide the host-control
operations this adapter requires.

## Build

```sh
adapters/pcsx2/build.sh
```

The script checks out the revisions pinned by `upstream.lock`, applies the patch stack, builds
PCSX2 and `emucap-pcsx2-bridge`, and writes an `emucap-pcsx2-build.json` sidecar. The separate
PCSX2 patches repository is pinned by commit and tree hash; its `patches` tree is packed with a
fixed timestamp and verified before it is included as `patches.zip`. Launch accepts only a binary
whose upstream commits, host API, resource archive, and patch digest match the lock.

On macOS, PCSX2 recommends an x86_64 build on Apple Silicon. The build therefore uses PCSX2's pinned
x86_64 dependency set and runs through Rosetta. Xcode's Metal Toolchain is required; if `xcrun
--find metal` fails, install it with:

```sh
xcodebuild -downloadComponent MetalToolchain
```

The Ninja build does not apply Xcode target-signing attributes by itself. The build script
ad-hoc-signs the local app with PCSX2's JIT entitlements and verifies the result. Without that step,
the process starts but fails while mapping EE and recompiler memory.

The current Windows and Linux source paths share the Rust launcher and bridge, but have not received
the same live runtime verification as the macOS x86_64 build.

## BIOS and media

PCSX2 needs a BIOS dumped from a console owned by the operator. Set its absolute path before
starting the Control MCP:

```sh
export EMUCAP_PCSX2_BIOS=/absolute/path/to/your/ps2-bios.bin
```

The launcher accepts a 4–8 MiB regular file and references it in place. It does not search for,
copy, bundle, or commit BIOS files. Game images are also operator-supplied and remain outside this
repository.

## Launch and isolation

Use the MCP launcher:

```text
launch(content_path="/absolute/path/to/game.iso", system="ps2")
```

An ISO9660 `SYSTEM.CNF` containing a `BOOT2` entry can identify PS2 media automatically, but passing
`system="ps2"` is appropriate when the container is ambiguous.

Each port gets an emucap-owned PCSX2 data root, settings, memory cards, logs, cache, and private PINE
endpoint. The configured BIOS is referenced from its original location. The session UI language is
fixed to English so a fresh headless profile cannot open a missing-font download dialog, and audio
is muted. The operator's normal PCSX2 profile is neither read nor modified.

There is no legacy shell launcher for this adapter. `bootstrap` and `launch_plan` report the Rust
MCP `launch` tool as the only supported launch path.

Follow the normal sequence:

1. Call `bootstrap`.
2. Call `launch_plan` with the image and `system="ps2"`.
3. Call `status` immediately before `launch`.
4. After `launch` returns, call `status` again and use only the reported surface.

## Tool surface

The adapter advertises:

- `get_rom_info`, `status`;
- `read_memory`, `write_memory`, `find_pattern`, `dump_memory`;
- `get_state`;
- `pause`, `resume`, and frame-unit `step`;
- `disassemble`;
- frozen-only `save_state` and `load_state`;
- `screenshot`;
- `set_input` and `press_buttons`;
- EE execution, read, and write breakpoints with event polling;
- frozen-state best-effort call stacks;
- synchronous reset.

The MCP server also exposes `tap` and `hold_until` when their dependencies are present. The adapter
does not advertise tracing, register watches, reset breakpoints, instruction stepping, or
`run_frames`. These methods must not be inferred from PCSX2's GUI or other debugger facilities.

### Memory and state

`memory_type="ee"` is the 32 MiB Emotion Engine RAM range, addressed from zero. Reads and writes
that cross `0x02000000` are rejected before reaching PINE. One request is bounded by the limits
reported in `status.contracts`.

`find_pattern` scans this same zero-based range and returns region-relative addresses.
`dump_memory` writes `ee.bin` and `regions.json`; the MCP server adds `state.json`. Both operations
freeze once for a coherent read when the guest was running and restore the original running state
before returning. Large dumps are streamed in bounded chunks and committed by rename.

`get_state` returns the EE PC, the low 64 bits of all 32 general-purpose registers, and HI/LO.
`disassemble(address, count)` takes a four-byte-aligned EE absolute address and uses PCSX2's EE
decoder.

### Breakpoints and call stacks

`set_breakpoint` supports exact EE execution addresses and read/write ranges in `memory_type="ee"`.
Read/write ranges are inclusive and limited to 64 KiB. PCSX2's native debugger always pauses on a
hit, so `pause_on_hit=false`, value or PC conditions, automatic savestates, and memory snapshots are
rejected before a breakpoint is armed.

`poll_events` drains a bounded native queue. Each hit includes its kind, accessing PC, exact access
address and width, and the low 64-bit EE register snapshot captured at the hit. The queue retains up
to 256 hits and reports how many older entries were dropped. Breakpoints created through this
adapter are tracked separately from PCSX2 GUI breakpoints; clearing or disconnecting the bridge
removes only the adapter-owned entries.

`call_stack` requires frozen state and uses PCSX2's MIPS stack walker. Frames are returned from
outermost to innermost with `pc`, estimated function `entry`, `sp`, and `stack_size`. Optimized code,
damaged stacks, and incomplete symbols can produce partial results, so the response marks this
surface as best-effort.

### Screenshot and input

`screenshot` captures the current GS output as PNG. A running guest is paused inside the native
request, captured synchronously, and resumed before the response; a frozen guest remains frozen.
The response binds the image to a stable emulator frame and the current launch generation.

Digital input uses DualShock 2 controller port 0. `set_input(buttons)` holds one replacement mask
until another `set_input` call, and `set_input([])` returns control to native input.
`press_buttons(buttons, frames)` applies the whole combination in one emulator-frame window,
accepts 1–240 frames, and releases the override before its terminal response. If the guest was
frozen, only the requested frames advance and the guest returns to frozen. If it was running, it
remains running. Native button changes observed during an override are restored when the override
ends.

### Execution and savestates

`pause` returns only after PCSX2 reaches a frozen state. That state persists until an explicit
resume, frame step, state load, or process exit; host wall-clock time does not advance the guest.

`step(count, unit="frames")` requires frozen state, accepts 1–15 frames, and returns frozen after
the requested frames complete. Instruction-unit stepping is not supported. Split longer movement
into calls and wait for each terminal response before issuing the next dependent request.

`save_state` and `load_state` require frozen state and preserve it on return. Their paths must be
absolute. A running-state call fails before starting a save or load.

`reset` pauses first, performs PCSX2's reset synchronously, releases any controller override, clears
stale hit events, and returns frozen with `post_reset_pc`. Armed adapter breakpoints remain available
for the fresh boot; call `resume` when inspection or breakpoint setup is complete.

MCP requests may execute concurrently, so dependent operations are not ordered merely because their
JSON-RPC messages were sent in one batch. For sequences such as write → readback or load → inspect,
wait for the previous terminal response before sending the next request.

`get_rom_info` reads the running title's serial, version, CRC, and title through PINE. Hashing a
large disc image happens outside the request path; the response reports whether the SHA-1 is
pending, ready, unavailable, or failed.

## License boundary

The Rust bridge and launcher are separate processes licensed under the repository's
GPL-2.0-or-later terms. The PCSX2 patch stack modifies GPL-3.0-or-later source and is distributed
under GPL-3.0-or-later. This repository distributes source patches and a build recipe, not PCSX2
binaries, BIOS files, or game media.
