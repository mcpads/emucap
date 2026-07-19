# emucap — Dolphin (GameCube / Wii) adapter

The Dolphin adapter adds live PowerPC debugging for GameCube and Wii. The supported path is a
repo-owned native fork: a small emucap service runs inside Dolphin and connects directly to the
Control MCP listener over NDJSON.

`status.methods` and `status.memory_types` are authoritative for every live session.

## Native adapter

The native adapter keeps Dolphin's normal JIT for free-running execution. It temporarily switches
to the interpreter only while servicing instruction-step requests, so
`step(unit="instructions")` remains instruction-exact without making normal execution slow.

The patch stack adds:

- native service startup and shutdown hooks;
- GameCube controller override support;
- exact PowerPC exec-breakpoint events;
- PowerPC disassembly and ABI stack walking;
- bounded current-frame screenshot capture;
- synchronous savestate capture and restore;
- build-system entries for the native service.

The upstream revision and patchset digest are pinned in `upstream.lock`. The launcher accepts only a
binary whose `emucap-dolphin-build.json` sidecar matches that lock.

## Build

On macOS or Linux:

```sh
adapters/dolphin/build.sh
```

The script checks out the pinned Dolphin revision under `adapters/dolphin/work`, applies the patch
stack, and builds:

- `dolphin-emu-nogui` for the default headless path;
- `DolphinQt` for `display=true`, when the Qt dependencies are available.

The headless target is required. The GUI target is best-effort and may be skipped with
`EMUCAP_DOLPHIN_BUILD_GUI=0`.

On Windows, `build.ps1` applies the same pinned patch stack with Visual Studio 2022 and writes the
same metadata sidecar expected by the native launcher. This source path is kept in sync, but its
runtime behavior has not been verified in this repository's current macOS test environment.

## Launch

Use the MCP launcher:

```text
launch(content_path="<game.iso|game.gcm|game.rvz|game.wbfs>", system="gamecube")
launch(content_path="<game.wbfs|game.iso|game.rvz>", system="wii")
```

GameCube and Wii share several container extensions. Pass `system` explicitly when media inference
cannot distinguish them.

Headless mode is the default. `display=true` selects the compatible DolphinQt build and opens its
render window. Both modes run from an emucap-owned portable copy with a per-port `--user`
directory. The fork also routes DolphinQt state through that directory, and the launcher rejects
redirecting symlinks inside the portable tree, leaving an installed Dolphin and its profile
untouched. Audio output is disabled.

Follow the normal connection sequence:

1. Call `bootstrap`.
2. Call `launch_plan` with the content path and system.
3. Call `status` immediately before `launch`.
4. After launch returns, call `status` again and use only the reported methods and memory types.

## Tool surface

The native adapter currently advertises:

- `read_memory`, `write_memory`;
- `get_state`, `status`;
- `pause`, `resume`, frame- and instruction-unit `step`;
- `set_breakpoint`, `clear_breakpoint`, `clear_all_breakpoints`, `list_breakpoints`, `poll_events`;
- `disassemble`, `call_stack`;
- frozen core only: `save_state`, `load_state`;
- running core only: `screenshot`;
- GameCube only: `set_input`.

It does not currently advertise read/write watchpoints, tracing, or Wii input injection. These
methods must not be inferred from dormant handler code.

The adapter publishes its feature-contract declaration. The Control MCP validates the declared
exact exec breakpoint, GameCube port 0 input, frozen savestate, and running screenshot limits
before admitting composite tools.

### Memory and registers

`memory_type="main"` uses absolute PowerPC effective addresses, such as `0x80000000`. `get_state`
returns `pc`, all 32 general-purpose registers, `lr`, `ctr`, `xer`, `msr`, and `cr`.

### Execution

`pause` synchronously reaches a frozen CPU boundary. Instruction stepping starts from that frozen
state and returns frozen. Frame stepping waits for Dolphin to present a non-duplicate frame, drains
the GPU queue at the next emulated field boundary, and returns only after the CPU is frozen again.
An emucap breakpoint hit interrupts the operation and leaves the core frozen. Both units accept at
most 15 steps per request so the synchronous operation remains inside the control-link deadline.
Split longer advances into checked calls.

### Breakpoints

Only exact-address exec breakpoints are supported. On a hit, Dolphin freezes before the matching
instruction and `poll_events` returns the adapter breakpoint ID together with the exact address,
PC, and PowerPC register snapshot. Adding or removing a breakpoint clears the relevant JIT cache
state so an already compiled block cannot bypass it.

### Disassembly and call stacks

`disassemble(address, count)` uses Dolphin's Gekko decoder and returns fixed-width PowerPC rows as
`{addr, bytes, text}`. Addresses are absolute effective addresses and must be four-byte aligned.

`call_stack()` uses Dolphin's ABI stack-chain walker and returns frames from outermost to innermost.
Optimized code, a damaged stack, or a title that does not maintain the expected chain can produce a
partial or invalid result, so the live contract reports `debug.call-stack="best_effort"`.

### GameCube input

GameCube controller port 0 accepts lowercase `a`, `b`, `x`, `y`, `z`, `l`, `r`, `start`, `up`,
`down`, `left`, and `right`. `set_input([])` releases the override and returns control to Dolphin's
native input path. `status.input_override` reports whether the adapter currently owns the pad.
Other ports, malformed axes, and unknown buttons fail before changing the active override.

Wii input is not advertised.

### Savestates

`save_state` and `load_state` require a frozen core and preserve that state on return. Save captures
one CPU/device snapshot, completes compression to a same-directory staging file, validates the
result, and publishes it only after all work is complete. Load rejects missing files before
mutation and acknowledges success only after Dolphin has restored the complete snapshot.

### Screenshots

`screenshot` captures the next frame presented after the request and returns a PNG with dimensions,
launch generation, and `freshness="current"` provenance. It is bounded to two seconds. A frozen
core is rejected before a capture is armed; the adapter never resumes guest execution implicitly.
