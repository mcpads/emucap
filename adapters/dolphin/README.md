# emucap — Dolphin (GameCube / Wii) adapter

The Dolphin adapter adds live PowerPC debugging for GameCube and Wii. The supported path is a
repo-owned native fork: a small emucap service runs inside Dolphin and connects directly to the
Control MCP listener over NDJSON. A legacy GDB-stub bridge remains available for manual use.

`status.methods` and `status.memory_types` are authoritative for every live session.

## Native adapter

The native adapter keeps Dolphin's normal JIT for free-running execution. It temporarily switches
to the interpreter only while servicing instruction-step requests, so
`step(unit="instructions")` remains instruction-exact without making normal execution slow.

The patch stack adds:

- native service startup and shutdown hooks;
- GameCube controller override support;
- exact PowerPC exec-breakpoint events;
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

`build.ps1` is a legacy Windows build helper. It is not yet part of the metadata-checked native
launcher path; Windows native launch therefore remains unverified.

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
directory, leaving an installed Dolphin and its profile untouched. Audio output is disabled.

Follow the normal connection sequence:

1. Call `bootstrap`.
2. Call `launch_plan` with the content path and system.
3. Call `status` immediately before `launch`.
4. After launch returns, call `status` again and use only the reported methods and memory types.

## Tool surface

The native adapter currently advertises:

- `read_memory`, `write_memory`;
- `get_state`, `status`;
- `pause`, `resume`, instruction-unit `step`;
- `set_breakpoint`, `clear_breakpoint`, `list_breakpoints`, `poll_events`;
- running core only: `screenshot`;
- GameCube only: `set_input`.

It does not currently advertise savestates, frame stepping, read/write watchpoints, tracing, call
stacks, or Wii input injection. These methods must not be inferred from dormant handler code.

The adapter does not yet publish a feature-contract declaration, so the Control MCP reports its
contract state as `unreported`. Its atomic methods remain available, but contract-gated composite
tools are not admitted.

### Memory and registers

`memory_type="main"` uses absolute PowerPC effective addresses, such as `0x80000000`. `get_state`
returns `pc`, all 32 general-purpose registers, `lr`, `ctr`, `xer`, `msr`, and `cr`.

### Execution

`pause` synchronously reaches a frozen CPU boundary. Instruction stepping starts from that frozen
state and returns frozen. Frame-unit stepping is unsupported.

### Breakpoints

Only exact-address exec breakpoints are supported. On a hit, Dolphin freezes before the matching
instruction and `poll_events` returns the adapter breakpoint ID together with the exact address and
PC. Adding or removing a breakpoint clears the relevant JIT cache state so an already compiled
block cannot bypass it.

### GameCube input

GameCube controller port 0 accepts lowercase `a`, `b`, `x`, `y`, `z`, `l`, `r`, `start`, `up`,
`down`, `left`, and `right`. `set_input([])` releases the override and returns control to Dolphin's
native input path. Other ports and unknown buttons fail before changing the active override.

Wii input is not advertised.

### Screenshots

`screenshot` captures the next frame presented after the request and returns a PNG with dimensions,
launch generation, and `freshness="current"` provenance. It is bounded to two seconds. A frozen
core is rejected before a capture is armed; the adapter never resumes guest execution implicitly.

## Legacy GDB-stub bridge

`emucap-gdb-bridge.py` can relay Dolphin's built-in PowerPC GDB stub to the emucap wire protocol.
This path does not require the native fork, but it forces a non-JIT CPU core and exposes a smaller,
less precise surface. It is not integrated with the preferred cross-platform MCP launcher.

The PowerShell launch scripts are retained for this manual Windows workflow. They must be given the
current listener port; do not assume a fixed port, and do not attach another GDB client because
Dolphin's stub accepts a single persistent debugger connection.
